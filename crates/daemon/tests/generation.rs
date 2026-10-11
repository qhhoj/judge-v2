//! A generation on a pool slot: the stages, the slot it holds, cancel, and the session it is bound to.
//! The grader's generation itself is covered by the isolate suite; the runner here is a double.
mod support;

use judge_daemon::config::parse;
use judge_daemon::gates::Gates;
use judge_daemon::generation::{GenerationLink, GrpcGeneration, MAX_BUNDLE_BYTES, Ticket};
use judge_daemon::link::{Connection, SlotPool};
use judge_daemon::metrics::Metrics;
use judge_daemon::outbox::{OUTBOX_WATERMARK, Outboxes};
use judge_daemon::pool::Pool;
use judge_daemon::source::SourceFetch;
use judge_daemon::wire::{JudgeMessage, judge_message, v1};
use judge_daemon::worker::{Resolve, Runner, WorkerDeps};
use judge_grader::generate::{Control, GenerationFailure, GenerationJob, GenerationPlan, Kind, Stage};
use judge_grader::judge::{Outcome, Submission};
use judge_grader::report::Reporter;
use judge_problem::data_version::DataError;
use judge_problem::resolve::ResolvedProblem;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use support::fake_bridge::{eventually, start};

/// Records what the worker does on the wire; `live` is the one session whose messages get through.
struct Link {
    events: Mutex<Vec<String>>,
    sent: Mutex<Vec<JudgeMessage>>,
    live: String,
    fetch_hangs: bool,
}

#[tonic::async_trait]
impl GenerationLink for Link {
    async fn fetch_bundle(&self, _: &Ticket, max_bytes: u64, dest: &Path) -> Result<(), String> {
        self.events.lock().unwrap().push(format!("fetch up to {max_bytes}"));
        if self.fetch_hangs {
            std::future::pending::<()>().await;
        }
        std::fs::write(dest, b"bundle").map_err(|e| e.to_string())
    }

    async fn upload(&self, _: &Ticket, file: &Path) -> Result<(), String> {
        let size = std::fs::metadata(file).map_err(|e| e.to_string())?.len();
        self.events.lock().unwrap().push(format!("upload {size}"));
        Ok(())
    }

    async fn send(&self, ticket: &Ticket, message: JudgeMessage) -> Result<(), String> {
        if ticket.session_id != self.live {
            return Err("the session has ended".into());
        }
        self.sent.lock().unwrap().push(message);
        Ok(())
    }
}

/// What the double generation does: the result it returns, and whether it waits for a cancel first.
struct Generating {
    result: Result<(), GenerationFailure>,
    wait_for_cancel: bool,
}

impl Runner for Generating {
    fn run(&mut self, _: &ResolvedProblem, _: &Submission, _: &mut dyn Reporter) -> Outcome {
        unreachable!("no submissions in this test")
    }

