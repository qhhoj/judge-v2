//! `bridged` checkers: problem-author code compiled and run inside the helper box (docs/grading.md#checkers).
//! It never runs on the host; it sees only copies of the case input, the output and the answer.

use super::report_window::{End, REPORT_WINDOW, read_report};
use super::select::CustomKind;
use super::testlib::{self, Ended};
use super::themis::{self, ThemisNames};
use super::{CheckError, CheckInput, CheckOutcome};
use crate::error::GradeError;
use crate::languages::{Placeholders, expand};
use crate::program::Program;
use judge_sandbox::classify::{OutputUse, RunVerdict, classify};
use judge_sandbox::spec::{Limits, RunSpec, Stdio};
use judge_sandbox::{IsolateBox, Room};

pub const MAX_FEEDBACK: usize = 50;
pub const MAX_EXTENDED: usize = 512;
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

    /// The share `points` earns, or `None` when they are outside the case: below 0, or over its points
    /// (100 with the percentage flag). A case worth nothing takes any points of 0 or more.
    pub fn fraction_in_range(&self, points: f64) -> Option<f64> {
        let most = match self.mode {
            PartialPoints::Percentage => 100.0,
            PartialPoints::Absolute if self.case_points > 0.0 => self.case_points,
            PartialPoints::Absolute => f64::INFINITY,
        };
        (points >= 0.0 && points <= most * (1.0 + 1e-9)).then(|| self.fraction(points))
    }
}

/// What a `default` checker reports: its exit code, its share first on stdout, its message on stderr.
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

/// Time max(10 s, 2 x TL), 1 GiB, one process (docs/grading.md#checkers).
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

fn first_number(text: &str) -> Option<f64> {
    text.split_whitespace().next()?.parse::<f64>().ok().filter(|n| n.is_finite())
}

