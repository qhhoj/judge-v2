//! Spawning a session and waiting for it. The daemon creates the channel, hands each child its ends and drops its
//! own copies before returning, so a side that exits gives the other EOF. A pair that blocks on each other is
//! ended by the sandbox wall limits; the host deadline kills what is left if a limit does not fire (the error is
//! RETRY).

use crate::error::GradeError;
use judge_sandbox::IsolateBox;
use judge_sandbox::error::SandboxError;
use judge_sandbox::spec::{Limits, RunSpec};
use std::process::{Child, ExitStatus, Stdio as ProcessStdio};
use std::time::{Duration, Instant};

/// Host-side margin past the manager's own wall limit before the isolate processes are killed.
const DEADLINE_SLACK: Duration = Duration::from_secs(5);
/// isolate's own exit status for an internal error (1 is "the program failed").
const ISOLATE_FAILURE: i32 = 2;
const POLL: Duration = Duration::from_millis(2);
/// isolate resets signal dispositions inside the box, so a jury program is started through a shell that
/// ignores SIGPIPE and execs it: a last write to a contestant that already exited fails with EPIPE, and the
/// jury program still reaches its own verdict instead of dying.
const IGNORE_SIGPIPE: [&str; 3] = ["/bin/sh", "-c", "trap '' PIPE; exec \"$0\" \"$@\""];

/// How the manager and the contestant are joined. `StdioPipes`: two pipes, each side's stdin reads what the other
/// writes to its stdout (interactive problems).
pub enum ChannelPlan {
    StdioPipes,
}

/// One member of a session: the box it runs in and its run.
pub struct MemberSpec<'a> {
    pub sandbox: &'a IsolateBox,
    pub run: &'a RunSpec,
}

/// The running isolate processes of one session.
pub struct Session {
    manager: Child,
    contestant: Child,
}

/// The manager's wall limit plus the host slack: past it the session is killed and the job retried.
pub fn host_deadline(manager: &Limits) -> Instant {
    Instant::now() + Duration::from_secs_f64(manager.wall_s) + DEADLINE_SLACK
}

/// Every jury program starts through this shell: it ignores SIGPIPE and execs the program (see `IGNORE_SIGPIPE`).
pub fn ignoring_sigpipe(argv: Vec<String>) -> Vec<String> {
    IGNORE_SIGPIPE.iter().map(|part| part.to_string()).chain(argv).collect()
}

pub fn spawn_session(
    manager: &MemberSpec,
    contestant: &MemberSpec,
    channel: ChannelPlan,
) -> Result<Session, GradeError> {
    match channel {
        ChannelPlan::StdioPipes => spawn_piped(manager, contestant),
    }
}

/// Two `std::io::pipe()`s; every pipe end is moved into a `Command` and is gone when this returns.
fn spawn_piped(manager: &MemberSpec, contestant: &MemberSpec) -> Result<Session, GradeError> {
    let (to_manager_read, to_manager_write) = std::io::pipe()?;
    let (to_contestant_read, to_contestant_write) = std::io::pipe()?;
    let mut manager_child = manager
        .sandbox
        .command(manager.run)?
        .stdin(ProcessStdio::from(to_manager_read))
        .stdout(ProcessStdio::from(to_contestant_write))
        .stderr(ProcessStdio::null())
        .spawn()?;
    let contestant_child = contestant
        .sandbox
        .command(contestant.run)?
        .stdin(ProcessStdio::from(to_contestant_read))
        .stdout(ProcessStdio::from(to_manager_write))
        .stderr(ProcessStdio::null())
        .spawn();
    match contestant_child {
        Ok(child) => Ok(Session { manager: manager_child, contestant: child }),
        Err(error) => {
            let _ = manager_child.kill();
            let _ = manager_child.wait();
            Err(error.into())
        }
    }
}

/// Waits for every member; the manager is killed first at the deadline.
pub fn wait_all(session: &mut Session, deadline: Instant) -> Result<(), GradeError> {
    wait_children(&mut [&mut session.manager, &mut session.contestant], deadline)
}

