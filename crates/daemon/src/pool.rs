//! N slots, one worker task per slot (spec §2 "Daemon", §3.5 credit).

use crate::link::{SlotPool, SlotsFull};
use crate::outbox::{AssignmentRef, Outboxes};
use crate::wire::Assignment;
use crate::worker::{Job, Release, Runner, WorkerDeps, process};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

struct Running {
    r: AssignmentRef,
    abort: Arc<AtomicBool>,
}

struct Slot {
    tx: mpsc::UnboundedSender<Job>,
    running: Arc<Mutex<Option<Running>>>,
}

pub struct Pool {
    slots: Vec<Slot>,
    outboxes: Arc<Outboxes>,
    draining: AtomicBool,
}

impl Pool {
    pub fn start(deps: Arc<WorkerDeps>, runners: Vec<Box<dyn Runner>>) -> Arc<Pool> {
        let outboxes = deps.outboxes.clone();
        let slots = runners.into_iter().map(|runner| spawn_slot(deps.clone(), runner)).collect();
        Arc::new(Pool { slots, outboxes, draining: AtomicBool::new(false) })
    }

    /// Informational: the bridge stops assigning once it sees `Draining`, and an Assign that crossed
    /// that message is still accepted while a slot is free (preflight I2).
    pub fn set_draining(&self) {
        self.draining.store(true, Ordering::SeqCst);
    }

    pub fn is_draining(&self) -> bool {
        self.draining.load(Ordering::SeqCst)
    }

    pub fn idle(&self) -> bool {
        self.busy() == 0
    }

    fn kill(&self, r: &AssignmentRef) {
        for slot in &self.slots {
            if let Some(running) = slot.running.lock().unwrap().as_ref().filter(|run| &run.r == r) {
                running.abort.store(true, Ordering::SeqCst);
            }
        }
    }
}

/// One worker task per slot. The slot is freed by the job's `release` (right before its Final),
/// not when `process` returns; a job assigned meanwhile waits in the channel for the runner.
fn spawn_slot(deps: Arc<WorkerDeps>, mut runner: Box<dyn Runner>) -> Slot {
    let (tx, mut rx) = mpsc::unbounded_channel::<Job>();
    tokio::spawn(async move {
        while let Some(job) = rx.recv().await {
            runner = process(&deps, runner, job).await;
        }
    });
    Slot { tx, running: Arc::default() }
}

/// Clears `running` only while it still holds `r` (a newer assignment may already sit there).
fn release(running: &Arc<Mutex<Option<Running>>>, r: &AssignmentRef) -> Release {
    let (running, r) = (running.clone(), r.clone());
    Arc::new(move || {
        let mut slot = running.lock().unwrap();
        if slot.as_ref().is_some_and(|run| run.r == r) {
            *slot = None;
        }
    })
}

impl SlotPool for Pool {
    /// Refuses only when no slot is free; draining does not refuse (preflight I2).
    /// A repeated Assign for a ref this judge already holds is ignored: a second run would reset its outbox.
    fn assign(&self, assignment: Assignment) -> Result<(), SlotsFull> {
        if self.outboxes.contains(&assignment.r) {
            tracing::warn!(submission_id = %assignment.r.submission_id, generation = assignment.r.generation, "duplicate Assign ignored");
            return Ok(());
        }
        for slot in &self.slots {
            let mut running = slot.running.lock().unwrap();
            if running.is_none() {
                let abort = Arc::new(AtomicBool::new(false));
                *running = Some(Running { r: assignment.r.clone(), abort: abort.clone() });
                self.outboxes.open(&assignment.r);
                let release = release(&slot.running, &assignment.r);
                let _ = slot.tx.send(Job { assignment, abort, release });
                return Ok(());
            }
        }
        Err(SlotsFull)
    }

    fn abort(&self, r: &AssignmentRef) {
        self.kill(r);
    }

    /// Resume `DROP`: killed like `abort`; the link has marked the outbox, so only `Final{ABORTED}` goes out.
    fn drop_run(&self, r: &AssignmentRef) {
        self.kill(r);
    }

    fn busy(&self) -> u32 {
        self.slots.iter().filter(|s| s.running.lock().unwrap().is_some()).count() as u32
    }

    fn slots(&self) -> u32 {
        self.slots.len() as u32
    }
}
