//! testlib checkers and interactors (docs/grading.md#checkers). Each run gets a result file the judge
//! creates (`<args> .result.txt`): testlib writes its quit message there, apart from anything the program
//! printed, and its verdict is the exit code. Helpers are built with `TESTSYS`, so `_pc(n)` exits 50 + n
//! instead of colliding with the other codes.

use super::custom::{MAX_EXTENDED, MAX_FEEDBACK, PartialScale, clip_text};
use super::identical::PRESENTATION_ERROR;
use super::report_window::{REPORT_WINDOW, Report};
use super::{CheckError, CheckOutcome};
use crate::error::GradeError;
use judge_sandbox::IsolateBox;
use std::io::Read;

/// The result file, created empty by the judge in the freshly staged helper box before each run.
pub const RESULT_FILE: &str = ".result.txt";
/// The define a testlib helper is compiled with: `_pc(n)` exits `PC_BASE` + n with `PC_BASE` 50.
pub const TESTSYS_DEFINE: &str = "-DTESTSYS";
/// testlib's last stderr line when it quits with a result file.
const SEE_RESULT_FILE: &str = "See file to check exit message";
/// `_pc(n)` exits this + n under `TESTSYS`.
const PC_BASE: i32 = 50;
/// `_pc(n)` earns n / 200 of the case (Polygon's 0..=200 scale for `_pc`).
const PARTIALLY_CORRECT_SCALE: u32 = 200;

/// Creates the empty result file in a freshly staged box. A file of that name already there (one of the
/// program's own files) is refused, so no result can be in place before the run.
pub fn create_result_file(sandbox: &IsolateBox) -> Result<(), GradeError> {
    if std::fs::symlink_metadata(sandbox.dir().join(RESULT_FILE)).is_ok() {
        return Err(GradeError::Checker(format!(
            "the program's files hold `{RESULT_FILE}`, the result file"
        )));
    }
    Ok(sandbox.put_writable(RESULT_FILE, b"")?)
}

/// testlib's quit message from the result file after the run (empty when the program wrote none). A file
/// that is gone, unreadable or longer than the window is a checker error.
pub fn read_result(sandbox: &IsolateBox) -> Result<String, CheckError> {
    let lost = || CheckError(format!("the result file `{RESULT_FILE}` could not be read"));
    let file = sandbox.open_output(RESULT_FILE).map_err(|_| lost())?;
    let mut bytes = Vec::new();
    file.take(REPORT_WINDOW + 1).read_to_end(&mut bytes).map_err(|_| lost())?;
    if bytes.len() as u64 > REPORT_WINDOW {
        return Err(CheckError(format!("the result file is over {} KiB", REPORT_WINDOW >> 10)));
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// What a run is graded from: its exit code, its stderr and the message in its result file.
pub struct Ended<'a> {
    pub code: Option<i32>,
    pub stderr: &'a Report,
    pub message: &'a str,
}

impl Ended<'_> {
    fn fail(&self, what: &str) -> CheckError {
        CheckError(format!("checker {what}: {}", clip_text(self.message.trim(), MAX_EXTENDED)))
    }

    /// testlib wrote this run's result: its stderr ends with testlib's own line about the result file.
    fn quit_by_testlib(&self) -> bool {
        self.stderr.text.trim_end().ends_with(SEE_RESULT_FILE)
    }

    fn graded(&self, passed: bool, fraction: f64, message: &str) -> CheckOutcome {
        let message = message.trim();
        let trace = trace(&self.stderr.text);
        let shown = if message.is_empty() { trace } else { message };
        let extended = if trace.is_empty() || trace == shown {
            shown.to_string()
        } else {
            format!("{trace}\n{message}")
        };
        CheckOutcome {
            passed,
            fraction,
            feedback: clip_text(shown.lines().next().unwrap_or(""), MAX_FEEDBACK),
            extended: clip_text(extended.trim(), MAX_EXTENDED),
            jury_time_s: 0.0,
        }
    }
}

/// The verdict is the exit code: 0 AC, 1 WA, 2 and 4 presentation error, 8 unexpected EOF (WA), 3 fail
/// (IE); 7 the points that start the result message; 50 + n `_pc(n)`, n / 200 of the case, when testlib
/// wrote the result. Anything else, points that are not a number or outside the case, or no exit code is
/// IE. Partial points of 0 are a wrong answer, so they stop a batch, and a full run at a pretest.
pub fn interpret(run: &Ended, scale: PartialScale) -> Result<CheckOutcome, CheckError> {
    let code = run.code.ok_or_else(|| run.fail("crashed"))?;
    match code {
        0 => Ok(run.graded(true, 1.0, run.message)),
        1 | 8 => Ok(run.graded(false, 0.0, run.message)),
        2 | 4 => Ok(presentation_error(run.graded(false, 0.0, run.message))),
        7 => points(run, scale),
        n if n >= PC_BASE && run.quit_by_testlib() => partially_correct((n - PC_BASE) as u32, run),
        _ => Err(run.fail(&format!("failed with exit code {code}"))),
    }
}