/// `default`: 0 AC, 1 WA, 2 a 0..1 share first on stdout (0 or less is a wrong answer), anything else or no
/// exit code (signal, limit) IE. Its first stderr line is the feedback.
pub fn interpret(report: &ExitReport) -> Result<CheckOutcome, CheckError> {
    let fail =
        |what: &str| CheckError(format!("checker {what}: {}", clip_text(&report.stderr, MAX_EXTENDED)));
    let graded = |passed: bool, fraction: f64| CheckOutcome {
        passed,
        fraction: fraction.clamp(0.0, 1.0),
        feedback: clip_text(report.stderr.lines().next().unwrap_or("").trim(), MAX_FEEDBACK),
        extended: clip_text(report.stderr.trim(), MAX_EXTENDED),
        jury_time_s: 0.0,
    };
    match report.code {
        Some(0) => Ok(graded(true, 1.0)),
        Some(1) => Ok(graded(false, 0.0)),
        Some(2) => {
            let share =
                first_number(&report.stdout).ok_or_else(|| fail("gave partial points without a score"))?;
            Ok(graded(share > 0.0, share))
        }
        Some(code) => Err(fail(&format!("failed with exit code {code}"))),
        None => Err(fail("crashed")),
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

/// The case files of one check and where a themis checker finds them.
struct Layout<'a> {
    io: &'a CheckInput<'a>,
    themis_names: &'a ThemisNames,
}

/// One finished checker run: its exit code and CPU seconds.
struct Run {
    code: Option<i32>,
    time_s: f64,
}

impl CustomChecker {
    fn spec(&self) -> RunSpec {
        let mut limits = checker_limits(self.time_limit_s);
        limits.processes = self.program.processes.max(1);
        let mut argv = helper_argv(&self.program, &limits);
        let mut stdin = "/dev/null".to_string();
        match self.kind {
            CustomKind::Themis => stdin = themis::PATHS_FILE.into(),
            CustomKind::Default => argv.extend(["input", "output", "answer"].map(String::from)),
            CustomKind::Testlib => {
                argv.extend(["input", "output", "answer"].map(String::from));
                argv.push(testlib::RESULT_FILE.into());
            }
        }
        RunSpec {
            argv,
            limits,
            stdio: Stdio { stdin: Some(stdin), stdout: Some(STDOUT.into()), stderr: Some(STDERR.into()) },
            env: self.program.env.clone(),
            dirs: self.program.dirs.clone(),
            inherit_fds: false,
        }
    }

    /// Runs in the worker's helper box. The three paths are daemon-owned files (a scratch copy or a
    /// `/proc/self/fd/N` link of a checked handle); they are copied in, never mounted.
    pub fn check(&self, sandbox: &mut IsolateBox, io: &CheckInput) -> Result<CheckOutcome, GradeError> {
        self.check_laid_out(sandbox, &Layout { io, themis_names: &self.themis_names })
    }

    /// `check` for a case of the problem: a themis checker also finds the case files under the case's own
    /// input and answer file names (`case_files`, paths in the problem data).
    pub fn check_case(
        &self,
        sandbox: &mut IsolateBox,
        io: &CheckInput,
        case_files: (&str, &str),
    ) -> Result<CheckOutcome, GradeError> {
        let names = match self.kind {
            CustomKind::Themis => self.themis_names.with_case(case_files.0, case_files.1),
            _ => ThemisNames::default(),
        };
        self.check_laid_out(sandbox, &Layout { io, themis_names: &names })
    }

    fn check_laid_out(&self, sandbox: &mut IsolateBox, layout: &Layout) -> Result<CheckOutcome, GradeError> {
        if self.kind == CustomKind::Testlib {
            return self.check_testlib(sandbox, layout);
        }
        let run = self.run(sandbox, layout)?;
        let outcome = if self.kind == CustomKind::Themis {
            // A themis checker prints its score last on stdout.
            themis::interpret(run.code, &read_report(sandbox, STDOUT, End::Tail).text)?
        } else {
            let stdout = read_report(sandbox, STDOUT, End::Head).text;
            let stderr = read_report(sandbox, STDERR, End::Head).text;
            interpret(&ExitReport { code: run.code, stdout, stderr })?
        };
        Ok(CheckOutcome { jury_time_s: run.time_s, ..outcome })
    }

    /// A testlib checker: its exit code and the quit message testlib wrote to the result file.
    fn check_testlib(&self, sandbox: &mut IsolateBox, layout: &Layout) -> Result<CheckOutcome, GradeError> {
        let scale = PartialScale { mode: self.partial, case_points: layout.io.points };
        let run = self.run(sandbox, layout)?;
        let message = testlib::read_result(sandbox)?;
        let stderr = read_report(sandbox, STDERR, End::Tail);
        let outcome =
            testlib::interpret(&Ended { code: run.code, stderr: &stderr, message: &message }, scale)?;
        Ok(CheckOutcome { jury_time_s: run.time_s, ..outcome })
    }

    /// Stages a fresh box and runs the checker in it.
    fn run(&self, sandbox: &mut IsolateBox, layout: &Layout) -> Result<Run, GradeError> {
        self.stage(sandbox, layout)?;
        let spec = self.spec();
        let meta = sandbox.run(&spec)?;
        let verdict = classify(&meta, OutputUse::UNMEASURED, &spec.limits);
        Ok(Run { code: finished_code(&verdict, meta.exit_code, "checker")?, time_s: meta.time_s })
    }

    fn stage(&self, sandbox: &mut IsolateBox, layout: &Layout) -> Result<(), GradeError> {
        let io = layout.io;
        // A themis layout puts each case file in once and links its other names, so it takes the same room.
        let room = Room::for_files(&[&self.program.dir, io.input, io.output, io.answer])?;
        sandbox.reset_with_room(room.plus_file(REPORT_WINDOW))?;
        sandbox.put_tree(&self.program.dir)?;
        match self.kind {
            CustomKind::Themis => stage_themis(sandbox, layout),
            CustomKind::Default => put_case(sandbox, io),
            CustomKind::Testlib => {
                put_case(sandbox, io)?;
                testlib::create_result_file(sandbox)
            }
        }
    }
}

fn put_case(sandbox: &IsolateBox, io: &CheckInput) -> Result<(), GradeError> {
    sandbox.put("input", io.input, false)?;
    sandbox.put("output", io.output, false)?;
    sandbox.put("answer", io.answer, false)?;
    Ok(())
}

fn stage_themis(sandbox: &IsolateBox, layout: &Layout) -> Result<(), GradeError> {
    if layout.themis_names.is_empty() {
        return Err(GradeError::Checker(
            "a themis checker needs names for the case files: the problem's file I/O names, \
             `.inp`/`.out`/`.ans` names in its source, or plain case file names"
                .into(),
        ));
    }
    let io = layout.io;
    themis::stage(sandbox.dir(), layout.themis_names, (io.input, io.output, io.answer))?;
    sandbox.put_bytes(themis::PATHS_FILE, themis::paths_text().as_bytes())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::checkers::testing::heap_kib;

    fn report(code: Option<i32>, stdout: &str, stderr: &str) -> ExitReport {
        ExitReport { code, stdout: stdout.into(), stderr: stderr.into() }
    }

    #[test]
    fn points_outside_the_case_have_no_share() {
        let five = PartialScale { mode: PartialPoints::Absolute, case_points: 5.0 };
        assert_eq!(five.fraction_in_range(3.0), Some(0.6));
        assert_eq!(five.fraction_in_range(5.0), Some(1.0));
        assert_eq!(five.fraction_in_range(0.0), Some(0.0));
        assert_eq!((five.fraction_in_range(5.5), five.fraction_in_range(-1.0)), (None, None));
        let percent = PartialScale { mode: PartialPoints::Percentage, case_points: 40.0 };
        assert_eq!((percent.fraction_in_range(50.0), percent.fraction_in_range(101.0)), (Some(0.5), None));
        let pretest = PartialScale { mode: PartialPoints::Absolute, case_points: 0.0 };
        assert_eq!(pretest.fraction_in_range(3.0), Some(1.0), "a case worth nothing takes its whole value");
    }

    #[test]
    fn a_signal_or_limit_has_no_exit_code_and_is_an_error() {
        assert!(interpret(&report(None, "", "")).is_err());
    }

    #[test]
    fn default_exit_codes_read_partial_points_from_stdout() {
        let default = |code, stdout: &str| interpret(&report(Some(code), stdout, ""));
        assert!(default(0, "").unwrap().passed);
        assert!(!default(1, "").unwrap().passed);
        assert!((default(2, "0.5\n").unwrap().fraction - 0.5).abs() < 1e-9);
        let zero = default(2, "0\n").unwrap();
        assert!(!zero.passed && zero.fraction == 0.0, "a 0 share is a wrong answer: {zero:?}");
        assert!(default(2, "").is_err());
        assert!(default(5, "").is_err());
        assert!(default(8, "").is_err(), "testlib's EOF code means nothing to a default checker");
    }

    #[test]
    fn feedback_is_clipped_on_a_char_boundary() {
        let long = "x".repeat(600);
        let r = interpret(&report(Some(1), "", &long)).unwrap();
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
