//! `bridged` checkers: problem-author code compiled and run inside the helper box (spec §2 "Custom").
//! It never runs on the host; it sees only copies of the case input, the output and the answer.

use super::identical::PRESENTATION_ERROR;
use super::select::CustomKind;
use super::themis::{self, ThemisNames};
use super::{CheckError, CheckInput, CheckOutcome};
use crate::error::GradeError;
use crate::languages::{Placeholders, expand};
use crate::program::Program;
use judge_sandbox::IsolateBox;
use judge_sandbox::classify::{OutputUse, RunVerdict, classify};
use judge_sandbox::spec::{Limits, RunSpec, Stdio};

pub const MAX_FEEDBACK: usize = 50;
pub const MAX_EXTENDED: usize = 512;
/// How much of the checker's stdout and stderr is read back.
const MAX_REPORT_BYTES: u64 = 64 << 10;
const STDOUT: &str = ".stdout";
const STDERR: &str = ".stderr";

pub struct CustomChecker {
    pub program: Program,
    pub kind: CustomKind,
    pub partial: PartialPoints,
    /// Where a themis checker looks for the case files; empty for the other kinds.
    pub themis_names: ThemisNames,
    pub time_limit_s: f64,
}

/// What testlib's `points N` means (the old judge's reading, which migrated checkers were written for):
/// N points of the case, or N percent of the case's points with `treat_checker_points_as_percentage`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PartialPoints {
    #[default]
    Absolute,
    Percentage,
}

/// A partial-points reading for one case: its mode and the points the case is worth.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PartialScale {
    pub mode: PartialPoints,
    pub case_points: f64,
}

impl PartialScale {
    /// The share of the case that `points` earns, clamped to 0..=1. A case worth nothing (a pretest in a full
    /// run) earns its whole (zero) value.
    pub fn fraction(&self, points: f64) -> f64 {
        let fraction = match self.mode {
            PartialPoints::Percentage => points / 100.0,
            PartialPoints::Absolute if self.case_points > 0.0 => points / self.case_points,
            PartialPoints::Absolute => 1.0,
        };
        fraction.clamp(0.0, 1.0)
    }
}