/// `quitp(p, ...)` and `quitf(_points, "p ...")` both start the result message with the points. A program
/// that leaves the result file empty (not built on testlib) gives them as its last stderr line instead,
/// exactly `points <number>` and newline-terminated.
fn points(run: &Ended, scale: PartialScale) -> Result<CheckOutcome, CheckError> {
    let (first, rest) = match run.message.trim() {
        "" => (last_points_line(&run.stderr.text).unwrap_or(""), ""),
        message => message.split_once(char::is_whitespace).unwrap_or((message, "")),
    };
    let points = first.parse::<f64>().ok().filter(|p| p.is_finite());
    let points = points.ok_or_else(|| run.fail("gave partial points without a number first"))?;
    let fraction =
        scale.fraction_in_range(points).ok_or_else(|| run.fail(&format!("gave {points} points")))?;
    Ok(run.graded(points > 0.0, fraction, rest))
}

/// The number of a last stderr line that is exactly `points <number>`, when stderr ends with a newline.
fn last_points_line(stderr: &str) -> Option<&str> {
    let last = stderr.strip_suffix('\n')?.rsplit('\n').next()?;
    let number = last.strip_suffix('\r').unwrap_or(last).strip_prefix("points ")?;
    (!number.is_empty() && !number.contains(char::is_whitespace)).then_some(number)
}

fn partially_correct(n: u32, run: &Ended) -> Result<CheckOutcome, CheckError> {
    if n > PARTIALLY_CORRECT_SCALE {
        return Err(run.fail(&format!("gave partially correct ({n}), over {PARTIALLY_CORRECT_SCALE}")));
    }
    Ok(run.graded(n > 0, f64::from(n) / f64::from(PARTIALLY_CORRECT_SCALE), run.message))
}

/// What the program printed on stderr, without the lines testlib adds when it has a result file.
fn trace(stderr: &str) -> &str {
    let trimmed = stderr.trim_end();
    trimmed.strip_suffix(SEE_RESULT_FILE).unwrap_or(trimmed).trim()
}

