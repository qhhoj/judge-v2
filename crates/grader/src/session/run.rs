//! Spawning a session and waiting for it. The daemon creates the channel, hands each child its ends and drops its
//! own copies before returning, so a side that exits gives the other EOF. A pair that blocks on each other is
//! ended by the sandbox wall limits; the host deadline kills what is left if a limit does not fire (the error is
//! RETRY).

use crate::error::GradeError;
use crate::session::channel::RingChannel;
use judge_sandbox::IsolateBox;
use judge_sandbox::error::SandboxError;
use judge_sandbox::spec::{Limits, RunSpec};
use std::process::{Child, ExitStatus, Stdio as ProcessStdio};
use std::time::{Duration, Instant};

/// Host-side margin past the manager's own wall limit before the isolate processes are killed.
const DEADLINE_SLACK: Duration = Duration::from_secs(5);
/// Exit statuses that mean isolate did not run properly, so its meta file is stale. isolate exits 2 on its own
/// failures (1 is "the program failed"). A channel run starts isolate through the mover shell, which exits 126
/// (found but not executable) or 127 (not found) when its `exec` fails; so does `taskset`. isolate itself only
/// exits 0, 1 or 2, so these never hide a program's result.
const ISOLATE_FAILURES: [i32; 3] = [2, 126, 127];
const POLL: Duration = Duration::from_millis(2);
/// isolate resets signal dispositions inside the box, so a jury program is started through a shell that
/// ignores SIGPIPE and execs it: a last write to a contestant that already exited fails with EPIPE, and the
/// jury program still reaches its own verdict instead of dying.
const IGNORE_SIGPIPE: [&str; 3] = ["/bin/sh", "-c", "trap '' PIPE; exec \"$0\" \"$@\""];

/// How the manager and the contestant are joined.
pub enum ChannelPlan {
    /// Two pipes, each side's stdin reads what the other writes to its stdout (interactive problems).
    StdioPipes,
    /// The split channel on fds 3/4/5 of each box (`channel`): the manager gets `jury`, the contestant
    /// `contestant`. Both runs must set `inherit_fds`. Every daemon copy is dropped when the spawn returns.
    Ring(RingChannel),
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
        ChannelPlan::Ring(ring) => spawn_ring(manager, contestant, ring),
    }
}

