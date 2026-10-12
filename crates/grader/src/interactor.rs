//! Interactive problems: two isolate runs joined by two pipes (spec §2 "Interactor").
//!
//! The solution runs in its box and the interactor, which is problem-author code, in the helper box.
//! The daemon only creates the pipes and hands one end of each to a child as its stdio; it never reads or
//! writes them. The parent's copies are closed as soon as both children are spawned, so a side that exits
//! gives the other EOF. A pair that blocks on each other is ended by the sandbox wall-time limits; a host
//! deadline on top of that kills both isolate processes if a limit does not fire.

use crate::checkers::CheckOutcome;
use crate::checkers::custom::{
    ExitReport, PartialPoints, PartialScale, checker_limits, finished_code, interpret,
};
use crate::checkers::select::CustomKind;
use crate::error::GradeError;
use crate::languages::{Placeholders, expand};
use crate::program::Program;
use crate::run_case::{Runnable, STDERR_FILE, keeps_stderr, refine, solution_limits};
use judge_sandbox::IsolateBox;
use judge_sandbox::classify::{OutputUse, RunVerdict, classify};
use judge_sandbox::error::SandboxError;
use judge_sandbox::spec::{Limits, RunSpec, Stdio};
use std::path::Path;
use std::process::{Child, Stdio as ProcessStdio};
use std::time::{Duration, Instant};

/// stdout of the solution is a pipe, not a file, so this only sizes `--fsize` for its other files.
const SOLUTION_FILE_CAP: u64 = 64 << 20;
const INTERACTOR_STDERR: &str = ".stderr";
const MAX_INTERACTOR_STDERR: u64 = 64 << 10;
/// Host-side margin past the interactor's own wall limit before the isolate processes are killed.
const DEADLINE_SLACK: Duration = Duration::from_secs(5);
/// isolate's own exit status for an internal error (1 is "the program failed").
const ISOLATE_FAILURE: i32 = 2;
const POLL: Duration = Duration::from_millis(2);
const NO_OUTPUT: OutputUse = OutputUse { written: 0, cap: u64::MAX };
/// isolate resets signal dispositions inside the box, so the interactor is started through a shell that
/// ignores SIGPIPE and execs it: a last write to a solution that already exited fails with EPIPE, and the
/// interactor still reaches its own verdict instead of dying.
const IGNORE_SIGPIPE: [&str; 3] = ["/bin/sh", "-c", "trap '' PIPE; exec \"$0\" \"$@\""];

pub struct Boxes<'a> {
    pub solution: &'a mut IsolateBox,
    pub helper: &'a mut IsolateBox,
}

pub struct Interaction<'a> {
    pub solution: &'a Runnable<'a>,
    pub interactor: &'a Program,
    pub input: &'a Path,
    pub answer: &'a Path,
    /// What the case is worth: testlib's `points N` is N points of it, as for checkers.
    pub points: f64,
}

#[derive(Debug)]
pub struct Interactive {
    pub verdict: RunVerdict,
    pub time_s: f64,
    pub memory_kib: u64,
    /// `None` when the solution's own verdict wins.
    pub check: Option<CheckOutcome>,
}

/// Wall = solution wall + 2 s, CPU = max(10 s, 2 x TL), 1 GiB, one process.
pub fn interactor_limits(solution: &Limits) -> Limits {
    let mut limits = checker_limits(solution.time_s);
    limits.wall_s = solution.wall_s + 2.0;
    limits
}

const SIGPIPE_REASON: &str = "SIGPIPE";
/// testlib's exit code for `_fail` (a jury error).
const TESTLIB_FAIL_EXIT: i32 = 3;
/// testlib's exit codes for `_wa` and `_points`: verdicts formed from what the interactor read. Its other
/// non-zero codes (2 is also testlib's unexpected EOF, 4, 8) can be the symptom of a solution that died.
const TESTLIB_JUDGED_EXITS: [i32; 2] = [1, 7];

