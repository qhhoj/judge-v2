//! Plan A's judge-role scenarios, played by the real daemon (link, outbox, acks, resume, pool, worker)
//! against plan A's fake bridge. Only grading is replaced: `Replay` reports what the transcript's judge
//! steps say the grading produced (spec §8: a stub sandbox and a scripted fake bridge).
//!
//! Harness adaptations (the scenarios are authoritative and are never edited):
//! - `Hello.protocol_minor`, build, slots and runtimes come from the scenario's first `hello`.
//! - An `assign` without `problemCode` grades the scenario's first problem (`ScenarioResolve`).
//! - A `fetch` step expecting a refusal is a probe the harness makes itself through `GrpcFetch`, after
//!   the daemon's own fetches went through (the daemon only fetches sources it was assigned).
use conformance::expand::{Bindings, expand};
use conformance::fake_bridge::{FakeBridge, Incoming, play_bridge};
use conformance::play::key_of;
use conformance::scenario::{Role, Scenario, Step, load_all, parse_judge, scenarios_dir};
use judge_daemon::config::{Config, parse};
use judge_daemon::gates::Gates;
use judge_daemon::generation::GrpcGeneration;
use judge_daemon::link::{Connection, LinkContext, SlotPool, run_link};
use judge_daemon::metrics::Metrics;
use judge_daemon::outbox::{AssignmentRef, OUTBOX_WATERMARK, Outboxes};
use judge_daemon::pool::Pool;
use judge_daemon::source::{GrpcFetch, SourceFetch};
use judge_daemon::wire::{HelloInfo, JudgeMessage, judge_message, v1};
use judge_daemon::worker::{Resolve, Runner, WorkerDeps};
use judge_grader::judge::{Outcome, RetryReason, Submission};
use judge_grader::report::{CaseReport, Reporter};
use judge_problem::data_version::{DataError, FsStamps, WaitPolicy};
use judge_problem::dir_cache::DirCache;
use judge_problem::resolve::{ResolvedProblem, Resolver};
use judge_problem::verdict::Status;
use judge_proto::v1::judge_link_server::JudgeLinkServer;
use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{RwLock, mpsc, watch};
use tokio_stream::wrappers::TcpListenerStream;

const JUDGE_SCENARIOS: [&str; 12] = [
    "11-grade",
    "12-compile-error",
    "18-resume-continue",
    "20-resume-drop-after-rejudge",
    "21b-rejudge-mid-run-one-slot",
    "22-abort-running",
    "24-abort-in-grace-resume",
    "28b-accept-timeout-one-slot",
    "30-judge-refuses-beyond-slots",
    "32-fetch-source",
    "33-draining",
    "38-generation-refused",
];
const HOLD_LIMIT: Duration = Duration::from_secs(5);
/// The longest the replay waits for a resume or an abort the transcript promises; then the scenario fails.
const WAIT_LIMIT: Duration = Duration::from_secs(10);
const POLL: Duration = Duration::from_millis(5);

/// One thing the grading reports, in transcript order. `Resume`: wait until the link has reconnected and
/// been welcomed again. `Drain`: start the daemon's drain. `Hold`: no Final in the transcript, keep the slot.
#[derive(Clone)]
enum Act {
    Compiled(String),
    Started(bool, u32),
    Cases(Vec<v1::Case>),
    Final(i32, String, i32),
    Resume,
    Drain,
    Hold,
}

fn status(code: i32) -> Status {
    match v1::Status::try_from(code).unwrap_or(v1::Status::Unspecified) {
        v1::Status::Ac => Status::Ac,
        v1::Status::Wa => Status::Wa,
        v1::Status::Tle => Status::Tle,
        v1::Status::Mle => Status::Mle,
        v1::Status::Ole => Status::Ole,
        v1::Status::Rte => Status::Rte,
        v1::Status::Ir => Status::Ir,
        _ => Status::Sc,
    }
}

fn retry_reason(code: i32) -> RetryReason {
    match v1::RetryReason::try_from(code).unwrap_or(v1::RetryReason::Unspecified) {
        v1::RetryReason::DataUnavailable => RetryReason::DataUnavailable,
        v1::RetryReason::RuntimeMissing => RetryReason::RuntimeMissing,
        v1::RetryReason::Unsupported => RetryReason::Unsupported,
        _ => RetryReason::SandboxFailure,
    }
}

