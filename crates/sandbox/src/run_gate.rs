//! The host's run gate (COR-48): every sandboxed run holds it shared, and a TLE re-run holds it exclusive, so
//! the re-run is measured while no other box on the host runs (docs/grading.md#tle-re-run).
//!
//! Two layers, taken in this order and released in reverse:
//! - In the process, a FIFO queue: a pass is granted in the order it was asked for, consecutive shared passes
//!   together. A run asked for after a re-run waits for that one re-run only, and a re-run waits only for the
//!   runs in flight before it, so no slot starves.
//! - Across processes (the daemon and `selftest`), two `flock`s on files next to `host_lock`: `<host_lock>.turn`
//!   is taken exclusive by every pass on its way in, then `host_lock` itself shared or exclusive. An exclusive
//!   pass keeps the turn until it holds `host_lock`, so no new run slips past a waiting re-run (a plain shared
//!   `flock` is granted while an exclusive one waits).
//!
//! A thread already holding a pass (a re-run's own runs, a session's runs, the runs of a case that holds one)
//! gets a nested pass that waits for nothing. Every pass is a guard: a panic or an error releases it. The lock
//! files are 0600 and the daemon refuses a `cache_dir` others can reach, so no other user can hold the host; a
//! `flock` wait past 30 s is logged.

use crate::error::SandboxError;
use rustix::fs::{FlockOperation, flock};
use std::cell::Cell;
use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::marker::PhantomData;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{RecvTimeoutError, channel};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

/// A `flock` wait longer than this is logged: a re-run lasts one case, so a longer wait means a stuck holder.
const SLOW_LOCK: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Shared,
    Exclusive,
}

#[derive(Debug, Default)]
struct Queue {
    next_ticket: u64,
    waiting: VecDeque<(u64, Mode)>,
    shared: usize,
    exclusive: bool,
}

impl Queue {
    fn may_enter(&self, ticket: u64, mode: Mode) -> bool {
        let first = self.waiting.front().is_some_and(|(next, _)| *next == ticket);
        first && !self.exclusive && (mode == Mode::Shared || self.shared == 0)
    }
}

#[derive(Debug, Default)]
pub struct RunGate {
    queue: Mutex<Queue>,
    turn: Condvar,
    /// `None`: this process only (tests, a selftest without a daemon config).
    host_lock: Option<PathBuf>,
}

thread_local! {
    /// Passes this thread holds, nested ones included.
    static HELD: Cell<u32> = const { Cell::new(0) };
}

/// A held pass; dropping it lets the next one in.
pub struct RunPass {
    gate: Option<(Arc<RunGate>, Mode)>,
    host: Option<File>,
    /// The pass belongs to the thread that took it (`HELD`).
    _thread: PhantomData<*const ()>,
}

impl RunGate {
    pub fn new(host_lock: Option<PathBuf>) -> RunGate {
        RunGate { host_lock, ..RunGate::default() }
    }

    /// A pass for one run; waits while a re-run holds the host or was asked for first.
    pub fn shared(self: &Arc<Self>) -> Result<RunPass, SandboxError> {
        self.pass(Mode::Shared)
    }

    /// The host alone: waits for the runs in flight, and no run starts until the pass drops. Never asked for by
    /// a thread that holds a pass (it would wait for itself).
    pub fn exclusive(self: &Arc<Self>) -> Result<RunPass, SandboxError> {
        assert_eq!(HELD.get(), 0, "an exclusive pass is asked for outside any run");
        self.pass(Mode::Exclusive)
    }

    /// Whether some thread holds the host alone (logs and tests).
    pub fn is_exclusive(&self) -> bool {
        self.lock_queue().exclusive
    }

    /// Whether some thread holds a shared pass (tests).
    pub fn is_shared(&self) -> bool {
        self.lock_queue().shared > 0
    }

    fn pass(self: &Arc<Self>, mode: Mode) -> Result<RunPass, SandboxError> {
        let gate = if HELD.get() > 0 { None } else { Some((self.clone(), mode)) };
        if gate.is_some() {
            self.enter(mode);
        }
        HELD.set(HELD.get() + 1);
        let mut pass = RunPass { gate, host: None, _thread: PhantomData };
        if pass.gate.is_some() {
            pass.host = self.lock_host(mode)?; // an error drops `pass`, which leaves the queue
        }
        Ok(pass)
    }