/// A solution that failed on its own wins over the interactor, with two exceptions where the failure is the
/// consequence of the interactor having finished first: SIGPIPE (it wrote to the closed pipe), and an
/// exception or non-zero exit (EOF, `BrokenPipeError`) after the interactor quit with WA or partial points.
/// A crash by any other signal stays the solution's own.
pub fn solution_wins(solution: &RunVerdict, interactor: &RunVerdict) -> bool {
    let interactor_judged =
        matches!(interactor, RunVerdict::Ir { exit_code } if TESTLIB_JUDGED_EXITS.contains(exit_code));
    match solution {
        RunVerdict::Ok => false,
        RunVerdict::Rte { reason } if reason == SIGPIPE_REASON => false,
        RunVerdict::Rte { reason } if reason.starts_with("SIG") => true,
        RunVerdict::Rte { .. } | RunVerdict::Ir { .. } => !interactor_judged,
        _ => true,
    }
}

fn solution_spec(x: &Interaction) -> RunSpec {
    let (program, limits) = (x.solution.program, x.solution.limits);
    let stderr = if keeps_stderr(program) { STDERR_FILE } else { "/dev/null" };
    RunSpec {
        argv: expand(&program.run, &Placeholders { memory_kib: limits.memory_kib, class: None }),
        limits: solution_limits(x.solution, SOLUTION_FILE_CAP),
        stdio: Stdio { stdin: None, stdout: None, stderr: Some(stderr.into()) },
        env: program.env.clone(),
        dirs: program.dirs.clone(),
    }
}

fn interactor_spec(x: &Interaction, solution: &Limits) -> RunSpec {
    let mut argv: Vec<String> = IGNORE_SIGPIPE.map(String::from).into();
    argv.extend(x.interactor.run.iter().cloned());
    argv.extend(["input", "tout", "answer"].map(String::from));
    let mut limits = interactor_limits(solution);
    limits.processes = x.interactor.processes.max(1);
    RunSpec {
        argv,
        limits,
        stdio: Stdio { stdin: None, stdout: None, stderr: Some(INTERACTOR_STDERR.into()) },
        env: x.interactor.env.clone(),
        dirs: x.interactor.dirs.clone(),
    }
}

fn prepare(boxes: &mut Boxes, x: &Interaction) -> Result<(), GradeError> {
    boxes.solution.reset()?;
    boxes.solution.put_tree(&x.solution.program.dir)?;
    boxes.helper.reset()?;
    boxes.helper.put_tree(&x.interactor.dir)?;
    boxes.helper.put("input", x.input, false)?;
    boxes.helper.put("answer", x.answer, false)?;
    Ok(())
}

/// Spawns both isolate processes, joined by two pipes. Every `Command` and pipe end is a temporary that
/// is gone when this returns, so the daemon holds no pipe end and EOF reaches the surviving side.
fn spawn_pair(boxes: &Boxes, interactor: &RunSpec, solution: &RunSpec) -> Result<(Child, Child), GradeError> {
    let (to_interactor_read, to_interactor_write) = std::io::pipe()?;
    let (to_solution_read, to_solution_write) = std::io::pipe()?;
    let mut interactor_child = boxes
        .helper
        .command(interactor)?
        .stdin(ProcessStdio::from(to_interactor_read))
        .stdout(ProcessStdio::from(to_solution_write))
        .stderr(ProcessStdio::null())
        .spawn()?;
    let solution_child = boxes
        .solution
        .command(solution)?
        .stdin(ProcessStdio::from(to_solution_read))
        .stdout(ProcessStdio::from(to_interactor_write))
        .stderr(ProcessStdio::null())
        .spawn();
    match solution_child {
        Ok(child) => Ok((interactor_child, child)),
        Err(error) => {
            let _ = interactor_child.kill();
            let _ = interactor_child.wait();
            Err(error.into())
        }
    }
}

/// Waits for both children; past the deadline both are killed (the next box reset kills what is left
/// inside) and the interaction is an error. isolate exits 2 on its own failures, which is an error too,
/// because the meta file would then be stale.
fn wait_both(first: &mut Child, second: &mut Child, deadline: Instant) -> Result<(), GradeError> {
    let (mut first_status, mut second_status) = (None, None);
    while first_status.is_none() || second_status.is_none() {
        first_status = first_status.or(first.try_wait()?);
        second_status = second_status.or(second.try_wait()?);
        if Instant::now() >= deadline && (first_status.is_none() || second_status.is_none()) {
            let _ = first.kill();
            let _ = second.kill();
            first.wait()?;
            second.wait()?;
            return Err(SandboxError::Isolate("interaction exceeded the host deadline".into()).into());
        }
        std::thread::sleep(POLL);
    }
    for status in [first_status, second_status].into_iter().flatten() {
        if status.code() == Some(ISOLATE_FAILURE) {
            return Err(SandboxError::Isolate(format!("isolate exited with {status}")).into());
        }
    }
    Ok(())
}