#[derive(Debug, Clone)]
pub struct ExitReport {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

pub fn clip_text(text: &str, max: usize) -> String {
    let mut end = text.len().min(max);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_string()
}

/// Time max(10 s, 2 x TL), 1 GiB, one process (spec §5).
pub fn checker_limits(solution_time_limit_s: f64) -> Limits {
    let time = (2.0 * solution_time_limit_s).max(10.0);
    Limits {
        time_s: time,
        wall_s: 2.0 * time,
        extra_time_s: 0.0,
        memory_kib: 1 << 20,
        processes: 1,
        fsize_kib: 64 << 10,
        open_files: 64,
    }
}

/// Box memory a JVM helper keeps outside its heap (metaspace, thread stacks, code cache).
const HELPER_JVM_RESERVE_KIB: u64 = 128 << 10;

/// A helper's run argv with its placeholders filled in: a JVM's `-Xmx` is the box's memory minus a reserve.
pub(crate) fn helper_argv(program: &Program, limits: &Limits) -> Vec<String> {
    let reserve = if program.jvm { HELPER_JVM_RESERVE_KIB } else { 0 };
    let memory_kib = limits.memory_kib.saturating_sub(reserve);
    expand(&program.run, &Placeholders { memory_kib, class: None })
}

/// testlib starts its stderr message with the verdict ("ok ", "wrong answer ", "points 0.5 ", ...).
const TESTLIB_PREFIXES: [&str; 6] =
    ["ok ", "wrong answer ", "wrong output format ", "unexpected eof ", "FAIL ", "partially correct "];

fn strip_testlib_prefix(line: &str) -> &str {
    let line = line.trim();
    if let Some(rest) = line.strip_prefix("points ") {
        return rest.split_once(char::is_whitespace).map_or("", |(_, message)| message.trim_start());
    }
    TESTLIB_PREFIXES.iter().find_map(|prefix| line.strip_prefix(prefix)).unwrap_or(line)
}

fn first_number(text: &str) -> Option<f64> {
    let token = text.split_whitespace().find(|t| *t != "points")?;
    token.parse::<f64>().ok().filter(|n| n.is_finite())
}

fn outcome(passed: bool, fraction: f64, report: &ExitReport, testlib: bool) -> CheckOutcome {
    let first_line = report.stderr.lines().next().unwrap_or("");
    let message = if testlib { strip_testlib_prefix(first_line) } else { first_line.trim() };
    CheckOutcome {
        passed,
        fraction: fraction.clamp(0.0, 1.0),
        feedback: clip_text(message, MAX_FEEDBACK),
        extended: clip_text(report.stderr.trim(), MAX_EXTENDED),
    }
}

/// testlib: 0 AC, 1 WA, 2 and 4 presentation error, 8 unexpected EOF (WA), 7 partial points read through
/// `scale`, 3 fail (IE). default: 0 AC, 1 WA, 2 a 0..1 share on stdout, anything else IE. No exit code
/// (signal, limit) is IE.
pub fn interpret(
    kind: CustomKind,
    report: &ExitReport,
    scale: PartialScale,
) -> Result<CheckOutcome, CheckError> {
    let testlib = kind == CustomKind::Testlib;
    let fail =
        |what: &str| CheckError(format!("checker {what}: {}", clip_text(&report.stderr, MAX_EXTENDED)));
    let partial = |text: &str| -> Result<CheckOutcome, CheckError> {
        let score = first_number(text).ok_or_else(|| fail("gave partial points without a score"))?;
        let fraction = if testlib { scale.fraction(score) } else { score };
        Ok(outcome(true, fraction, report, testlib))
    };
    match (kind, report.code) {
        (_, Some(0)) => Ok(outcome(true, 1.0, report, testlib)),
        (_, Some(1)) | (CustomKind::Testlib, Some(8)) => Ok(outcome(false, 0.0, report, testlib)),
        (CustomKind::Testlib, Some(2 | 4)) => {
            let mut result = outcome(false, 0.0, report, true);
            result.feedback = PRESENTATION_ERROR.to_string();
            Ok(result)
        }
        (CustomKind::Testlib, Some(7)) => partial(&report.stderr),
        (CustomKind::Default, Some(2)) => partial(&report.stdout),
        (_, Some(code)) => Err(fail(&format!("failed with exit code {code}"))),
        (_, None) => Err(fail("crashed")),
    }
}

/// The exit code of a helper that ended by itself (exit 0 or a non-zero exit), else why it did not.
pub fn finished_code(
    verdict: &RunVerdict,
    exit_code: Option<i32>,
    who: &str,
) -> Result<Option<i32>, GradeError> {
    match verdict {
        RunVerdict::Ok | RunVerdict::Ir { .. } => Ok(exit_code),
        RunVerdict::Internal(message) => Err(judge_sandbox::SandboxError::Isolate(message.clone()).into()),
        RunVerdict::Tle => Err(GradeError::Checker(format!("{who} exceeded its time limit"))),
        RunVerdict::Mle => Err(GradeError::Checker(format!("{who} exceeded its memory limit"))),
        RunVerdict::Ole => Err(GradeError::Checker(format!("{who} wrote too much"))),
        RunVerdict::Rte { reason } => Err(GradeError::Checker(format!("{who} crashed ({reason})"))),
    }
}

fn read_text(sandbox: &IsolateBox, name: &str) -> String {
    let bytes = sandbox.read_output(name, MAX_REPORT_BYTES).unwrap_or_default();
    String::from_utf8_lossy(&bytes).into_owned()
}

impl CustomChecker {
    fn spec(&self) -> RunSpec {
        let mut limits = checker_limits(self.time_limit_s);
        limits.processes = self.program.processes.max(1);
        let mut argv = helper_argv(&self.program, &limits);
        let mut stdin = "/dev/null".to_string();
        if self.kind == CustomKind::Themis {
            stdin = themis::PATHS_FILE.into();
        } else {
            argv.extend(["input", "output", "answer"].map(String::from));
        }
        RunSpec {
            argv,
            limits,
            stdio: Stdio { stdin: Some(stdin), stdout: Some(STDOUT.into()), stderr: Some(STDERR.into()) },
            env: self.program.env.clone(),
            dirs: self.program.dirs.clone(),
        }
    }