    fn lock_queue(&self) -> MutexGuard<'_, Queue> {
        self.queue.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn enter(&self, mode: Mode) {
        let mut queue = self.lock_queue();
        let ticket = queue.next_ticket;
        queue.next_ticket += 1;
        queue.waiting.push_back((ticket, mode));
        let mut queue = self
            .turn
            .wait_while(queue, |queue| !queue.may_enter(ticket, mode))
            .unwrap_or_else(PoisonError::into_inner);
        queue.waiting.pop_front();
        match mode {
            Mode::Shared => queue.shared += 1,
            Mode::Exclusive => queue.exclusive = true,
        }
        self.turn.notify_all(); // the next shared pass may come in too
    }

    fn leave(&self, mode: Mode) {
        let mut queue = self.lock_queue();
        match mode {
            Mode::Shared => queue.shared -= 1,
            Mode::Exclusive => queue.exclusive = false,
        }
        self.turn.notify_all();
    }

    /// `host_lock` held in `mode`, through the turn. None without a host lock, or when its directory does not
    /// exist (the daemon creates it, so no daemon runs on this host).
    fn lock_host(&self, mode: Mode) -> Result<Option<File>, SandboxError> {
        let Some(path) = &self.host_lock else { return Ok(None) };
        let turn_path = turn_path(path);
        let (Some(turn), Some(runs)) = (open_lock(&turn_path)?, open_lock(path)?) else {
            return Ok(None);
        };
        lock((&turn, &turn_path), Mode::Exclusive)?;
        lock((&runs, path), mode)?;
        Ok(Some(runs)) // the turn closes here: the next pass may queue on `runs`
    }

    #[cfg(test)]
    fn waiting(&self) -> usize {
        self.lock_queue().waiting.len()
    }
}

impl Drop for RunPass {
    fn drop(&mut self) {
        self.host.take(); // closing the file releases its flock
        if let Some((gate, mode)) = self.gate.take() {
            gate.leave(mode);
        }
        HELD.set(HELD.get() - 1);
    }
}

fn turn_path(host_lock: &Path) -> PathBuf {
    let mut name = host_lock.as_os_str().to_owned();
    name.push(".turn");
    name.into()
}

/// Opens (creating, mode 0600) a lock file. None when the directory does not exist.
fn open_lock(path: &Path) -> Result<Option<File>, SandboxError> {
    let opened =
        OpenOptions::new().read(true).write(true).create(true).truncate(false).mode(0o600).open(path);
    let file = match opened {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        file => file?,
    };
    give_to_directory_owner(&file, path);
    Ok(Some(file))
}

/// A lock file another user made (`selftest` run as root) goes to the owner of its directory, the judge user,
/// who could not open it otherwise. Only root may, so for anyone else this changes nothing.
fn give_to_directory_owner(file: &File, path: &Path) {
    let dir = path.parent().and_then(|dir| std::fs::metadata(dir).ok());
    let (Some(dir), Ok(own)) = (dir, file.metadata()) else { return };
    if own.uid() != dir.uid() {
        let _ = std::os::unix::fs::fchown(file, Some(dir.uid()), Some(dir.gid()));
    }
}

fn operation(mode: Mode, wait: bool) -> FlockOperation {
    match (mode, wait) {
        (Mode::Shared, true) => FlockOperation::LockShared,
        (Mode::Exclusive, true) => FlockOperation::LockExclusive,
        (Mode::Shared, false) => FlockOperation::NonBlockingLockShared,
        (Mode::Exclusive, false) => FlockOperation::NonBlockingLockExclusive,
    }
}

/// `flock` without EINTR; Ok(false) when `wait` is off and another holder has it.
fn try_flock(file: &File, mode: Mode, wait: bool) -> Result<bool, SandboxError> {
    loop {
        match flock(file, operation(mode, wait)) {
            Ok(()) => return Ok(true),
            Err(rustix::io::Errno::INTR) => continue,
            Err(rustix::io::Errno::WOULDBLOCK) if !wait => return Ok(false),
            Err(error) => return Err(SandboxError::Io(format!("run lock: {error}"))),
        }
    }
}