pub fn interact(mut boxes: Boxes, x: &Interaction) -> Result<Interactive, GradeError> {
    prepare(&mut boxes, x)?;
    let solution = solution_spec(x);
    let interactor = interactor_spec(x, &solution.limits);
    let (mut interactor_child, mut solution_child) = spawn_pair(&boxes, &interactor, &solution)?;
    let deadline = Instant::now() + Duration::from_secs_f64(interactor.limits.wall_s) + DEADLINE_SLACK;
    wait_both(&mut interactor_child, &mut solution_child, deadline)?;
    decide(&boxes, x, &solution.limits)
}

fn solution_result(boxes: &Boxes, x: &Interaction) -> Result<Interactive, GradeError> {
    let meta = boxes.solution.read_meta()?;
    let verdict = classify(&meta, NO_OUTPUT);
    if let RunVerdict::Internal(message) = &verdict {
        return Err(SandboxError::Isolate(message.clone()).into());
    }
    let verdict = refine(verdict, x.solution.program, boxes.solution);
    Ok(Interactive { verdict, time_s: meta.time_s, memory_kib: meta.cg_mem_kib, check: None })
}

fn interactor_outcome(
    boxes: &Boxes,
    verdict: &RunVerdict,
    exit_code: Option<i32>,
    points: f64,
) -> Result<CheckOutcome, GradeError> {
    let code = finished_code(verdict, exit_code, "interactor")?;
    let stderr = boxes.helper.read_output(INTERACTOR_STDERR, MAX_INTERACTOR_STDERR).unwrap_or_default();
    let report = ExitReport { code, stdout: String::new(), stderr: String::from_utf8_lossy(&stderr).into() };
    let scale = PartialScale { mode: PartialPoints::Absolute, case_points: points };
    interpret(CustomKind::Testlib, &report, scale)
        .map_err(|e| GradeError::Checker(format!("interactor: {}", e.0)))
}

/// The interactor itself failed: testlib's `_fail` (exit 3) or a crash other than a broken pipe. The case is
/// then IE whatever the solution did (a solution that read the dead interactor's EOF is not to blame).
pub fn interactor_failed(verdict: &RunVerdict) -> bool {
    match verdict {
        RunVerdict::Ir { exit_code } => *exit_code == TESTLIB_FAIL_EXIT,
        RunVerdict::Rte { reason } => reason != SIGPIPE_REASON,
        _ => false,
    }
}

/// A solution killed by the wall clock within its CPU limit, while the interactor used more CPU than the
/// wall allows on top of that limit, was waiting for the interactor: jury time, not the contestant's TLE.
/// This also catches an interactor that hangs, which the wall limit ends. Policy: a problem whose
/// interactor needs that much CPU cannot be judged within these limits, so it is IE, not a verdict.
fn stalled_by_interactor(solution: &Interactive, interactor_cpu_s: f64, limits: &Limits) -> bool {
    solution.verdict == RunVerdict::Tle
        && solution.time_s < limits.time_s
        && interactor_cpu_s > limits.wall_s - limits.time_s
}

