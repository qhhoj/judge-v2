//! N slots, one worker task per slot (docs/architecture.md; the bridge sends at most `slots` assignments, docs/protocol.md#flow-control).

use crate::generation::{GenerationAssignment, process_generation};
use crate::link::{SlotPool, SlotsFull};
use crate::outbox::{AssignmentRef, Outboxes};
use crate::wire::Assignment;
use crate::worker::{Job, Release, Runner, SlotJob, WorkerDeps, process};
use judge_grader::generate::GenerationPlan;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use tokio::sync::mpsc;

/// What a busy slot is doing: a submission's run, or a test generation.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Work {
    Run(AssignmentRef),
    Generation(String),
}

struct Running {
    work: Work,
    abort: Arc<AtomicBool>,
}

struct Slot {
    tx: mpsc::UnboundedSender<SlotJob>,
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
    /// that message is still accepted while a slot is free (docs/operations.md#drain).
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
        self.abort_where(|work| work == &Work::Run(r.clone()));
    }

    fn abort_where(&self, matching: impl Fn(&Work) -> bool) {
        for slot in &self.slots {
            if let Some(running) = slot.running.lock().unwrap().as_ref().filter(|run| matching(&run.work)) {
                running.abort.store(true, Ordering::SeqCst);
            }
        }
    }

    fn is_running(&self, work: &Work) -> bool {
        self.slots
            .iter()
            .any(|slot| slot.running.lock().unwrap().as_ref().is_some_and(|run| &run.work == work))
    }

    /// Gives the work to the first free slot, else to the first slot a cancelled generation still holds;
    /// `job` makes the slot's job from its abort flag and release.
    fn place(
        &self,
        work: Work,
        job: impl FnOnce(Arc<AtomicBool>, Release) -> SlotJob,
    ) -> Result<(), SlotsFull> {
        let free = |running: &Option<Running>| running.is_none();
        let slot = self.claim(free).or_else(|| self.claim(Running::is_cancelled_generation));
        let Some((slot, mut running)) = slot else { return Err(SlotsFull) };
        let abort = Arc::new(AtomicBool::new(false));
        *running = Some(Running { work: work.clone(), abort: abort.clone() });
        let _ = slot.tx.send(job(abort, release(&slot.running, &work)));
        Ok(())
    }

    /// The first slot whose `running` passes `takes`, locked.
    fn claim(
        &self,
        takes: impl Fn(&Option<Running>) -> bool,
    ) -> Option<(&Slot, MutexGuard<'_, Option<Running>>)> {
        self.slots.iter().find_map(|slot| {
            let running = slot.running.lock().unwrap();
            takes(&running).then_some((slot, running))
        })
    }
}

impl Running {
    /// A generation cancelled (its session is gone) inside a program run keeps its slot until that run ends,
    /// though the bridge already counts the slot free; work given to it waits in the slot's channel.
    fn is_cancelled_generation(running: &Option<Running>) -> bool {
        running
            .as_ref()
            .is_some_and(|run| matches!(run.work, Work::Generation(_)) && run.abort.load(Ordering::SeqCst))
    }
}

/// One worker task per slot. The slot is freed by the job's `release` (right before its Final),
/// not when `process` returns; a job assigned meanwhile waits in the channel for the runner.
fn spawn_slot(deps: Arc<WorkerDeps>, mut runner: Box<dyn Runner>) -> Slot {
    let (tx, mut rx) = mpsc::unbounded_channel::<SlotJob>();
    tokio::spawn(async move {
        while let Some(job) = rx.recv().await {
            runner = match job {
                SlotJob::Run(job) => process(&deps, runner, job).await,
                SlotJob::Generate(job) => process_generation(&deps, runner, job).await,
            };
        }
    });
    Slot { tx, running: Arc::default() }
}

/// Clears `running` only while it still holds `work` (a newer assignment may already sit there).
fn release(running: &Arc<Mutex<Option<Running>>>, work: &Work) -> Release {
    let (running, work) = (running.clone(), work.clone());
    Arc::new(move || {
        let mut slot = running.lock().unwrap();
        if slot.as_ref().is_some_and(|run| run.work == work) {
            *slot = None;
        }
    })
}

impl SlotPool for Pool {
    /// Refuses only when no slot is free; draining does not refuse (docs/operations.md#drain).
    /// A repeated Assign for a ref this judge already holds is ignored: a second run would reset its outbox.
    fn assign(&self, assignment: Assignment) -> Result<(), SlotsFull> {
        if self.outboxes.contains(&assignment.r) {
            tracing::warn!(submission_id = %assignment.r.submission_id, generation = assignment.r.generation, "duplicate Assign ignored");
            return Ok(());
        }
        let r = assignment.r.clone();
        self.place(Work::Run(r.clone()), |abort, release| {
            self.outboxes.open(&r);
            SlotJob::Run(Job { assignment, abort, release })
        })
    }

    /// Takes a free slot like an Assign does; a generation already held is ignored.
    fn generate(&self, plan: GenerationPlan, session_id: String) -> Result<(), SlotsFull> {
        let work = Work::Generation(plan.id.clone());
        if self.is_running(&work) {
            tracing::warn!(generation_id = %plan.id, "duplicate GenerateTests ignored");
            return Ok(());
        }
        self.place(work, |abort, release| {
            SlotJob::Generate(GenerationAssignment { plan, session_id, abort, release })
        })
    }

    fn cancel_generation(&self, generation_id: &str) {
        self.abort_where(|work| work == &Work::Generation(generation_id.to_string()));
    }

    fn cancel_generations(&self) {
        self.abort_where(|work| matches!(work, Work::Generation(_)));
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
