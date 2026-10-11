//! One assignment from Assign to Final (spec §2 "Daemon").
//!
//! Lifecycle: gate `ack` → abort check → `Accepted` → source → resolve → grade (gate `begin` before
//! `GradingStarted`) → flush cases → gate `end` → `Final`. The outcome is taken only once the runner has
//! returned: `judge_submission` may still turn a Graded, IE or signature CE into `RETRY DATA_UNAVAILABLE`
//! when it rechecks a loose-file problem's stamp at the end.

use crate::batcher::{CaseBatcher, MAX_BATCH_AGE};
use crate::gates::{Gates, Stage};
use crate::generation::{GenerationAssignment, GenerationLink};
use crate::metrics::{Metrics, outcome_label};
use crate::outbox::{AssignmentRef, Body, Outboxes};
use crate::source::{MAX_FILE_SOURCE, SourceFetch};
use crate::wire::Assignment;
use judge_grader::generate::{Control, GenerationFailure, GenerationJob, generate_tests};
use judge_grader::judge::{JudgeEnv, Outcome, RetryReason, Submission, SubmissionSource, judge_submission};
use judge_grader::report::{CaseReport, Reporter};
use judge_problem::data_version::DataError;
use judge_problem::resolve::{ResolvedProblem, Resolver};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

/// A FetchSource that has not finished by then is DATA_UNAVAILABLE.
pub const FETCH_TIMEOUT: Duration = Duration::from_secs(60);
const ABORT_POLL: Duration = Duration::from_millis(20);

pub trait Runner: Send {
    fn run(
        &mut self,
        problem: &ResolvedProblem,
        submission: &Submission,
        reporter: &mut dyn Reporter,
    ) -> Outcome;

    /// Runners that cannot generate tests (the test doubles) fail the job.
    fn generate(&mut self, _job: &GenerationJob, _control: &Control) -> Result<(), GenerationFailure> {
        Err(GenerationFailure::internal("this runner cannot generate tests"))
    }
}

impl Runner for JudgeEnv {
    fn run(
        &mut self,
        problem: &ResolvedProblem,
        submission: &Submission,
        reporter: &mut dyn Reporter,
    ) -> Outcome {
        judge_submission(self, problem, submission, reporter)
    }

    fn generate(&mut self, job: &GenerationJob, control: &Control) -> Result<(), GenerationFailure> {
        generate_tests(self, job, control)
    }
}

/// Finds an assignment's problem data (blocking). `Resolver` in the daemon.
pub trait Resolve: Send + Sync {
    fn resolve(&self, code: &str, assigned: u64) -> Result<ResolvedProblem, DataError>;
}

impl Resolve for Resolver {
    fn resolve(&self, code: &str, assigned: u64) -> Result<ResolvedProblem, DataError> {
        Resolver::resolve(self, code, assigned)
    }
}

pub struct WorkerDeps {
    /// The judge name, on every log line about an assignment.
    pub judge: String,
    pub outboxes: Arc<Outboxes>,
    pub resolver: Arc<dyn Resolve>,
    pub fetch: Arc<dyn SourceFetch>,
    pub generation: Arc<dyn GenerationLink>,
    pub gates: Gates,
    pub metrics: Arc<Metrics>,
    pub scratch: PathBuf,
    /// The longest a FetchSource may take (`FETCH_TIMEOUT` in the daemon).
    pub fetch_timeout: Duration,
}

/// Frees the job's pool slot; called right before `Final` is pushed, so the slot is free by the time
/// the bridge can see the Final and send the next Assign (preflight C2).
pub type Release = Arc<dyn Fn() + Send + Sync>;

/// What a slot's worker task takes from the pool.
pub enum SlotJob {
    Run(Job),
    Generate(GenerationAssignment),
}

pub struct Job {
    pub assignment: Assignment,
    pub abort: Arc<AtomicBool>,
    pub release: Release,
}

impl Job {
    fn aborted(&self) -> bool {
        self.abort.load(Ordering::SeqCst)
    }
}

enum Event {
    Compiled(String),
    Started(bool, u32),
    Case(CaseReport),
}

/// Runs on the blocking thread; forwards events to the async side.
struct ChannelReporter {
    tx: mpsc::UnboundedSender<Event>,
    abort: Arc<AtomicBool>,
    outboxes: Arc<Outboxes>,
    gates: Gates,
}

impl Reporter for ChannelReporter {
    fn compiled(&mut self, log: &str) {
        let _ = self.tx.send(Event::Compiled(log.to_string()));
    }

    fn started(&mut self, pretested: bool, case_count: u32) {
        self.gates.wait_blocking(Stage::Begin, &self.abort);
        let _ = self.tx.send(Event::Started(pretested, case_count));
    }