/// Polls every child until all exited. Past the deadline all are killed in order, reaped, and the session is an
/// error. isolate exits 2 on its own failures, which is an error too, because its meta file then does not
/// describe the run. An isolate killed by a signal leaves no meta file, which `read_meta` refuses.
fn wait_children(children: &mut [&mut Child], deadline: Instant) -> Result<(), GradeError> {
    let mut statuses: Vec<Option<ExitStatus>> = vec![None; children.len()];
    loop {
        for (child, status) in children.iter_mut().zip(statuses.iter_mut()) {
            *status = status.or(child.try_wait()?);
        }
        if statuses.iter().all(Option::is_some) {
            return refuse_isolate_failure(&statuses);
        }
        if Instant::now() >= deadline {
            kill_all(children)?;
            return Err(SandboxError::Isolate("interaction exceeded the host deadline".into()).into());
        }
        std::thread::sleep(POLL);
    }
}

fn kill_all(children: &mut [&mut Child]) -> Result<(), GradeError> {
    for child in children.iter_mut() {
        let _ = child.kill();
    }
    for child in children.iter_mut() {
        child.wait()?;
    }
    Ok(())
}

fn refuse_isolate_failure(statuses: &[Option<ExitStatus>]) -> Result<(), GradeError> {
    match statuses.iter().flatten().find(|status| status.code() == Some(ISOLATE_FAILURE)) {
        Some(status) => Err(SandboxError::Isolate(format!("isolate exited with {status}")).into()),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spawn(program: &str, args: &[&str]) -> Child {
        std::process::Command::new(program).args(args).spawn().unwrap()
    }

    /// A reaped child has no `/proc/<pid>` entry; an unreaped one stays as a zombie. Unlike `try_wait`, looking
    /// here does not reap, so it proves the code under test did.
    fn is_reaped(child: &Child) -> bool {
        !std::path::Path::new(&format!("/proc/{}", child.id())).exists()
    }

    #[test]
    fn children_that_outlive_the_deadline_are_all_killed_reaped_and_reported() {
        let (mut a, mut b) = (spawn("sleep", &["30"]), spawn("sleep", &["30"]));
        let started = Instant::now();
        let result = wait_children(&mut [&mut a, &mut b], Instant::now() + Duration::from_millis(100));
        assert!(matches!(result, Err(GradeError::Sandbox(_))));
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(is_reaped(&a) && is_reaped(&b), "both killed and waited for, none left as a zombie");
    }

    #[test]
    fn finished_children_do_not_wait_for_the_deadline() {
        let (mut a, mut b) = (spawn("true", &[]), spawn("false", &[]));
        let started = Instant::now();
        wait_children(&mut [&mut a, &mut b], Instant::now() + Duration::from_secs(60)).unwrap();
        assert!(started.elapsed() < Duration::from_secs(5), "exit 1 is a failed program, not a failure");
    }

    #[test]
    fn an_isolate_internal_error_is_an_error() {
        let (mut a, mut b) = (spawn("true", &[]), spawn("sh", &["-c", "exit 2"]));
        let result = wait_children(&mut [&mut a, &mut b], Instant::now() + Duration::from_secs(60));
        assert!(matches!(result, Err(GradeError::Sandbox(_))));
    }

    #[test]
    fn any_number_of_children_is_waited_for() {
        let mut children: Vec<Child> = (0..3).map(|_| spawn("true", &[])).collect();
        let mut refs: Vec<&mut Child> = children.iter_mut().collect();
        wait_children(&mut refs, Instant::now() + Duration::from_secs(60)).unwrap();
        assert!(children.iter().all(is_reaped));
    }

    #[test]
    fn a_jury_program_started_through_the_wrapper_survives_sigpipe() {
        let argv =
            ignoring_sigpipe(vec!["/bin/sh".into(), "-c".into(), "kill -PIPE $$; echo survived".into()]);
        let output = std::process::Command::new(&argv[0]).args(&argv[1..]).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&output.stdout), "survived\n");
    }
}