fn decide(boxes: &Boxes, x: &Interaction, limits: &Limits) -> Result<Interactive, GradeError> {
    let result = solution_result(boxes, x)?;
    let interactor = boxes.helper.read_meta()?;
    if stalled_by_interactor(&result, interactor.time_s, limits) {
        let used = interactor.time_s;
        return Err(GradeError::Checker(format!(
            "interactor used {used:.1} s of CPU; the solution timed out waiting"
        )));
    }
    let interactor_verdict = classify(&interactor, NO_OUTPUT);
    if solution_wins(&result.verdict, &interactor_verdict) && !interactor_failed(&interactor_verdict) {
        return Ok(result);
    }
    let check = interactor_outcome(boxes, &interactor_verdict, interactor.exit_code, x.points)?;
    Ok(Interactive { check: Some(check), ..result })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rte(reason: &str) -> RunVerdict {
        RunVerdict::Rte { reason: reason.into() }
    }

    #[test]
    fn the_interactor_fails_on_fail_and_crashes() {
        let table = [
            (RunVerdict::Ok, false),
            (RunVerdict::Ir { exit_code: 1 }, false), // testlib WA
            (RunVerdict::Ir { exit_code: 3 }, true),  // testlib FAIL
            (rte("SIGSEGV"), true),
            (rte("SIGPIPE"), false),  // the solution hung up
            (RunVerdict::Tle, false), // a hang is caught by `stalled_by_interactor`
        ];
        for (verdict, failed) in table {
            assert_eq!(interactor_failed(&verdict), failed, "{verdict:?}");
        }
    }

    #[test]
    fn the_solutions_own_failure_wins_unless_it_followed_the_interactors_verdict() {
        // The interactor's testlib exit code: 0 ok, 1 WA, 2 PE (also unexpected EOF), 7 points.
        let (ok, wa, eof, points) = (0, 1, 2, 7);
        let interactor = |code| if code == 0 { RunVerdict::Ok } else { RunVerdict::Ir { exit_code: code } };
        // (solution, interactor, the solution's verdict stands)
        let table = [
            (RunVerdict::Ok, wa, false),
            (rte("SIGPIPE"), ok, false),
            (rte("SIGSEGV"), eof, true), // it crashed and the interactor read EOF
            (RunVerdict::Ir { exit_code: 1 }, eof, true),
            (rte("SIGSEGV"), ok, true),
            (rte("SIGSEGV"), wa, true), // a real crash, whatever the interactor made of the EOF
            (rte("EOFError"), wa, false), // the interactor quit with WA first
            (rte("BrokenPipeError"), points, false),
            (RunVerdict::Ir { exit_code: 1 }, wa, false),
            (RunVerdict::Tle, wa, true),
            (RunVerdict::Mle, wa, true),
        ];
        for (solution, code, wins) in table {
            assert_eq!(solution_wins(&solution, &interactor(code)), wins, "{solution:?} vs exit {code}");
        }
    }

    #[test]
    fn a_wall_timeout_is_the_interactors_when_it_used_the_waiting_time() {
        let limits = Limits::for_solution(1.0, 1 << 18, 1, 1 << 20); // 1 s CPU, 4 s wall
        let tle = |time_s| Interactive { verdict: RunVerdict::Tle, time_s, memory_kib: 0, check: None };
        assert!(stalled_by_interactor(&tle(0.01), 5.0, &limits), "idle solution, busy interactor");
        assert!(!stalled_by_interactor(&tle(0.01), 0.01, &limits), "both idle: a deadlock is the solution's");
        assert!(!stalled_by_interactor(&tle(1.5), 5.0, &limits), "it used up its own CPU limit");
        let ok = Interactive { verdict: RunVerdict::Ok, ..tle(0.01) };
        assert!(!stalled_by_interactor(&ok, 5.0, &limits));
    }

    fn spawn(program: &str, args: &[&str]) -> Child {
        std::process::Command::new(program).args(args).spawn().unwrap()
    }

    #[test]
    fn a_child_that_outlives_the_deadline_is_killed_and_reported() {
        let (mut a, mut b) = (spawn("sleep", &["30"]), spawn("true", &[]));
        let started = Instant::now();
        let result = wait_both(&mut a, &mut b, Instant::now() + Duration::from_millis(100));
        assert!(matches!(result, Err(GradeError::Sandbox(_))));
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(a.try_wait().unwrap().is_some());
    }

    #[test]
    fn finished_children_do_not_wait_for_the_deadline() {
        let (mut a, mut b) = (spawn("true", &[]), spawn("false", &[]));
        let started = Instant::now();
        wait_both(&mut a, &mut b, Instant::now() + Duration::from_secs(60)).unwrap();
        assert!(started.elapsed() < Duration::from_secs(5), "exit 1 is a failed program, not a failure");
    }

    #[test]
    fn an_isolate_internal_error_is_an_error() {
        let (mut a, mut b) = (spawn("true", &[]), spawn("sh", &["-c", "exit 2"]));
        let result = wait_both(&mut a, &mut b, Instant::now() + Duration::from_secs(60));
        assert!(matches!(result, Err(GradeError::Sandbox(_))));
    }
}