    fn case(&mut self, report: CaseReport) {
        let _ = self.tx.send(Event::Case(report));
    }

    /// Waits while the outboxes are over the watermark; false once the job is aborted.
    fn before_case(&mut self) -> bool {
        while self.outboxes.over_watermark() && !self.abort.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(50));
        }
        !self.abort.load(Ordering::SeqCst)
    }
}

/// One job on this slot's runner. The outbox was opened by `Pool::assign` (preflight M2).
pub async fn process(deps: &WorkerDeps, runner: Box<dyn Runner>, job: Job) -> Box<dyn Runner> {
    deps.gates.wait(Stage::Ack, &job.abort).await;
    if job.aborted() {
        // Aborted or dropped before Accepted: only Final{ABORTED}, at seq 1 (preflight C2/C3).
        finish(deps, &job, Outcome::Aborted, 0).await;
        return runner;
    }
    deps.outboxes.push(&job.assignment.r, Body::Accepted);
    let (runner, outcome, version) = match prepare(deps, &job).await {
        Ok(prepared) => {
            let version = prepared.problem.data_version_used;
            let (runner, outcome) = grade(deps, runner, &job, prepared).await;
            (runner, outcome, version)
        }
        Err(outcome) => (runner, outcome, 0),
    };
    finish(deps, &job, outcome, version).await;
    discard_source(deps, &job.assignment.r);
    runner
}

fn source_path(deps: &WorkerDeps, r: &AssignmentRef) -> PathBuf {
    deps.scratch.join(format!("source-{}-{}", r.submission_id.replace('/', "_"), r.generation))
}

fn discard_source(deps: &WorkerDeps, r: &AssignmentRef) {
    let _ = std::fs::remove_file(source_path(deps, r));
}

/// Resolves once the job's abort flag is set.
pub(crate) async fn aborted(abort: &AtomicBool) {
    while !abort.load(Ordering::SeqCst) {
        tokio::time::sleep(ABORT_POLL).await;
    }
}

/// Inline sources come with the Assign; file-only ones over FetchSource. A failed or stalled fetch
/// (`fetch_timeout`) is DATA_UNAVAILABLE; an abort stops the wait and the run ends ABORTED.
async fn source(deps: &WorkerDeps, job: &Job) -> Result<SubmissionSource, Outcome> {
    let a = &job.assignment;
    if !a.source_is_file {
        return Ok(SubmissionSource::Inline(a.source.clone()));
    }
    let path = source_path(deps, &a.r);
    // source_size 0 = not announced: only the 64 MiB cap applies.
    let announced = if a.source_size == 0 { MAX_FILE_SOURCE } else { a.source_size.min(MAX_FILE_SOURCE) };
    let fetch = tokio::time::timeout(deps.fetch_timeout, deps.fetch.fetch(&a.r, announced, &path));
    let fetched = tokio::select! {
        fetched = fetch => fetched.unwrap_or_else(|_| Err("FetchSource timed out".into())),
        _ = aborted(&job.abort) => return Err(Outcome::Aborted),
    };
    if let Err(error) = fetched {
        tracing::warn!(judge = %deps.judge, submission_id = %a.r.submission_id, %error, "source fetch failed");
        return Err(Outcome::Retry(RetryReason::DataUnavailable));
    }
    Ok(SubmissionSource::File(path))
}

async fn resolve(deps: &WorkerDeps, a: &Assignment) -> Result<ResolvedProblem, Outcome> {
    let started = Instant::now();
    let (resolver, code, version) = (deps.resolver.clone(), a.problem_code.clone(), a.data_version);
    let resolved = tokio::task::spawn_blocking(move || resolver.resolve(&code, version))
        .await
        .map_err(|e| Outcome::InternalError(e.to_string()))?;
    deps.metrics.data_wait.observe(started.elapsed().as_secs_f64());
    resolved.map_err(|error| match error {
        DataError::Unavailable { .. } => Outcome::Retry(RetryReason::DataUnavailable),
        other => Outcome::InternalError(other.to_string()),
    })
}

/// What a runner grades.
struct Prepared {
    problem: ResolvedProblem,
    submission: Submission,
}

async fn prepare(deps: &WorkerDeps, job: &Job) -> Result<Prepared, Outcome> {
    let a = &job.assignment;
    let source = source(deps, job).await?;
    if job.aborted() {
        return Err(Outcome::Aborted);
    }
    let problem = resolve(deps, a).await?;
    let submission = Submission {
        language_key: a.language_key.clone(),
        source,
        time_limit_s: a.time_limit_s,
        memory_kib: a.memory_kib,
        short_circuit: a.short_circuit,
        pretests_only: a.pretests_only,
    };
    Ok(Prepared { problem, submission })
}