/// Two `std::io::pipe()`s; every pipe end is moved into a `Command` and is gone when this returns. Both command
/// lines are built before either is spawned, so a refused one leaves no isolate running.
fn spawn_piped(manager: &MemberSpec, contestant: &MemberSpec) -> Result<Session, GradeError> {
    let (mut manager_command, mut contestant_command) =
        (manager.sandbox.command(manager.run)?, contestant.sandbox.command(contestant.run)?);
    let (to_manager_read, to_manager_write) = std::io::pipe()?;
    let (to_contestant_read, to_contestant_write) = std::io::pipe()?;
    let mut manager_child = manager_command
        .stdin(ProcessStdio::from(to_manager_read))
        .stdout(ProcessStdio::from(to_contestant_write))
        .stderr(ProcessStdio::null())
        .spawn()?;
    let contestant_child = contestant_command
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

/// Both command lines are built before either is spawned, so a refused one leaves no isolate running; a failed
/// second spawn kills and reaps the first. `ring` is consumed: its fds move into the two commands and close in the
/// daemon when the commands drop, so a box never keeps a peer's doorbell open.
fn spawn_ring(
    manager: &MemberSpec,
    contestant: &MemberSpec,
    ring: RingChannel,
) -> Result<Session, GradeError> {
    let RingChannel { jury, contestant: contestant_fds } = ring;
    let mut manager_command = manager.sandbox.command_with_channel(manager.run, jury)?;
    let mut contestant_command = contestant.sandbox.command_with_channel(contestant.run, contestant_fds)?;
    let mut manager_child = manager_command.spawn()?;
    drop(manager_command); // the daemon's copies of the jury's fds close now
    match contestant_command.spawn() {
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
/// error. An isolate failure (`ISOLATE_FAILURES`) is an error too, because its meta file then does not describe
/// the run. An isolate killed by a signal leaves its meta file missing or empty, which `read_meta` refuses.
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
    let isolate_failed =
        |status: &&ExitStatus| status.code().is_some_and(|code| ISOLATE_FAILURES.contains(&code));
    match statuses.iter().flatten().find(isolate_failed) {
        Some(status) => Err(SandboxError::Isolate(format!("isolate exited with {status}")).into()),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::channel::RingChannel;
    use crate::session::fake_isolate::fake_box;

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
    fn a_mover_that_cannot_exec_isolate_is_an_error() {
        for code in ["126", "127"] {
            let (mut a, mut b) = (spawn("true", &[]), spawn("sh", &["-c", &format!("exit {code}")]));
            let result = wait_children(&mut [&mut a, &mut b], Instant::now() + Duration::from_secs(60));
            assert!(matches!(result, Err(GradeError::Sandbox(_))), "exit {code}: {result:?}");
        }
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

    fn sleeping_run() -> RunSpec {
        RunSpec {
            argv: vec!["/bin/sleep".into(), "30".into()],
            limits: Limits::for_solution(1.0, 65_536, 1, 1 << 20),
            stdio: judge_sandbox::spec::Stdio::default(),
            env: vec![],
            dirs: vec![],
            inherit_fds: true,
        }
    }

    fn two_fake_boxes() -> (Vec<tempfile::TempDir>, IsolateBox, IsolateBox) {
        let dirs: Vec<_> = (0..2).map(|_| tempfile::tempdir().unwrap()).collect();
        let manager_box = fake_box(dirs[0].path(), 7);
        let contestant_box = fake_box(dirs[1].path(), 8);
        (dirs, manager_box, contestant_box)
    }

    /// Both boxes opened through the fake, which is then deleted: the mover's `exec` of isolate finds nothing.
    fn two_boxes_with_missing_isolate() -> (Vec<tempfile::TempDir>, IsolateBox, IsolateBox) {
        let (dirs, manager_box, contestant_box) = two_fake_boxes();
        for dir in &dirs {
            std::fs::remove_file(dir.path().join("isolate")).unwrap();
        }
        (dirs, manager_box, contestant_box)
    }

    #[test]
    fn a_ring_session_whose_second_member_is_refused_starts_neither() {
        let (dirs, manager_box, contestant_box) = two_fake_boxes();
        let manager_run = sleeping_run();
        let contestant_run = RunSpec { inherit_fds: false, ..sleeping_run() }; // command_with_channel refuses it
        let started = Instant::now();
        let result = spawn_session(
            &MemberSpec { sandbox: &manager_box, run: &manager_run },
            &MemberSpec { sandbox: &contestant_box, run: &contestant_run },
            ChannelPlan::Ring(RingChannel::create().unwrap()),
        );
        assert!(matches!(result, Err(GradeError::Sandbox(_))));
        assert!(started.elapsed() < Duration::from_secs(5), "no member was started and waited out");
        let manager_calls = std::fs::read_to_string(dirs[0].path().join("calls")).unwrap();
        assert!(!manager_calls.contains("--run"), "the manager never started: {manager_calls}");
    }

    #[test]
    fn an_isolate_binary_missing_behind_the_mover_is_a_sandbox_error() {
        let (_dirs, manager_box, contestant_box) = two_boxes_with_missing_isolate();
        let run = sleeping_run();
        let mut session = spawn_session(
            &MemberSpec { sandbox: &manager_box, run: &run },
            &MemberSpec { sandbox: &contestant_box, run: &run },
            ChannelPlan::Ring(RingChannel::create().unwrap()),
        )
        .expect("the spawn itself succeeds: the command is /bin/sh");
        let waited = wait_all(&mut session, Instant::now() + Duration::from_secs(10));
        assert!(matches!(waited, Err(GradeError::Sandbox(SandboxError::Isolate(_)))), "{waited:?}");
    }
}