/// Locks `file` in `mode`; a wait longer than `SLOW_LOCK` is logged with the lock's `path`.
fn lock((file, path): (&File, &Path), mode: Mode) -> Result<(), SandboxError> {
    if try_flock(file, mode, false)? {
        return Ok(());
    }
    let (done, waiting) = channel::<()>();
    let path = path.to_path_buf();
    let watch = std::thread::spawn(move || {
        if waiting.recv_timeout(SLOW_LOCK) == Err(RecvTimeoutError::Timeout) {
            tracing::warn!(lock = %path.display(), ?mode, "a run has waited over 30 s for the host lock");
        }
    });
    let locked = try_flock(file, mode, true);
    drop(done); // the watch ends at once
    let _ = watch.join();
    locked.map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::{Receiver, Sender, channel};
    use std::thread::JoinHandle;

    type Log = Arc<Mutex<Vec<&'static str>>>;

    /// Waits until `n` passes are queued behind the holders (a condition, not a timer).
    fn until_waiting(gate: &RunGate, n: usize) {
        while gate.waiting() < n {
            std::thread::yield_now();
        }
    }

    /// A thread that takes a pass in `mode`, logs `name`, waits for `release`, then logs `name` again.
    fn holder(
        gate: &Arc<RunGate>,
        mode: Mode,
        log: &Log,
        name: &'static str,
    ) -> (Sender<()>, JoinHandle<()>) {
        let (release, wait): (Sender<()>, Receiver<()>) = channel();
        let (gate, log) = (gate.clone(), log.clone());
        let thread = std::thread::spawn(move || {
            let _pass = gate.pass(mode).unwrap();
            log.lock().unwrap().push(name);
            wait.recv().unwrap();
            log.lock().unwrap().push(name);
        });
        (release, thread)
    }

    #[test]
    fn an_exclusive_pass_waits_for_runs_in_flight_and_holds_back_every_later_run() {
        let (gate, log): (Arc<RunGate>, Log) = Default::default();
        let (release_a, a) = holder(&gate, Mode::Shared, &log, "a");
        while log.lock().unwrap().is_empty() {
            std::thread::yield_now();
        }
        let (release_rerun, rerun) = holder(&gate, Mode::Exclusive, &log, "rerun");
        until_waiting(&gate, 1);
        let (release_c, c) = holder(&gate, Mode::Shared, &log, "c");
        until_waiting(&gate, 2);
        assert_eq!(*log.lock().unwrap(), ["a"], "the re-run waits for a; c waits behind the re-run");
        release_a.send(()).unwrap();
        a.join().unwrap();
        while log.lock().unwrap().len() < 3 {
            std::thread::yield_now();
        }
        assert!(gate.is_exclusive());
        assert_eq!(gate.waiting(), 1, "c still waits while the re-run holds the host");
        release_rerun.send(()).unwrap();
        rerun.join().unwrap();
        release_c.send(()).unwrap();
        c.join().unwrap();
        assert_eq!(*log.lock().unwrap(), ["a", "a", "rerun", "rerun", "c", "c"]);
    }

    #[test]
    fn runs_inside_a_pass_do_not_wait_and_the_host_lock_follows_the_mode() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("run.lock");
        let gate = Arc::new(RunGate::new(Some(path.clone())));
        let other = || File::open(&path).unwrap();
        let rerun = gate.exclusive().unwrap();
        let nested = gate.shared().unwrap();
        assert!(gate.is_exclusive(), "the re-run's own run passes");
        assert!(flock(other(), FlockOperation::NonBlockingLockShared).is_err(), "another process waits");
        drop((nested, rerun));
        assert!(!gate.is_exclusive());
        let run = gate.shared().unwrap();
        assert!(flock(other(), FlockOperation::NonBlockingLockShared).is_ok(), "runs share the host");
        assert!(flock(other(), FlockOperation::NonBlockingLockExclusive).is_err());
        drop(run);
        for file in [path.clone(), turn_path(&path)] {
            let mode = std::fs::metadata(file).unwrap().mode() & 0o777;
            assert_eq!(mode, 0o600, "no other user can hold the host");
        }
        let missing = Arc::new(RunGate::new(Some(dir.path().join("none/run.lock"))));
        assert!(missing.shared().is_ok(), "no lock directory: no daemon on this host");
    }
}