/// Grades on a blocking thread; a panicking runner is an internal error, and the slot keeps its runner.
async fn grade(
    deps: &WorkerDeps,
    mut runner: Box<dyn Runner>,
    job: &Job,
    prepared: Prepared,
) -> (Box<dyn Runner>, Outcome) {
    if job.aborted() {
        return (runner, Outcome::Aborted); // before compile
    }
    let (tx, rx) = mpsc::unbounded_channel();
    let mut reporter = ChannelReporter {
        tx,
        abort: job.abort.clone(),
        outboxes: deps.outboxes.clone(),
        gates: deps.gates.clone(),
    };
    let forwarder = Forwarder::new(deps, &job.assignment);
    let forward = tokio::spawn(forwarder.run(rx));
    let blocking = tokio::task::spawn_blocking(move || {
        let Prepared { problem, submission } = prepared;
        let ran = catch_unwind(AssertUnwindSafe(|| runner.run(&problem, &submission, &mut reporter)));
        drop(reporter); // closes the channel: the forwarder flushes and ends
        (runner, ran.unwrap_or_else(|_| Outcome::InternalError("the grader panicked".into())))
    });
    let (runner, outcome) = blocking.await.expect("the grading thread is joined");
    let _ = forward.await;
    (runner, outcome)
}

/// Turns grading events into outbox messages, coalescing cases (spec §3.5).
struct Forwarder {
    outboxes: Arc<Outboxes>,
    metrics: Arc<Metrics>,
    r: AssignmentRef,
    lang: String,
    started: Instant,
    batcher: CaseBatcher,
}

impl Forwarder {
    fn new(deps: &WorkerDeps, a: &Assignment) -> Self {
        let batcher = CaseBatcher::new(deps.outboxes.limits().batch_cases(), MAX_BATCH_AGE);
        Forwarder {
            outboxes: deps.outboxes.clone(),
            metrics: deps.metrics.clone(),
            r: a.r.clone(),
            lang: a.language_key.clone(),
            started: Instant::now(),
            batcher,
        }
    }

    async fn run(mut self, mut rx: mpsc::UnboundedReceiver<Event>) {
        loop {
            let event = match self.batcher.due() {
                Some(at) => tokio::time::timeout_at(at.into(), rx.recv()).await,
                None => Ok(rx.recv().await),
            };
            match event {
                Err(_) => self.flush(),
                Ok(None) => return self.flush(),
                Ok(Some(event)) => self.on_event(event),
            }
        }
    }

    fn on_event(&mut self, event: Event) {
        match event {
            Event::Case(report) => {
                self.metrics.case_seconds.observe(report.time_s);
                let full = self.batcher.push(report, Instant::now());
                self.push_cases(full);
            }
            Event::Compiled(log) => {
                let seconds = self.started.elapsed().as_secs_f64();
                self.metrics.compile_seconds.with_label_values(&[self.lang.as_str()]).observe(seconds);
                self.outboxes.push(&self.r, Body::Compiled { log });
            }
            Event::Started(pretested, case_count) => {
                self.outboxes.push(&self.r, Body::Started { pretested, case_count });
            }
        }
    }

    fn flush(&mut self) {
        let cases = self.batcher.flush();
        self.push_cases(cases);
    }

    fn push_cases(&self, cases: Option<Vec<CaseReport>>) {
        if let Some(cases) = cases {
            self.outboxes.push(&self.r, Body::Cases(cases));
        }
    }
}

/// A job whose abort flag is set ends with `Final{ABORTED}` whatever the runner returned:
/// the bridge frees that slot only on this Final (preflight C2).
async fn finish(deps: &WorkerDeps, job: &Job, outcome: Outcome, data_version_used: u64) {
    let r = &job.assignment.r;
    deps.gates.wait(Stage::End, &job.abort).await;
    let outcome = if job.aborted() { Outcome::Aborted } else { outcome };
    deps.metrics.assignment_finished(&outcome);
    tracing::info!(
        judge = %deps.judge,
        submission_id = %r.submission_id,
        generation = r.generation,
        lang = %job.assignment.language_key,
        outcome = outcome_label(&outcome),
        "assignment finished"
    );
    let log = final_log(&outcome);
    (job.release)();
    deps.outboxes.push(r, Body::Final { outcome, log, data_version_used });
}

/// `Final.log`: the compile log, the internal error's message, or the retry reason.
fn final_log(outcome: &Outcome) -> String {
    match outcome {
        Outcome::CompileError(log) | Outcome::InternalError(log) => log.clone(),
        Outcome::Retry(reason) => format!("{reason:?}"),
        Outcome::Graded | Outcome::Aborted => String::new(),
    }
}