    fn generate(&mut self, job: &GenerationJob, control: &Control) -> Result<(), GenerationFailure> {
        assert_eq!(std::fs::read(&job.bundle_zip).unwrap(), b"bundle", "the bundle was fetched first");
        if self.wait_for_cancel {
            loop {
                control.check()?;
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        self.result.clone()?;
        std::fs::write(&job.result_zip, b"result zip").unwrap();
        Ok(())
    }
}

struct NoProblems;
impl Resolve for NoProblems {
    fn resolve(&self, _: &str, _: u64) -> Result<ResolvedProblem, DataError> {
        unreachable!("no submissions in this test")
    }
}

struct NoSources;
#[tonic::async_trait]
impl SourceFetch for NoSources {
    async fn fetch(&self, _: &judge_daemon::outbox::AssignmentRef, _: u64, _: &Path) -> Result<(), String> {
        unreachable!("no submissions in this test")
    }
}

struct World {
    pool: Arc<Pool>,
    link: Arc<Link>,
    outboxes: Arc<Outboxes>,
    scratch: tempfile::TempDir,
}

fn world(runner: Generating, fetch_hangs: bool) -> World {
    let scratch = tempfile::tempdir().unwrap();
    let link =
        Arc::new(Link { events: Mutex::default(), sent: Mutex::default(), live: "s1".into(), fetch_hangs });
    let outboxes = Arc::new(Outboxes::new(OUTBOX_WATERMARK));
    let deps = WorkerDeps {
        judge: "j1".into(),
        outboxes: outboxes.clone(),
        resolver: Arc::new(NoProblems),
        fetch: Arc::new(NoSources),
        generation: link.clone(),
        gates: Gates { dir: None },
        metrics: Arc::new(Metrics::new()),
        scratch: scratch.path().into(),
        fetch_timeout: Duration::from_secs(5),
    };
    let pool = Pool::start(Arc::new(deps), vec![Box::new(runner)]);
    World { pool, link, outboxes, scratch }
}

fn generating(result: Result<(), GenerationFailure>) -> Generating {
    Generating { result, wait_for_cancel: false }
}

fn plan(id: &str) -> GenerationPlan {
    GenerationPlan {
        id: id.into(),
        bundle_size: 6,
        deadline_s: 60,
        limits: Default::default(),
        programs: vec![],
        tests: vec![],
        file_io: None,
    }
}

fn done(world: &World) -> Vec<v1::GenerationDone> {
    let sent = world.link.sent.lock().unwrap();
    sent.iter()
        .filter_map(|m| match &m.body {
            Some(judge_message::Body::GenerationDone(done)) => Some(done.clone()),
            _ => None,
        })
        .collect()
}

async fn finished(world: &World) -> v1::GenerationDone {
    eventually(|| !done(world).is_empty()).await;
    done(world).remove(0)
}

fn leftovers(world: &World) -> usize {
    std::fs::read_dir(world.scratch.path()).unwrap().count()
}

#[tokio::test]
async fn a_generation_fetches_generates_uploads_and_then_reports_done_with_its_slot_free() {
    let w = world(generating(Ok(())), false);
    w.pool.generate(plan("g1"), "s1".into()).unwrap();
    let report = finished(&w).await;
    assert_eq!((report.generation_id.as_str(), report.ok, report.failure), ("g1", true, None));
    assert_eq!(*w.link.events.lock().unwrap(), ["fetch up to 6", "upload 10"]);
    assert_eq!(w.pool.busy(), 0, "the slot is free when the bridge sees the result");
    assert_eq!(leftovers(&w), 0, "the bundle and the result zip are removed");
    assert!(w.outboxes.refs().is_empty(), "a generation has no outbox");
}

#[tokio::test]
async fn a_failed_generation_reports_the_failure_and_uploads_nothing() {
    let failure =
        GenerationFailure::new(Stage::Validate, Kind::Rejected, "bad n").of_program("validator").at_test(2);
    let w = world(generating(Err(failure)), false);
    w.pool.generate(plan("g2"), "s1".into()).unwrap();
    let report = finished(&w).await;
    let failure = report.failure.unwrap();
    assert!(!report.ok);
    assert_eq!(failure.stage, v1::generation_failure::Stage::Validate as i32);
    assert_eq!(failure.kind, v1::generation_failure::Kind::Rejected as i32);
    assert_eq!(
        (failure.program.as_str(), failure.test_index, failure.detail.as_str()),
        ("validator", 2, "bad n")
    );
    assert_eq!(*w.link.events.lock().unwrap(), ["fetch up to 6"]);
}

#[tokio::test]
async fn a_failure_detail_is_clipped_to_the_bridges_log_limit() {
    let w = world(generating(Err(GenerationFailure::internal("é".repeat(100_000)))), false);
    w.outboxes.limits().update(0, 1000);
    w.pool.generate(plan("g3"), "s1".into()).unwrap();
    let detail = finished(&w).await.failure.unwrap().detail;
    assert_eq!(detail.len(), 1000);
}

#[tokio::test]
async fn a_bundle_over_the_cap_is_refused_without_fetching() {
    let w = world(generating(Ok(())), false);
    let mut big = plan("g4");
    big.bundle_size = MAX_BUNDLE_BYTES + 1;
    w.pool.generate(big, "s1".into()).unwrap();
    let failure = finished(&w).await.failure.unwrap();
    assert_eq!(failure.stage, v1::generation_failure::Stage::Job as i32);
    assert_eq!(failure.kind, v1::generation_failure::Kind::OutputLimit as i32);
    assert!(w.link.events.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_running_generation_holds_a_slot_and_a_cancel_ends_it_silently() {
    let runner = Generating { wait_for_cancel: true, ..generating(Ok(())) };
    let w = world(runner, false);
    w.pool.generate(plan("g5"), "s1".into()).unwrap();
    assert_eq!(w.pool.busy(), 1);
    assert!(w.pool.generate(plan("g6"), "s1".into()).is_err(), "no free slot");
    assert!(w.pool.generate(plan("g5"), "s1".into()).is_ok(), "the same generation again is ignored");
    w.pool.cancel_generation("g5");
    eventually(|| w.pool.busy() == 0).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(done(&w).is_empty(), "nothing is sent for a cancelled generation");
    assert_eq!(leftovers(&w), 0);
    w.pool.generate(plan("g6"), "s1".into()).unwrap();
}

#[tokio::test]
async fn losing_the_session_cancels_every_generation_and_nothing_reaches_the_new_one() {
    let runner = Generating { wait_for_cancel: true, ..generating(Ok(())) };
    let w = world(runner, false);
    w.pool.generate(plan("g7"), "s1".into()).unwrap();
    w.pool.cancel_generations();
    eventually(|| w.pool.busy() == 0).await;
    assert!(done(&w).is_empty());
}

#[tokio::test]
async fn a_result_for_a_session_that_is_gone_is_dropped_and_the_slot_is_freed() {
    let w = world(generating(Ok(())), false);
    w.pool.generate(plan("g8"), "old-session".into()).unwrap();
    eventually(|| w.link.events.lock().unwrap().len() == 2).await;
    eventually(|| w.pool.busy() == 0).await;
    assert!(done(&w).is_empty());
}

#[tokio::test]
async fn a_fetch_that_outlasts_the_deadline_is_a_job_time_limit() {
    let w = world(generating(Ok(())), true);
    let mut short = plan("g9");
    short.deadline_s = 1;
    w.pool.generate(short, "s1".into()).unwrap();
    let failure = finished(&w).await.failure.unwrap();
    assert_eq!(failure.stage, v1::generation_failure::Stage::Job as i32);
    assert_eq!(failure.kind, v1::generation_failure::Kind::JobTimeLimit as i32);
    assert_eq!(w.pool.busy(), 0);
}

#[tokio::test]
async fn the_grpc_link_streams_the_bundle_down_and_the_result_up_in_the_sessions_name() {
    let (url, bridge) = start().await;
    *bridge.bundle.lock().unwrap() = b"bundle bytes".to_vec();
    let key = tempfile::tempdir().unwrap();
    std::fs::write(key.path().join("k"), "sekret\n").unwrap();
    let config = parse(&format!(
        "name = 'j1'\nbridge_url = '{url}'\nkey_file = '{}'\nproblem_data_root = '/d'\ncache_dir = '/c'\nlanguages_file = '/l'\ntestlib_dir = '/t'\n",
        key.path().join("k").display()
    ))
    .unwrap();
    let channel = tonic::transport::Endpoint::from_shared(url).unwrap().connect().await.unwrap();
    let (outbound, mut queue) = tokio::sync::mpsc::channel(4);
    let connection = Connection { channel, session_id: "s1".into(), outbound };
    let link = GrpcGeneration { config: Arc::new(config), connection: Arc::new(Some(connection).into()) };
    let ticket = Ticket { generation_id: "g1".into(), session_id: "s1".into() };
    let dir = tempfile::tempdir().unwrap();
    let bundle = dir.path().join("bundle.zip");
    link.fetch_bundle(&ticket, 100, &bundle).await.unwrap();
    assert_eq!(std::fs::read(&bundle).unwrap(), b"bundle bytes");
    assert!(link.fetch_bundle(&ticket, 5, &bundle).await.is_err(), "over the cap");
    let result = dir.path().join("result.zip");
    std::fs::write(&result, vec![7u8; (2 << 20) + 5]).unwrap();
    link.upload(&ticket, &result).await.unwrap();
    let uploads = bridge.uploads.lock().unwrap().clone();
    assert_eq!(
        (uploads[0].0.as_str(), uploads[0].1.as_str(), uploads[0].2.len()),
        ("g1", "s1", (2 << 20) + 5)
    );
    link.send(&ticket, judge_daemon::wire::draining()).await.unwrap();
    assert!(queue.recv().await.is_some());
    let stale = Ticket { session_id: "s0".into(), ..ticket };
    assert!(link.fetch_bundle(&stale, 100, &bundle).await.is_err());
    assert!(link.upload(&stale, &result).await.is_err());
    assert!(link.send(&stale, judge_daemon::wire::draining()).await.is_err());
}