fn presentation_error(mut outcome: CheckOutcome) -> CheckOutcome {
    outcome.feedback = PRESENTATION_ERROR.to_string();
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::checkers::custom::PartialPoints;

    const ONE_POINT: PartialScale = PartialScale { mode: PartialPoints::Absolute, case_points: 1.0 };
    /// What testlib leaves on stderr after the program's own lines when it has a result file.
    const QUIT_BY_TESTLIB: &str = "\nSee file to check exit message\n";

    fn on(
        code: Option<i32>,
        stderr: &str,
        message: &str,
        scale: PartialScale,
    ) -> Result<CheckOutcome, CheckError> {
        let stderr = Report { text: stderr.into(), cut: false };
        interpret(&Ended { code, stderr: &stderr, message }, scale)
    }

    fn quit(code: i32, message: &str) -> Result<CheckOutcome, CheckError> {
        on(Some(code), QUIT_BY_TESTLIB, message, ONE_POINT)
    }

    #[test]
    fn the_exit_code_is_the_verdict() {
        let ok = quit(0, "3 numbers").unwrap();
        assert!(ok.passed && ok.fraction == 1.0 && ok.feedback == "3 numbers", "{ok:?}");
        let wa = quit(1, "expected 5, found 9").unwrap();
        assert!(!wa.passed && wa.fraction == 0.0 && wa.feedback == "expected 5, found 9", "{wa:?}");
        for code in [2, 4] {
            let pe = quit(code, "extra tokens").unwrap();
            assert!(!pe.passed && pe.feedback == PRESENTATION_ERROR, "{pe:?}");
        }
        let eof = quit(8, "Unexpected end of file").unwrap();
        assert!(!eof.passed && eof.feedback != PRESENTATION_ERROR);
    }

    #[test]
    fn a_fail_another_exit_code_or_none_is_a_checker_error() {
        assert!(matches!(quit(3, "bad answer file"), Err(CheckError(m)) if m.contains("exit code 3")));
        for code in [5, 6, 9, 49] {
            assert!(quit(code, "").is_err(), "exit {code}");
        }
        assert!(on(None, "", "", ONE_POINT).is_err(), "a signal or limit has no verdict");
    }

    #[test]
    fn exit_7_points_start_the_result_message() {
        let partial = quit(7, "0.25 partial").unwrap();
        assert!(partial.passed && partial.fraction == 0.25 && partial.feedback == "partial", "{partial:?}");
        let old_style = quit(7, "0.250000 \n partial").unwrap();
        assert!((old_style.fraction - 0.25).abs() < 1e-9, "quitf(_points, \"%f ...\"): {old_style:?}");
        let zero = quit(7, "0").unwrap();
        assert!(!zero.passed && zero.fraction == 0.0, "0 points is a wrong answer: {zero:?}");
        for message in ["graded 0.5", "", "NaN x", "inf", "1,5 x", "1.5 over", "-3 under"] {
            assert!(quit(7, message).is_err(), "{message:?}");
        }
        let five = PartialScale { mode: PartialPoints::Absolute, case_points: 5.0 };
        let three = on(Some(7), QUIT_BY_TESTLIB, "3 of five", five).unwrap();
        assert!((three.fraction - 0.6).abs() < 1e-9, "{three:?}");
        let percent = PartialScale { mode: PartialPoints::Percentage, case_points: 40.0 };
        assert_eq!(on(Some(7), QUIT_BY_TESTLIB, "50 half", percent).unwrap().fraction, 0.5);
        let pretest = PartialScale { mode: PartialPoints::Absolute, case_points: 0.0 };
        assert!(on(Some(7), QUIT_BY_TESTLIB, "3", pretest).unwrap().passed, "a zero-point case passes");
    }

    #[test]
    fn pc_n_exits_50_plus_n_and_earns_n_two_hundredths() {
        let half = quit(150, "half").unwrap();
        assert!(half.passed && half.fraction == 0.5 && half.feedback == "half", "{half:?}");
        let none = quit(50, "nothing").unwrap();
        assert!(!none.passed && none.fraction == 0.0, "_pc(0) earns nothing: {none:?}");
        assert_eq!(quit(250, "all").unwrap().fraction, 1.0);
        assert!(quit(251, "over").is_err(), "_pc(201) is over the scale");
        assert!(on(Some(150), "", "", ONE_POINT).is_err(), "a program that is not testlib exits 150: IE");
    }

    #[test]
    fn nothing_on_stderr_changes_the_verdict() {
        let stderr = format!("points 1 x\nok y\npartially correct (200) z 7 7 7{QUIT_BY_TESTLIB}");
        let points = on(Some(7), &stderr, "0.1 graded", ONE_POINT).unwrap();
        assert!((points.fraction - 0.1).abs() < 1e-9, "{points:?}");
        let pc0 = on(Some(50), &stderr, "nothing", ONE_POINT).unwrap();
        assert!(!pc0.passed && pc0.fraction == 0.0, "{pc0:?}");
        assert!(on(Some(7), "points 1 x\n", "", ONE_POINT).is_err(), "no points in the result: IE");
    }

    #[test]
    fn a_program_without_a_result_gives_exit_7_points_as_its_last_stderr_line() {
        let points = |stderr: &str| on(Some(7), stderr, "", ONE_POINT);
        let hand_written = points("trace\npoints 0.55\n").unwrap();
        assert!(hand_written.passed && (hand_written.fraction - 0.55).abs() < 1e-9, "{hand_written:?}");
        assert!((points("points 0.25\r\n").unwrap().fraction - 0.25).abs() < 1e-9, "CRLF");
        for refused in [
            "points 0.55",
            "points 0.55 x\n",
            "points 0.55\ntrace\n",
            "x points 0.55\n",
            "points \n",
            "points 2\n",
        ] {
            assert!(points(refused).is_err(), "{refused:?}");
        }
        let testlib = on(Some(7), "points 1\n", "0.1 graded", ONE_POINT).unwrap();
        assert!((testlib.fraction - 0.1).abs() < 1e-9, "a result message wins: {testlib:?}");
    }

    #[test]
    fn the_details_hold_the_programs_trace_and_then_the_message() {
        let traced =
            on(Some(1), &format!("n = 4\n12 7 3{QUIT_BY_TESTLIB}"), "expected 5", ONE_POINT).unwrap();
        assert_eq!(
            (traced.feedback.as_str(), traced.extended.as_str()),
            ("expected 5", "n = 4\n12 7 3\nexpected 5")
        );
        let plain = on(Some(1), "Sai o dong 3\n", "", ONE_POINT).unwrap();
        assert_eq!((plain.feedback.as_str(), plain.extended.as_str()), ("Sai o dong 3", "Sai o dong 3"));
    }
}
