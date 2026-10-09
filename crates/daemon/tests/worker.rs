use judge_daemon::gates::Gates;
use judge_daemon::link::SlotPool;
use judge_daemon::metrics::Metrics;
use judge_daemon::outbox::{AssignmentRef, Body, OUTBOX_WATERMARK, Outboxes};
use judge_daemon::pool::Pool;
use judge_daemon::source::SourceFetch;
use judge_daemon::wire::Assignment;
use judge_daemon::worker::{Runner, WorkerDeps};
use judge_grader::judge::{Outcome, RetryReason, Submission, SubmissionSource};
use judge_grader::report::{CaseReport, Reporter};
use judge_problem::data_version::{FsStamps, WaitPolicy};
use judge_problem::dir_cache::DirCache;
use judge_problem::resolve::{ResolvedProblem, Resolver};
use judge_problem::verdict::Status;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Reports `cases` AC cases, waiting `pause` before each; records what it was given.
struct Scripted {
    cases: u32,
    pause: Duration,
    seen: Arc<Mutex<Vec<String>>>,
}

impl Runner for Scripted {
    fn run(
        &mut self,
        problem: &ResolvedProblem,
        submission: &Submission,
        reporter: &mut dyn Reporter,
    ) -> Outcome {
        let source = match &submission.source {
            SubmissionSource::Inline(b) => String::from_utf8_lossy(b).into_owned(),
            SubmissionSource::File(p) => std::fs::read_to_string(p).unwrap(),
        };
        assert_ne!(source, "panic", "the runner panics on this source");
        self.seen
            .lock()
            .unwrap()
            .push(format!("{} v{} {source}", submission.language_key, problem.data_version_used));
        reporter.compiled("");
        reporter.started(false, self.cases);
        for position in 1..=self.cases {
            std::thread::sleep(self.pause);
            if !reporter.before_case() && source != "stubborn" {
                return Outcome::Aborted;
            }
            reporter.case(CaseReport {
                position,
                status: Status::Ac,
                time_s: 0.0,
                memory_kib: 0,
                points: 1.0,
                total: 1.0,
                batch: None,
                feedback: String::new(),
                extended_feedback: String::new(),
                output_prefix: String::new(),
            });
        }
        Outcome::Graded
    }
}

struct FixedFetch;
#[tonic::async_trait]
impl SourceFetch for FixedFetch {
    async fn fetch(&self, _r: &AssignmentRef, max: u64, dest: &Path) -> Result<(), String> {
        if max < 9 {
            return Err("larger than announced".into());
        }
        std::fs::write(dest, "from-file").map_err(|e| e.to_string())
    }
}

struct World {
    pool: Arc<Pool>,
    outboxes: Arc<Outboxes>,
    seen: Arc<Mutex<Vec<String>>>,
    metrics: Arc<Metrics>,
    gates_dir: tempfile::TempDir,
    _dirs: Vec<tempfile::TempDir>,
}

fn world(slots: usize, cases: u32, pause: Duration) -> World {
    world_with(slots, cases, pause, OUTBOX_WATERMARK)
}

fn world_with(slots: usize, cases: u32, pause: Duration, watermark: usize) -> World {
    let dirs: Vec<_> = (0..3).map(|_| tempfile::tempdir().unwrap()).collect();
    let problem = dirs[0].path().join("p");
    std::fs::create_dir_all(&problem).unwrap();
    std::fs::write(problem.join("init.yml"), "test_cases:\n- {in: 1.in, out: 1.out, points: 1}\n").unwrap();
    std::fs::write(problem.join(".data-version"), "5\n").unwrap();
    let gates_dir = tempfile::tempdir().unwrap();
    let outboxes = Arc::new(Outboxes::new(watermark));
    let metrics = Arc::new(Metrics::new());
    let resolver = Resolver {
        root: dirs[0].path().into(),
        cache: Arc::new(DirCache::new(dirs[1].path().into(), 1 << 20)),
        wait: WaitPolicy { timeout: Duration::from_millis(100), poll: Duration::from_millis(10) },
        zip: Default::default(),
        stamps: Arc::new(FsStamps),
    };
    let deps = WorkerDeps {
        judge: "j1".into(),
        outboxes: outboxes.clone(),
        resolver: Arc::new(resolver),
        fetch: Arc::new(FixedFetch),
        gates: Gates { dir: Some(gates_dir.path().into()) },
        metrics: metrics.clone(),
        scratch: dirs[2].path().into(),
    };
    let seen = Arc::new(Mutex::new(Vec::new()));
    let runners: Vec<Box<dyn Runner>> = (0..slots)
        .map(|_| Box::new(Scripted { cases, pause, seen: seen.clone() }) as Box<dyn Runner>)
        .collect();
    World { pool: Pool::start(Arc::new(deps), runners), outboxes, seen, metrics, gates_dir, _dirs: dirs }
}

