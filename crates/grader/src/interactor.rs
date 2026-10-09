//! Interactive problems: two isolate runs joined by two pipes (spec §2 "Interactor").
//!
//! The solution runs in its box and the interactor, which is problem-author code, in the helper box.
//! The daemon only creates the pipes and hands one end of each to a child as its stdio; it never reads or
//! writes them. The parent's copies are closed as soon as both children are spawned, so a side that exits
//! gives the other EOF. A pair that blocks on each other is ended by the sandbox wall-time limits; a host
//! deadline on top of that kills both isolate processes if a limit does not fire.

use crate::checkers::CheckOutcome;
use crate::checkers::custom::{ExitReport, checker_limits, finished_code, interpret};
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

pub struct Boxes<'a> {
    pub solution: &'a mut IsolateBox,
    pub helper: &'a mut IsolateBox,
}

pub struct Interaction<'a> {
    pub solution: &'a Runnable<'a>,
    pub interactor: &'a Program,
    pub input: &'a Path,
    pub answer: &'a Path,
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

/// A solution that failed on its own wins over the interactor. The exception is SIGPIPE: the solution
/// wrote to an interactor that had already finished, so the interactor's verdict is the real one.
pub fn solution_wins(verdict: &RunVerdict) -> bool {
    match verdict {
        RunVerdict::Ok => false,
        RunVerdict::Rte { reason } => reason != SIGPIPE_REASON,
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
    let mut argv = x.interactor.run.clone();
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
    decide(&boxes, x)
}

fn solution_result(boxes: &Boxes, x: &Interaction) -> Result<Interactive, GradeError> {
    let meta = boxes.solution.read_meta()?;
    let verdict = classify(&meta, OutputUse { written: 0, cap: u64::MAX });
    if let RunVerdict::Internal(message) = &verdict {
        return Err(SandboxError::Isolate(message.clone()).into());
    }
    let verdict = refine(verdict, x.solution.program, boxes.solution);
    Ok(Interactive { verdict, time_s: meta.time_s, memory_kib: meta.cg_mem_kib, check: None })
}

fn interactor_outcome(boxes: &Boxes) -> Result<CheckOutcome, GradeError> {
    let meta = boxes.helper.read_meta()?;
    let verdict = classify(&meta, OutputUse { written: 0, cap: u64::MAX });
    let code = finished_code(&verdict, meta.exit_code, "interactor")?;
    let stderr = boxes.helper.read_output(INTERACTOR_STDERR, MAX_INTERACTOR_STDERR).unwrap_or_default();
    let report = ExitReport { code, stdout: String::new(), stderr: String::from_utf8_lossy(&stderr).into() };
    interpret(CustomKind::Testlib, &report).map_err(|e| GradeError::Checker(format!("interactor: {}", e.0)))
}

fn decide(boxes: &Boxes, x: &Interaction) -> Result<Interactive, GradeError> {
    let result = solution_result(boxes, x)?;
    if solution_wins(&result.verdict) {
        return Ok(result);
    }
    Ok(Interactive { check: Some(interactor_outcome(boxes)?), ..result })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interactor_limits_follow_the_spec() {
        let solution = Limits::for_solution(2.0, 65_536, 1, 1 << 20);
        let limits = interactor_limits(&solution);
        assert_eq!((limits.time_s, limits.wall_s, limits.memory_kib), (10.0, solution.wall_s + 2.0, 1 << 20));
        assert_eq!(limits.processes, 1);
        let slow = Limits::for_solution(8.0, 65_536, 1, 1 << 20);
        assert_eq!(interactor_limits(&slow).time_s, 16.0);
    }

    #[test]
    fn a_failed_solution_wins_except_for_a_broken_pipe() {
        assert!(solution_wins(&RunVerdict::Tle));
        assert!(solution_wins(&RunVerdict::Mle));
        assert!(solution_wins(&RunVerdict::Ir { exit_code: 1 }));
        assert!(solution_wins(&RunVerdict::Rte { reason: "SIGSEGV".into() }));
        assert!(!solution_wins(&RunVerdict::Rte { reason: "SIGPIPE".into() }));
        assert!(!solution_wins(&RunVerdict::Ok));
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