fn outcome(code: i32, log: &str, reason: i32) -> Outcome {
    match v1::Outcome::try_from(code).unwrap_or(v1::Outcome::Unspecified) {
        v1::Outcome::CompileError => Outcome::CompileError(log.into()),
        v1::Outcome::InternalError => Outcome::InternalError(log.into()),
        v1::Outcome::Aborted => Outcome::Aborted,
        v1::Outcome::Retry => Outcome::Retry(retry_reason(reason)),
        _ => Outcome::Graded,
    }
}

fn key(r: Option<v1::AssignmentRef>) -> String {
    r.map(|r| format!("{}#{}", r.submission_id, r.generation)).unwrap_or_default()
}

/// ("id#generation", act) of a per-assignment judge message.
fn act_of(body: judge_message::Body) -> Option<(String, Option<Act>)> {
    use judge_message::Body as J;
    Some(match body {
        J::Accepted(m) => (key(m.r#ref), None),
        J::Compiled(m) => (key(m.r#ref), Some(Act::Compiled(m.log))),
        J::Started(m) => (key(m.r#ref), Some(Act::Started(m.pretested, m.case_count))),
        J::Cases(m) => (key(m.r#ref), Some(Act::Cases(m.cases))),
        J::Final(m) => (key(m.r#ref), Some(Act::Final(m.outcome, m.log, m.reason))),
        _ => return None,
    })
}

/// Per accepted assignment (acceptance order) its acts. An assignment the transcript never accepts
/// never reaches the runner, so it gets no script.
#[derive(Default)]
struct Scripts {
    order: Vec<String>,
    acts: HashMap<String, Vec<Act>>,
    hello: Option<v1::Hello>,
}

impl Scripts {
    fn of(scenario: &Scenario) -> Self {
        let (bindings, mut scripts) = (scenario.sample_bindings(), Scripts::default());
        for step in &scenario.steps {
            match step {
                Step::Drop { .. } => scripts.add_to_unfinished(Act::Resume),
                Step::Judge { message, .. } => {
                    if let Ok(JudgeMessage { body: Some(body) }) = parse_judge(message, &bindings) {
                        scripts.judge(body);
                    }
                }
                _ => {}
            }
        }
        scripts.add_to_unfinished(Act::Hold);
        scripts
    }

    /// Adds `act` to every accepted assignment that has no Final yet.
    fn add_to_unfinished(&mut self, act: Act) {
        let unfinished = |acts: &&mut Vec<Act>| !acts.iter().any(|x| matches!(x, Act::Final(..)));
        self.acts.values_mut().filter(unfinished).for_each(|acts| acts.push(act.clone()));
    }

    fn judge(&mut self, body: judge_message::Body) {
        match body {
            judge_message::Body::Hello(hello) => drop(self.hello.get_or_insert(hello)),
            judge_message::Body::Draining(_) => self.add_to_unfinished(Act::Drain),
            body => {
                let accepted = matches!(body, judge_message::Body::Accepted(_));
                let Some((id, act)) = act_of(body) else { return };
                if accepted && !self.acts.contains_key(&id) {
                    self.order.push(id.clone());
                    self.acts.insert(id.clone(), Vec::new());
                }
                if let Some(list) = self.acts.get_mut(&id) {
                    list.extend(act);
                }
            }
        }
    }

    fn queue(mut self) -> VecDeque<Vec<Act>> {
        self.order.iter().map(|id| self.acts.remove(id).unwrap_or_default()).collect()
    }
}

/// Replays the transcript's grading, one script per run in acceptance order.
struct Replay {
    queue: Arc<Mutex<VecDeque<Vec<Act>>>>,
    runs: Arc<AtomicUsize>,
    reconnects: Arc<Metrics>,
    connection: Arc<RwLock<Option<Connection>>>,
    drain: Arc<watch::Sender<bool>>,
    /// Waits that ran out (a daemon that never resumed or never aborted).
    failures: Arc<Mutex<Vec<String>>>,
}

/// Waits in `before_case` until the abort arrives.
/// True when the abort came before `limit`.
fn wait_for_abort(reporter: &mut dyn Reporter, limit: Duration) -> bool {
    let started = std::time::Instant::now();
    while started.elapsed() < limit {
        if !reporter.before_case() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    false
}

fn report_case(reporter: &mut dyn Reporter, c: &v1::Case) {
    reporter.case(CaseReport {
        position: c.position,
        status: status(c.status),
        time_s: c.time_s,
        jury_time_s: c.jury_time_s,
        memory_kib: c.memory_kib,
        points: c.points,
        total: c.total,
        batch: (c.batch > 0).then_some(c.batch),
        feedback: c.feedback.clone(),
        extended_feedback: c.extended_feedback.clone(),
        output_prefix: c.output_prefix.clone(),
    });
}

impl Replay {
    /// True once the link has reconnected and been welcomed again, within `WAIT_LIMIT`.
    fn wait_for_resume(&self, reconnects_before: u64) -> bool {
        let welcomed = || self.connection.try_read().map(|c| c.is_some()).unwrap_or(false);
        let started = std::time::Instant::now();
        while self.reconnects.reconnects.get() <= reconnects_before || !welcomed() {
            if started.elapsed() >= WAIT_LIMIT {
                return false;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        true
    }

    /// Records a wait the transcript promised but the daemon never ended, and ends the run.
    fn gave_up(&self, what: &str) -> Option<Outcome> {
        let message = format!("replay waited {} s for {what}", WAIT_LIMIT.as_secs());
        self.failures.lock().unwrap().push(message.clone());
        Some(Outcome::InternalError(message))
    }

    /// Plays one act; Some ends the run with that outcome.
    fn play(&self, act: Act, reporter: &mut dyn Reporter, reconnects_before: u64) -> Option<Outcome> {
        match act {
            Act::Compiled(log) => reporter.compiled(&log),
            Act::Started(pretested, count) => reporter.started(pretested, count),
            Act::Cases(cases) => {
                for c in &cases {
                    if !reporter.before_case() {
                        return Some(Outcome::Aborted);
                    }
                    report_case(reporter, c);
                }
            }
            Act::Resume if !self.wait_for_resume(reconnects_before) => return self.gave_up("the resume"),
            Act::Resume => {}
            Act::Drain => drop(self.drain.send(true)),
            Act::Hold => {
                wait_for_abort(reporter, HOLD_LIMIT);
                return Some(Outcome::Aborted);
            }
            Act::Final(code, _, _) if code == v1::Outcome::Aborted as i32 => {
                if !wait_for_abort(reporter, WAIT_LIMIT) {
                    return self.gave_up("an abort");
                }
                return Some(Outcome::Aborted);
            }
            Act::Final(code, log, reason) => return Some(outcome(code, &log, reason)),
        }
        None
    }
}

impl Runner for Replay {
    fn run(&mut self, _p: &ResolvedProblem, _s: &Submission, reporter: &mut dyn Reporter) -> Outcome {
        self.runs.fetch_add(1, Ordering::SeqCst);
        let acts = self.queue.lock().unwrap().pop_front().unwrap_or_default();
        let reconnects_before = self.reconnects.reconnects.get();
        acts.into_iter()
            .find_map(|act| self.play(act, reporter, reconnects_before))
            .unwrap_or(Outcome::Graded)
    }
}

/// An `assign` without `problemCode` grades the scenario's first problem.
struct ScenarioResolve {
    inner: Resolver,
    default_code: String,
}

impl Resolve for ScenarioResolve {
    fn resolve(&self, code: &str, assigned: u64) -> Result<ResolvedProblem, DataError> {
        let code = if code.is_empty() { &self.default_code } else { code };
        self.inner.resolve(code, assigned)
    }
}

/// Each problem of the setup: a one-case init.yml and its `dataVersion` as `.data-version`.
fn data_root(scenario: &Scenario) -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    for problem in &scenario.setup.problems {
        let dir = root.path().join(&problem.code);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("init.yml"), "test_cases:\n- {in: 1.in, out: 1.out, points: 1}\n").unwrap();
        std::fs::write(dir.join(".data-version"), format!("{}\n", problem.data_version)).unwrap();
    }
    root
}

/// `28b`: `hold-ack` exists until generation 1's `Final{ABORTED}` has been sent, which the next Assign
/// (another ref) proves. Generation 1 may come and go between two polls: abort and assign arrive together.
fn hold_ack_until_aborted(gates: &Path, outboxes: Arc<Outboxes>, r: AssignmentRef) {
    let hold = gates.join("hold-ack");
    std::fs::write(&hold, "").unwrap();
    tokio::spawn(async move {
        while !outboxes.refs().iter().any(|other| other != &r) {
            tokio::time::sleep(POLL).await;
        }
        let _ = std::fs::remove_file(hold);
    });
}

/// Refs of the `fetch` steps that expect a refusal, and how many fetches succeed before the first one.
fn probes(scenario: &Scenario, bindings: &Bindings) -> (Vec<AssignmentRef>, usize) {
    let (mut refs, mut ok_before) = (Vec::new(), 0);
    for step in &scenario.steps {
        let Step::Fetch { reference, expect, .. } = step else { continue };
        if expect == "OK" {
            ok_before += usize::from(refs.is_empty());
            continue;
        }
        let r: v1::AssignmentRef = serde_json::from_value(expand(reference, bindings).unwrap()).unwrap();
        refs.push(AssignmentRef { submission_id: r.submission_id, generation: r.generation });
    }
    (refs, ok_before)
}

/// The probes go once `runs` shows the daemon's own fetches are done (a run starts after its fetch).
fn spawn_probes(
    fetch: Arc<GrpcFetch>,
    scenario: &Scenario,
    runs: Arc<AtomicUsize>,
) -> mpsc::Receiver<String> {
    let (refs, ok_before) = probes(scenario, &scenario.sample_bindings());
    let (report, errors) = mpsc::channel(8);
    let dir = tempfile::tempdir().unwrap();
    tokio::spawn(async move {
        while runs.load(Ordering::SeqCst) < ok_before {
            tokio::time::sleep(POLL).await;
        }
        for r in refs {
            if fetch.fetch(&r, 1 << 20, &dir.path().join("probe")).await.is_ok() {
                let _ = report.send(format!("FetchSource of {r:?} was answered")).await;
            }
        }
    });
    errors
}

/// The daemon under test and what its scenario needs while it plays.
struct Judge {
    link: tokio::task::JoinHandle<()>,
    stop: watch::Sender<bool>,
    probe_errors: mpsc::Receiver<String>,
    replay_failures: Arc<Mutex<Vec<String>>>,
    _dirs: Vec<tempfile::TempDir>,
}

fn judge_config(scenario: &Scenario, url: &str, dirs: &[tempfile::TempDir]) -> Result<Arc<Config>, String> {
    let judge = scenario.setup.judges.first().ok_or("scenario has no judge")?.name.clone();
    std::fs::write(dirs[0].path().join("key"), key_of(&judge)).map_err(|e| e.to_string())?;
    let text = format!(
        "name = '{judge}'\nbridge_url = '{url}'\nkey_file = '{}'\nproblem_data_root = '{}'\ncache_dir = '/c'\n\
         languages_file = '/l'\ntestlib_dir = '/t'\n[reconnect]\ninitial_ms = 20\nmax_ms = 100\n",
        dirs[0].path().join("key").display(),
        dirs[1].path().display()
    );
    parse(&text).map(Arc::new).map_err(|e| e.to_string())
}

fn resolver(scenario: &Scenario, root: &Path, cache: &Path) -> ScenarioResolve {
    let inner = Resolver {
        root: root.into(),
        cache: Arc::new(DirCache::new(cache.into(), 1 << 20)),
        wait: WaitPolicy { timeout: Duration::ZERO, poll: Duration::from_millis(10) },
        zip: Default::default(),
        stamps: Arc::new(FsStamps),
    };
    let default_code = scenario.setup.problems.first().map(|p| p.code.clone()).unwrap_or_default();
    ScenarioResolve { inner, default_code }
}

fn hello_info(hello: &v1::Hello) -> HelloInfo {
    let runtimes = hello.runtimes.iter().map(|r| (r.language_key.clone(), r.name.clone(), r.version.clone()));
    HelloInfo {
        protocol_minor: hello.protocol_minor,
        build: hello.build.clone(),
        runtimes: runtimes.collect(),
        features: hello.features.clone(),
    }
}

/// Link context pieces shared by the runners, the probes and the link.
struct Wiring {
    config: Arc<Config>,
    outboxes: Arc<Outboxes>,
    metrics: Arc<Metrics>,
    connection: Arc<RwLock<Option<Connection>>>,
}

fn pool(
    scenario: &Scenario,
    w: &Wiring,
    dirs: &[tempfile::TempDir],
    replay: &dyn Fn() -> Replay,
) -> Arc<Pool> {
    let slots = Scripts::of(scenario).hello.map_or(1, |h| h.slots);
    let runners = (0..slots).map(|_| Box::new(replay()) as Box<dyn Runner>).collect();
    let deps = WorkerDeps {
        judge: w.config.name.clone(),
        outboxes: w.outboxes.clone(),
        resolver: Arc::new(resolver(scenario, dirs[1].path(), dirs[2].path())),
        fetch: Arc::new(GrpcFetch { config: w.config.clone(), connection: w.connection.clone() }),
        generation: Arc::new(GrpcGeneration { config: w.config.clone(), connection: w.connection.clone() }),
        gates: Gates { dir: Some(dirs[3].path().into()) },
        metrics: Arc::new(Metrics::new()),
        scratch: dirs[4].path().into(),
        fetch_timeout: judge_daemon::worker::FETCH_TIMEOUT,
    };
    Pool::start(Arc::new(deps), runners)
}

/// Temp dirs: 0 key, 1 data root, 2 cache, 3 gates, 4 scratch.
fn judge_dirs(scenario: &Scenario) -> Vec<tempfile::TempDir> {
    let fresh = || tempfile::tempdir().unwrap();
    vec![fresh(), data_root(scenario), fresh(), fresh(), fresh()]
}

fn start_judge(scenario: &Scenario, url: &str) -> Result<Judge, String> {
    let scripts = Scripts::of(scenario);
    let hello = scripts.hello.clone().ok_or("scenario has no hello")?;
    let dirs = judge_dirs(scenario);
    let w = Wiring {
        config: judge_config(scenario, url, &dirs)?,
        outboxes: Arc::new(Outboxes::new(OUTBOX_WATERMARK)),
        metrics: Arc::new(Metrics::new()),
        connection: Arc::default(),
    };
    let (drain, draining) = watch::channel(false);
    let (queue, runs, drain) =
        (Arc::new(Mutex::new(scripts.queue())), Arc::new(AtomicUsize::new(0)), Arc::new(drain));
    let replay_failures = Arc::new(Mutex::new(Vec::new()));
    let replay = || Replay {
        failures: replay_failures.clone(),
        queue: queue.clone(),
        runs: runs.clone(),
        reconnects: w.metrics.clone(),
        connection: w.connection.clone(),
        drain: drain.clone(),
    };
    let pool = pool(scenario, &w, &dirs, &replay);
    if scenario.name.starts_with("28b-") {
        let first = AssignmentRef { submission_id: scenario.sample_bindings()["s1"].clone(), generation: 1 };
        hold_ack_until_aborted(dirs[3].path(), w.outboxes.clone(), first);
    }
    let fetch = Arc::new(GrpcFetch { config: w.config.clone(), connection: w.connection.clone() });
    let probe_errors = spawn_probes(fetch, scenario, runs);
    let (stop, stop_rx) = watch::channel(false);
    let ctx = LinkContext {
        config: w.config,
        outboxes: w.outboxes,
        pool: pool as Arc<dyn SlotPool>,
        hello: hello_info(&hello),
        connection: w.connection,
        metrics: w.metrics,
        draining,
    };
    let link = tokio::spawn(run_link(ctx, stop_rx));
    Ok(Judge { link, stop, probe_errors, replay_failures, _dirs: dirs })
}

/// Plan A's fake bridge on a loopback port: its URL, what it hears, and the server task.
async fn start_bridge(
    scenario: &Scenario,
) -> Result<(String, mpsc::UnboundedReceiver<Incoming>, tokio::task::JoinHandle<()>), String> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.map_err(|e| e.to_string())?;
    let url = format!("http://{}", listener.local_addr().map_err(|e| e.to_string())?);
    let (service, incoming) = FakeBridge::new(scenario);
    let serve = tonic::transport::Server::builder()
        .add_service(JudgeLinkServer::new(service))
        .serve_with_incoming(TcpListenerStream::new(listener));
    let server = tokio::spawn(async move { drop(serve.await) });
    Ok((url, incoming, server))
}

async fn play(scenario: &Scenario) -> Result<(), String> {
    let (url, incoming, server) = start_bridge(scenario).await?;
    let mut judge = start_judge(scenario, &url)?;
    let played = play_bridge(scenario, &scenario.sample_bindings(), incoming).await.map(drop);
    let _ = judge.stop.send(true);
    let _ = judge.link.await;
    server.abort();
    let replay_failures = judge.replay_failures.lock().unwrap().join("; ");
    played.map_err(|error| format!("{error} {replay_failures}"))?;
    if !replay_failures.is_empty() {
        return Err(replay_failures);
    }
    match judge.probe_errors.try_recv() {
        Ok(error) => Err(error),
        Err(_) => Ok(()),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_judge_scenario_passes_against_the_daemon() {
    let scenarios = load_all(&scenarios_dir()).unwrap();
    let judge_side: Vec<&Scenario> =
        scenarios.iter().filter(|s| s.applies_to.contains(&Role::Judge)).collect();
    let names: Vec<&str> = judge_side.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, JUDGE_SCENARIOS, "plan A's judge-role set changed");
    let mut failures = Vec::new();
    for scenario in judge_side {
        if let Err(error) = play(scenario).await {
            failures.push(format!("{}: {error}", scenario.name));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