    /// Runs in the worker's helper box. The three paths are daemon-owned files (a scratch copy or a
    /// `/proc/self/fd/N` link of a checked handle); they are copied in, never mounted.
    pub fn check(&self, sandbox: &mut IsolateBox, io: &CheckInput) -> Result<CheckOutcome, GradeError> {
        sandbox.reset()?;
        sandbox.put_tree(&self.program.dir)?;
        if self.kind == CustomKind::Themis {
            themis::stage(sandbox.dir(), &self.themis_names, (io.input, io.output, io.answer))?;
            sandbox.put_bytes(themis::PATHS_FILE, themis::paths_text().as_bytes())?;
        } else {
            sandbox.put("input", io.input, false)?;
            sandbox.put("output", io.output, false)?;
            sandbox.put("answer", io.answer, false)?;
        }
        let meta = sandbox.run(&self.spec())?;
        let verdict = classify(&meta, OutputUse { written: 0, cap: u64::MAX });
        let code = finished_code(&verdict, meta.exit_code, "checker")?;
        if self.kind == CustomKind::Themis {
            return Ok(themis::interpret(code, &read_text(sandbox, STDOUT))?);
        }
        let report =
            ExitReport { code, stdout: read_text(sandbox, STDOUT), stderr: read_text(sandbox, STDERR) };
        let scale = PartialScale { mode: self.partial, case_points: io.points };
        Ok(interpret(self.kind, &report, scale)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::checkers::testing::heap_kib;

    fn report(code: i32, stdout: &str, stderr: &str) -> ExitReport {
        ExitReport { code: Some(code), stdout: stdout.into(), stderr: stderr.into() }
    }

    const ONE_POINT: PartialScale = PartialScale { mode: PartialPoints::Absolute, case_points: 1.0 };

    fn testlib(code: i32, stderr: &str) -> Result<CheckOutcome, CheckError> {
        interpret(CustomKind::Testlib, &report(code, "", stderr), ONE_POINT)
    }

    fn testlib_on(mode: PartialPoints, case_points: f64, stderr: &str) -> CheckOutcome {
        interpret(CustomKind::Testlib, &report(7, "", stderr), PartialScale { mode, case_points }).unwrap()
    }

    #[test]
    fn testlib_exit_codes() {
        let ok = testlib(0, "ok 3 numbers").unwrap();
        assert!(ok.passed && ok.fraction == 1.0);
        assert_eq!(ok.feedback, "3 numbers");
        let wa = testlib(1, "wrong answer expected 5, found 9").unwrap();
        assert!(!wa.passed);
        assert_eq!(wa.feedback, "expected 5, found 9");
        assert!(testlib(3, "FAIL bad answer file").is_err());
        assert!(testlib(0, "").unwrap().passed, "an empty message is fine");
    }

    #[test]
    fn presentation_errors_are_marked_and_unexpected_eof_is_wrong_answer() {
        for code in [2, 4] {
            let pe = testlib(code, "wrong output format extra tokens").unwrap();
            assert!(!pe.passed);
            assert_eq!(pe.feedback, PRESENTATION_ERROR);
        }
        let eof = testlib(8, "unexpected eof Unexpected end of file").unwrap();
        assert!(!eof.passed);
        assert_ne!(eof.feedback, PRESENTATION_ERROR);
    }

    #[test]
    fn testlib_partial_points() {
        let partial = testlib(7, "points 0.25 partial").unwrap();
        assert!(partial.passed);
        assert!((partial.fraction - 0.25).abs() < 1e-9);
        assert_eq!(partial.feedback, "partial");
        assert_eq!(testlib(7, "points 1.5 over").unwrap().fraction, 1.0);
        assert_eq!(testlib(7, "points -3 under").unwrap().fraction, 0.0);
        assert!(testlib(7, "no number").is_err());
        assert!(testlib(7, "points NaN x").is_err());
    }

    #[test]
    fn testlib_points_are_points_of_the_case_by_default() {
        let partial = testlib_on(PartialPoints::Absolute, 5.0, "points 3 three of five");
        assert!(partial.passed && (partial.fraction - 0.6).abs() < 1e-9, "{partial:?}");
        assert_eq!(testlib_on(PartialPoints::Absolute, 5.0, "points 9 too many").fraction, 1.0);
        assert_eq!(testlib_on(PartialPoints::Absolute, 5.0, "points -2 negative").fraction, 0.0);
        let pretest = testlib_on(PartialPoints::Absolute, 0.0, "points 3 worth nothing");
        assert!(pretest.passed, "a zero-point case still passes: {pretest:?}");
    }

    #[test]
    fn testlib_points_are_a_percentage_with_the_flag() {
        let half = testlib_on(PartialPoints::Percentage, 40.0, "points 50 half");
        assert!((half.fraction - 0.5).abs() < 1e-9, "{half:?}");
        assert_eq!(testlib_on(PartialPoints::Percentage, 40.0, "points 150 over").fraction, 1.0);
        assert_eq!(testlib_on(PartialPoints::Percentage, 40.0, "points -1 under").fraction, 0.0);
    }

    #[test]
    fn an_accepted_case_gets_full_points_whatever_the_mode() {
        let report = report(0, "", "ok fine");
        for mode in [PartialPoints::Absolute, PartialPoints::Percentage] {
            let ok =
                interpret(CustomKind::Testlib, &report, PartialScale { mode, case_points: 7.0 }).unwrap();
            assert!(ok.passed && ok.fraction == 1.0);
        }
    }

    #[test]
    fn a_signal_or_limit_has_no_exit_code_and_is_an_error() {
        let killed = ExitReport { code: None, stdout: String::new(), stderr: String::new() };
        assert!(interpret(CustomKind::Testlib, &killed, ONE_POINT).is_err());
        assert!(interpret(CustomKind::Default, &killed, ONE_POINT).is_err());
    }

    #[test]
    fn default_exit_codes_read_partial_points_from_stdout() {
        let default =
            |code, stdout: &str| interpret(CustomKind::Default, &report(code, stdout, ""), ONE_POINT);
        assert!(default(0, "").unwrap().passed);
        assert!(!default(1, "").unwrap().passed);
        assert!((default(2, "0.5\n").unwrap().fraction - 0.5).abs() < 1e-9);
        assert!(default(2, "").is_err());
        assert!(default(5, "").is_err());
        assert!(default(8, "").is_err(), "testlib's EOF code means nothing to a default checker");
    }

    #[test]
    fn feedback_is_clipped_on_a_char_boundary() {
        let long = "x".repeat(600);
        let r = interpret(CustomKind::Default, &report(1, "", &long), ONE_POINT).unwrap();
        assert_eq!((r.feedback.len(), r.extended.len()), (50, 512));
        assert_eq!(clip_text(&"é".repeat(30), 5), "éé");
    }

    #[test]
    fn only_a_helper_that_finished_by_itself_has_a_verdict() {
        let code = |v: RunVerdict| finished_code(&v, Some(3), "checker");
        assert_eq!(code(RunVerdict::Ok), Ok(Some(3)));
        assert_eq!(code(RunVerdict::Ir { exit_code: 3 }), Ok(Some(3)));
        for stopped in
            [RunVerdict::Tle, RunVerdict::Mle, RunVerdict::Ole, RunVerdict::Rte { reason: "SIGSEGV".into() }]
        {
            assert!(matches!(code(stopped), Err(GradeError::Checker(_))));
        }
    }

    #[test]
    fn a_java_checker_gets_a_heap_below_its_box_memory() {
        let registry =
            crate::languages::LanguageRegistry::from_toml(include_str!("../../../../languages.toml"))
                .unwrap();
        let java = registry.get("JAVA8").unwrap();
        let program =
            crate::compile::program_for(java, std::path::Path::new("/c"), b"public class Checker {}");
        let checker = CustomChecker {
            program,
            kind: CustomKind::Default,
            partial: PartialPoints::Absolute,
            themis_names: ThemisNames::default(),
            time_limit_s: 1.0,
        };
        let spec = checker.spec();
        assert!(spec.argv.iter().all(|a| !a.contains('{')), "unexpanded placeholder: {:?}", spec.argv);
        let heap = heap_kib(&spec.argv);
        assert!(
            heap >= 512 << 10 && heap < spec.limits.memory_kib,
            "heap {heap} KiB in a {} KiB box",
            spec.limits.memory_kib
        );
        assert_eq!(spec.argv[spec.argv.len() - 3..], ["input", "output", "answer"]);
    }
}
