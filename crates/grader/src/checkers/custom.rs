//! `bridged` checkers: problem-author code compiled and run inside the helper box (spec §2 "Custom").
//! It never runs on the host; it sees only copies of the case input, the output and the answer.

use super::identical::PRESENTATION_ERROR;
use super::select::CustomKind;
use super::{CheckError, CheckInput, CheckOutcome};
use crate::error::GradeError;
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
    pub time_limit_s: f64,
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

/// testlib: 0 AC, 1 WA, 2 and 4 presentation error, 8 unexpected EOF (WA), 7 partial points, 3 fail (IE).
/// default: 0 AC, 1 WA, 2 partial points on stdout, anything else IE. No exit code (signal, limit) is IE.
pub fn interpret(kind: CustomKind, report: &ExitReport) -> Result<CheckOutcome, CheckError> {
    let testlib = kind == CustomKind::Testlib;
    let fail =
        |what: &str| CheckError(format!("checker {what}: {}", clip_text(&report.stderr, MAX_EXTENDED)));
    let partial = |text: &str| -> Result<CheckOutcome, CheckError> {
        let fraction = first_number(text).ok_or_else(|| fail("gave partial points without a score"))?;
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
        let mut argv = self.program.run.clone();
        argv.extend(["input", "output", "answer"].map(String::from));
        let mut limits = checker_limits(self.time_limit_s);
        limits.processes = self.program.processes.max(1);
        RunSpec {
            argv,
            limits,
            stdio: Stdio {
                stdin: Some("/dev/null".into()),
                stdout: Some(STDOUT.into()),
                stderr: Some(STDERR.into()),
            },
            env: self.program.env.clone(),
            dirs: self.program.dirs.clone(),
        }
    }

    /// Runs in the worker's helper box. The three paths are daemon-owned files (a scratch copy or a
    /// `/proc/self/fd/N` link of a checked handle); they are copied in, never mounted.
    pub fn check(&self, sandbox: &mut IsolateBox, io: &CheckInput) -> Result<CheckOutcome, GradeError> {
        sandbox.reset()?;
        sandbox.put_tree(&self.program.dir)?;
        sandbox.put("input", io.input, false)?;
        sandbox.put("output", io.output, false)?;
        sandbox.put("answer", io.answer, false)?;
        let meta = sandbox.run(&self.spec())?;
        let verdict = classify(&meta, OutputUse { written: 0, cap: u64::MAX });
        let code = finished_code(&verdict, meta.exit_code, "checker")?;
        let report =
            ExitReport { code, stdout: read_text(sandbox, STDOUT), stderr: read_text(sandbox, STDERR) };
        Ok(interpret(self.kind, &report)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(code: i32, stdout: &str, stderr: &str) -> ExitReport {
        ExitReport { code: Some(code), stdout: stdout.into(), stderr: stderr.into() }
    }

    fn testlib(code: i32, stderr: &str) -> Result<CheckOutcome, CheckError> {
        interpret(CustomKind::Testlib, &report(code, "", stderr))
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
    fn a_signal_or_limit_has_no_exit_code_and_is_an_error() {
        let killed = ExitReport { code: None, stdout: String::new(), stderr: String::new() };
        assert!(interpret(CustomKind::Testlib, &killed).is_err());
        assert!(interpret(CustomKind::Default, &killed).is_err());
    }

    #[test]
    fn default_exit_codes_read_partial_points_from_stdout() {
        let default = |code, stdout: &str| interpret(CustomKind::Default, &report(code, stdout, ""));
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
        let r = interpret(CustomKind::Default, &report(1, "", &long)).unwrap();
        assert_eq!((r.feedback.len(), r.extended.len()), (50, 512));
        assert_eq!(clip_text(&"é".repeat(30), 5), "éé");
    }

    #[test]
    fn helper_limits_follow_the_spec() {
        let slow = checker_limits(7.0);
        assert_eq!((checker_limits(2.0).time_s, slow.time_s, slow.wall_s), (10.0, 14.0, 28.0));
        assert_eq!((slow.memory_kib, slow.processes), (1 << 20, 1));
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
}