fn assignment(id: &str, code: &str, version: u64) -> Assignment {
    Assignment {
        r: AssignmentRef { submission_id: id.into(), generation: 1 },
        problem_code: code.into(),
        data_version: version,
        language_key: "PY3".into(),
        source: b"inline".to_vec(),
        source_is_file: false,
        source_size: 6,
        time_limit_s: 1.0,
        memory_kib: 1024,
        short_circuit: false,
        pretests_only: false,
    }
}

fn bodies(w: &World, id: &str) -> Vec<Body> {
    w.outboxes
        .pending_after(&AssignmentRef { submission_id: id.into(), generation: 1 }, 0)
        .into_iter()
        .map(|e| e.body)
        .collect()
}

async fn final_of(w: &World, id: &str) -> Body {
    for _ in 0..300 {
        if let Some(last @ Body::Final { .. }) = bodies(w, id).last().cloned() {
            return last;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("no Final for {id}: {:?}", bodies(w, id));
}

#[tokio::test]
async fn messages_come_in_lifecycle_order_and_the_slot_frees() {
    let w = world(1, 3, Duration::ZERO);
    w.pool.assign(assignment("a", "p", 5)).unwrap();
    assert!(matches!(
        final_of(&w, "a").await,
        Body::Final { outcome: Outcome::Graded, data_version_used: 5, .. }
    ));
    let all = bodies(&w, "a");
    assert_eq!(all[0], Body::Accepted);
    assert!(matches!(all[1], Body::Compiled { .. }));
    assert!(matches!(all[2], Body::Started { case_count: 3, .. }));
    let cases: usize = all.iter().map(|b| if let Body::Cases(c) = b { c.len() } else { 0 }).sum();
    assert_eq!(cases, 3);
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(w.pool.busy(), 0);
}

#[tokio::test]
async fn a_full_pool_refuses() {
    let w = world(1, 50, Duration::from_millis(20));
    w.pool.assign(assignment("a", "p", 5)).unwrap();
    assert!(w.pool.assign(assignment("b", "p", 5)).is_err());
    w.pool.abort(&AssignmentRef { submission_id: "a".into(), generation: 1 });
    final_of(&w, "a").await;
}

#[tokio::test]
async fn the_slot_is_free_once_the_final_is_visible() {
    let w = world(1, 1, Duration::ZERO);
    for id in ["first", "second", "third"] {
        w.pool.assign(assignment(id, "p", 5)).unwrap();
        final_of(&w, id).await;
    }
}

#[tokio::test]
async fn accepts_an_assign_that_crossed_draining() {
    let w = world(2, 1, Duration::ZERO);
    w.pool.set_draining();
    w.pool.assign(assignment("crossed", "p", 5)).unwrap();
    assert!(matches!(final_of(&w, "crossed").await, Body::Final { outcome: Outcome::Graded, .. }));
}

#[tokio::test]
async fn abort_and_drop_both_end_with_aborted() {
    let w = world(2, 100, Duration::from_millis(20));
    w.pool.assign(assignment("a", "p", 5)).unwrap();
    w.pool.assign(assignment("d", "p", 5)).unwrap();
    tokio::time::sleep(Duration::from_millis(60)).await;
    w.pool.abort(&AssignmentRef { submission_id: "a".into(), generation: 1 });
    assert!(matches!(final_of(&w, "a").await, Body::Final { outcome: Outcome::Aborted, .. }));
    w.pool.drop_run(&AssignmentRef { submission_id: "d".into(), generation: 1 });
    assert!(matches!(final_of(&w, "d").await, Body::Final { outcome: Outcome::Aborted, .. }));
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(w.pool.busy(), 0);
}

#[tokio::test]
async fn an_abort_before_accepted_sends_only_final_aborted_at_seq_1() {
    let w = world(1, 1, Duration::ZERO);
    let hold = w.gates_dir.path().join("hold-ack");
    std::fs::write(&hold, "").unwrap();
    w.pool.assign(assignment("early", "p", 5)).unwrap();
    assert_eq!(
        w.outboxes.in_flight(),
        [(AssignmentRef { submission_id: "early".into(), generation: 1 }, 0)],
        "the outbox opens at assign"
    );
    w.pool.abort(&AssignmentRef { submission_id: "early".into(), generation: 1 });
    std::fs::remove_file(&hold).unwrap();
    final_of(&w, "early").await;
    let all = w.outboxes.pending_after(&AssignmentRef { submission_id: "early".into(), generation: 1 }, 0);
    assert_eq!(all.len(), 1, "no Accepted: {all:?}");
    assert_eq!(all[0].seq, 1);
    assert!(matches!(all[0].body, Body::Final { outcome: Outcome::Aborted, .. }));
    assert!(w.seen.lock().unwrap().is_empty(), "the runner never started");
}

#[tokio::test]
async fn lagging_data_is_retry_and_a_bad_code_is_internal_error() {
    let w = world(1, 1, Duration::ZERO);
    w.pool.assign(assignment("late", "p", 9)).unwrap();
    assert!(matches!(
        final_of(&w, "late").await,
        Body::Final { outcome: Outcome::Retry(RetryReason::DataUnavailable), .. }
    ));
    tokio::time::sleep(Duration::from_millis(50)).await;
    w.pool.assign(assignment("bad", "../etc", 0)).unwrap();
    assert!(matches!(final_of(&w, "bad").await, Body::Final { outcome: Outcome::InternalError(_), .. }));
}

#[tokio::test]
async fn file_sources_are_fetched() {
    let w = world(1, 1, Duration::ZERO);
    let mut a = assignment("f", "p", 5);
    (a.source, a.source_is_file, a.source_size) = (Vec::new(), true, 9);
    w.pool.assign(a).unwrap();
    final_of(&w, "f").await;
    assert_eq!(w.seen.lock().unwrap()[0], "PY3 v5 from-file");
}

#[tokio::test]
async fn hold_ack_delays_accepted_until_released() {
    let w = world(1, 1, Duration::ZERO);
    let hold = w.gates_dir.path().join("hold-ack");
    std::fs::write(&hold, "").unwrap();
    w.pool.assign(assignment("g", "p", 5)).unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(bodies(&w, "g").is_empty(), "nothing before the gate opens");
    std::fs::remove_file(&hold).unwrap();
    assert!(matches!(final_of(&w, "g").await, Body::Final { outcome: Outcome::Graded, .. }));
}

#[tokio::test]
async fn a_panicking_runner_is_an_internal_error_and_the_slot_keeps_working() {
    let w = world(1, 1, Duration::ZERO);
    let mut a = assignment("boom", "p", 5);
    a.source = b"panic".to_vec();
    w.pool.assign(a).unwrap();
    assert!(matches!(final_of(&w, "boom").await, Body::Final { outcome: Outcome::InternalError(_), .. }));
    w.pool.assign(assignment("after", "p", 5)).unwrap();
    assert!(matches!(final_of(&w, "after").await, Body::Final { outcome: Outcome::Graded, .. }));
}

#[tokio::test]
async fn compile_time_is_observed_per_language() {
    let w = world(1, 1, Duration::ZERO);
    w.pool.assign(assignment("c", "p", 5)).unwrap();
    final_of(&w, "c").await;
    let text = w.metrics.render();
    assert!(text.contains("judge_compile_seconds_count{lang=\"PY3\"} 1"), "{text}");
    assert!(text.contains("judge_assignments_total{outcome=\"graded\"} 1"), "{text}");
}

#[tokio::test]
async fn grading_pauses_above_the_watermark_until_acks_free_it() {
    let w = world_with(1, 2, Duration::ZERO, 50); // Accepted alone (64 bytes) is over it
    let r = AssignmentRef { submission_id: "w".into(), generation: 1 };
    w.pool.assign(assignment("w", "p", 5)).unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(w.outboxes.over_watermark());
    assert!(!bodies(&w, "w").iter().any(|b| matches!(b, Body::Cases(_))), "no case while over the watermark");
    for _ in 0..300 {
        if bodies(&w, "w").iter().any(|b| matches!(b, Body::Final { .. })) {
            return;
        }
        w.outboxes.ack(&r, w.outboxes.last_seq(&r));
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("acks never let grading go on");
}

#[tokio::test]
async fn the_fetched_source_is_removed_after_the_run() {
    let w = world(1, 1, Duration::ZERO);
    let mut a = assignment("tmp", "p", 5);
    (a.source, a.source_is_file, a.source_size) = (Vec::new(), true, 9);
    w.pool.assign(a).unwrap();
    final_of(&w, "tmp").await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(std::fs::read_dir(w._dirs[2].path()).unwrap().count(), 0);
}

#[tokio::test]
async fn an_aborted_job_ends_aborted_whatever_the_runner_returns() {
    let w = world(1, 5, Duration::from_millis(30));
    let mut a = assignment("s", "p", 5);
    a.source = b"stubborn".to_vec();
    w.pool.assign(a).unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    w.pool.abort(&AssignmentRef { submission_id: "s".into(), generation: 1 });
    assert!(matches!(final_of(&w, "s").await, Body::Final { outcome: Outcome::Aborted, .. }));
}

#[tokio::test]
async fn an_abort_releases_a_held_gate() {
    let w = world(1, 1, Duration::ZERO);
    std::fs::write(w.gates_dir.path().join("hold-ack"), "").unwrap();
    w.pool.assign(assignment("held", "p", 5)).unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    w.pool.abort(&AssignmentRef { submission_id: "held".into(), generation: 1 });
    assert!(matches!(final_of(&w, "held").await, Body::Final { outcome: Outcome::Aborted, .. }));
}

#[tokio::test]
async fn a_file_source_without_an_announced_size_is_capped_only_by_the_limit() {
    let w = world(1, 1, Duration::ZERO);
    let mut a = assignment("unsized", "p", 5);
    (a.source, a.source_is_file, a.source_size) = (Vec::new(), true, 0);
    w.pool.assign(a).unwrap();
    assert!(matches!(final_of(&w, "unsized").await, Body::Final { outcome: Outcome::Graded, .. }));
    let mut small = assignment("small", "p", 5);
    (small.source, small.source_is_file, small.source_size) = (Vec::new(), true, 3);
    w.pool.assign(small).unwrap();
    let retry = Outcome::Retry(RetryReason::DataUnavailable);
    assert!(matches!(final_of(&w, "small").await, Body::Final { outcome, .. } if outcome == retry));
}

#[tokio::test]
async fn a_repeated_assign_for_a_held_ref_starts_no_second_run() {
    let w = world(2, 20, Duration::from_millis(10));
    w.pool.assign(assignment("twice", "p", 5)).unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    w.pool.assign(assignment("twice", "p", 5)).unwrap();
    assert_eq!(w.pool.busy(), 1, "no second slot taken");
    final_of(&w, "twice").await;
    let accepted = bodies(&w, "twice").iter().filter(|b| matches!(b, Body::Accepted)).count();
    assert_eq!(accepted, 1);
    assert_eq!(w.seen.lock().unwrap().len(), 1);
}
