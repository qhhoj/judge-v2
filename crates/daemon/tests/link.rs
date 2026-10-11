mod support;
use judge_daemon::config::parse;
use judge_daemon::link::{LinkContext, SlotPool, SlotsFull, run_link};
use judge_daemon::metrics::Metrics;
use judge_daemon::outbox::{AssignmentRef, Body, OUTBOX_WATERMARK, Outboxes};
use judge_daemon::wire::{Assignment, HelloInfo, seq_of};
use judge_grader::generate::GenerationPlan;
use judge_proto::v1::{self, BridgeMessage, bridge_message, judge_message};
use std::sync::{Arc, Mutex};
use support::fake_bridge::{eventually, start};
use tokio::sync::watch;

#[derive(Default)]
struct FakePool {
    events: Mutex<Vec<String>>,
    full: bool,
}

impl SlotPool for FakePool {
    fn assign(&self, a: Assignment) -> Result<(), SlotsFull> {
        if self.full {
            return Err(SlotsFull);
        }
        self.events.lock().unwrap().push(format!("assign {}", a.r.submission_id));
        Ok(())
    }
    fn abort(&self, r: &AssignmentRef) {
        self.events.lock().unwrap().push(format!("abort {}", r.submission_id));
    }
    fn drop_run(&self, r: &AssignmentRef) {
        self.events.lock().unwrap().push(format!("drop {}", r.submission_id));
    }
    fn generate(&self, plan: GenerationPlan, session_id: String) -> Result<(), SlotsFull> {
        if self.full {
            return Err(SlotsFull);
        }
        self.events.lock().unwrap().push(format!("generate {} in {session_id}", plan.id));
        Ok(())
    }
    fn cancel_generation(&self, generation_id: &str) {
        self.events.lock().unwrap().push(format!("cancel {generation_id}"));
    }
    fn cancel_generations(&self) {
        self.events.lock().unwrap().push("cancel all generations".into());
    }
    fn busy(&self) -> u32 {
        1
    }
    fn slots(&self) -> u32 {
        2
    }
}

struct Harness {
    link: tokio::task::JoinHandle<()>,
    bridge: support::fake_bridge::Shared,
    outboxes: Arc<Outboxes>,
    pool: Arc<FakePool>,
    stop: watch::Sender<bool>,
    drain: watch::Sender<bool>,
    _key: tempfile::TempDir,
}

async fn harness(full: bool) -> Harness {
    let (url, bridge) = start().await;
    let key = tempfile::tempdir().unwrap();
    std::fs::write(key.path().join("k"), "sekret\n").unwrap();
    let config = parse(&format!(
        "name = 'j1'\nbridge_url = '{url}'\nkey_file = '{}'\nproblem_data_root = '/d'\ncache_dir = '/c'\nlanguages_file = '/l'\ntestlib_dir = '/t'\n[reconnect]\ninitial_ms = 20\nmax_ms = 100\n",
        key.path().join("k").display()
    )).unwrap();
    let outboxes = Arc::new(Outboxes::new(OUTBOX_WATERMARK));
    let pool = Arc::new(FakePool { full, ..Default::default() });
    let (stop, stop_rx) = watch::channel(false);
    let (drain, drain_rx) = watch::channel(false);
    let ctx = LinkContext {
        config: Arc::new(config),
        outboxes: outboxes.clone(),
        pool: pool.clone(),
        hello: HelloInfo {
            protocol_minor: judge_proto::PROTOCOL_MINOR,
            build: "test".into(),
            runtimes: vec![],
            features: vec![],
        },
        connection: Default::default(),
        metrics: Arc::new(Metrics::new()),
        draining: drain_rx,
    };
    let link = tokio::spawn(run_link(ctx, stop_rx));
    Harness { link, bridge, outboxes, pool, stop, drain, _key: key }
}

fn welcome(resume: Vec<v1::Resume>) -> BridgeMessage {
    BridgeMessage {
        body: Some(bridge_message::Body::Welcome(v1::Welcome {
            session_id: "s".into(),
            resume,
            ping_interval_ms: 10_000,
            ..Default::default()
        })),
    }
}

fn r(id: &str) -> AssignmentRef {
    AssignmentRef { submission_id: id.into(), generation: 1 }
}
fn pref(id: &str) -> Option<v1::AssignmentRef> {
    Some(v1::AssignmentRef { submission_id: id.into(), generation: 1 })
}

#[tokio::test]
async fn hello_first_with_credentials_then_ping_pong() {
    let h = harness(false).await;
    eventually(|| !h.bridge.messages(0).is_empty()).await;
    let Some(judge_message::Body::Hello(hello)) = &h.bridge.messages(0)[0].body else {
        panic!("hello first")
    };
    assert_eq!((hello.protocol_minor, hello.slots), (judge_proto::PROTOCOL_MINOR, 2));
    assert_eq!(h.bridge.credentials.lock().unwrap()[0], ("j1".to_string(), "sekret".to_string()));
    h.bridge.send(0, welcome(vec![]));
    h.bridge.send(0, BridgeMessage { body: Some(bridge_message::Body::Ping(v1::Ping { nonce: 42 })) });
    eventually(|| {
        h.bridge
            .messages(0)
            .iter()
            .any(|m| matches!(&m.body, Some(judge_message::Body::Pong(p)) if p.nonce == 42 && p.busy == 1))
    })
    .await;
    h.stop.send(true).unwrap();
}

#[tokio::test]
async fn outbox_is_sent_in_order_and_acks_trim_it() {
    let h = harness(false).await;
    eventually(|| h.bridge.session_count() == 1).await;
    h.bridge.send(0, welcome(vec![]));
    h.outboxes.open(&r("a"));
    h.outboxes.push(&r("a"), Body::Accepted);
    h.outboxes.push(&r("a"), Body::Compiled { log: String::new() });
    eventually(|| h.bridge.messages(0).iter().filter_map(seq_of).count() == 2).await;
    let seqs: Vec<u32> = h.bridge.messages(0).iter().filter_map(seq_of).collect();
    assert_eq!(seqs, [1, 2]);
    h.bridge.send(
        0,
        BridgeMessage { body: Some(bridge_message::Body::Ack(v1::ResultAck { r#ref: pref("a"), seq: 2 })) },
    );
    eventually(|| h.outboxes.total_bytes() == 0).await;
}

#[tokio::test]
async fn reconnect_replays_from_the_resume_point() {
    let h = harness(false).await;
    eventually(|| h.bridge.session_count() == 1).await;
    h.bridge.send(0, welcome(vec![]));
    h.outboxes.open(&r("a"));
    for _ in 0..3 {
        h.outboxes.push(&r("a"), Body::Accepted);
    }
    eventually(|| h.bridge.messages(0).iter().filter_map(seq_of).count() == 3).await;
    h.bridge.hang_up(0);
    eventually(|| h.bridge.session_count() == 2 && !h.bridge.messages(1).is_empty()).await;
    let Some(judge_message::Body::Hello(hello)) = &h.bridge.messages(1)[0].body else {
        panic!("hello first")
    };
    assert_eq!(hello.in_flight[0].last_seq_sent, 3);
    h.bridge.send(
        1,
        welcome(vec![v1::Resume {
            r#ref: pref("a"),
            action: v1::resume::Action::Continue as i32,
            from_seq: 2,
        }]),
    );
    eventually(|| h.bridge.messages(1).iter().filter_map(seq_of).count() == 2).await;
    let replayed: Vec<u32> = h.bridge.messages(1).iter().filter_map(seq_of).collect();
    assert_eq!(replayed, [2, 3]);
}

fn graded() -> Body {
    Body::Final { outcome: judge_grader::judge::Outcome::Graded, log: String::new(), data_version_used: 0 }
}

fn finals(messages: &[v1::JudgeMessage], id: &str) -> Vec<(u32, i32)> {
    messages
        .iter()
        .filter_map(|m| match &m.body {
            Some(judge_message::Body::Final(f))
                if f.r#ref.as_ref().is_some_and(|x| x.submission_id == id) =>
            {
                Some((f.seq, f.outcome))
            }
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn a_dropped_ref_ends_with_one_final_aborted_and_leaves_no_outbox() {
    let h = harness(false).await;
    eventually(|| h.bridge.session_count() == 1).await;
    h.outboxes.open(&r("done"));
    h.outboxes.push(&r("done"), Body::Accepted);
    h.outboxes.push(&r("done"), graded());
    h.outboxes.open(&r("live"));
    h.outboxes.push(&r("live"), Body::Accepted);
    h.bridge.hang_up(0);
    eventually(|| h.bridge.session_count() == 2).await;
    h.bridge.send(1, welcome(vec![]));
    eventually(|| h.pool.events.lock().unwrap().contains(&"drop live".to_string())).await;
    // The live run's worker reports once more, then finishes: only its Final goes out, as ABORTED.
    h.outboxes.push(&r("live"), Body::Compiled { log: String::new() });
    h.outboxes.push(&r("live"), graded());
    eventually(|| h.outboxes.refs().is_empty()).await;
    let sent = h.bridge.messages(1);
    assert_eq!(finals(&sent, "done"), [(3, v1::Outcome::Aborted as i32)]);
    assert_eq!(finals(&sent, "live"), [(2, v1::Outcome::Aborted as i32)]);
    assert_eq!(sent.iter().filter_map(seq_of).count(), 2, "nothing but the two Finals");
}

#[tokio::test]
async fn welcome_limits_lower_the_batch_and_the_log_clip() {
    let h = harness(false).await;
    eventually(|| h.bridge.session_count() == 1).await;
    let limits = Some(v1::Limits { max_cases_per_message: 8, max_log_bytes: 5, ..Default::default() });
    h.bridge.send(
        0,
        BridgeMessage {
            body: Some(bridge_message::Body::Welcome(v1::Welcome {
                session_id: "s".into(),
                limits,
                ..Default::default()
            })),
        },
    );
    eventually(|| h.outboxes.limits().batch_cases() == 8).await;
    h.outboxes.open(&r("a"));
    h.outboxes.push(&r("a"), Body::Compiled { log: "0123456789".into() });
    eventually(|| {
        h.bridge
            .messages(0)
            .iter()
            .any(|m| matches!(&m.body, Some(judge_message::Body::Compiled(c)) if c.log == "01234"))
    })
    .await;
}

#[tokio::test]
async fn a_first_reply_that_is_not_welcome_reconnects() {
    let h = harness(false).await;
    eventually(|| h.bridge.session_count() == 1).await;
    h.bridge.send(0, BridgeMessage { body: Some(bridge_message::Body::Ping(v1::Ping { nonce: 1 })) });
    eventually(|| h.bridge.session_count() == 2).await;
}

fn drainings(messages: &[v1::JudgeMessage]) -> usize {
    messages.iter().filter(|m| matches!(m.body, Some(judge_message::Body::Draining(_)))).count()
}

#[tokio::test]
async fn draining_is_announced_once_per_session_and_again_after_a_reconnect() {
    let h = harness(false).await;
    eventually(|| h.bridge.session_count() == 1).await;
    h.bridge.send(0, welcome(vec![]));
    h.drain.send(true).unwrap();
    eventually(|| drainings(&h.bridge.messages(0)) == 1).await;
    h.drain.send(true).unwrap();
    h.bridge.send(0, BridgeMessage { body: Some(bridge_message::Body::Ping(v1::Ping { nonce: 7 })) });
    eventually(|| h.bridge.messages(0).iter().any(|m| matches!(&m.body, Some(judge_message::Body::Pong(_)))))
        .await;
    assert_eq!(drainings(&h.bridge.messages(0)), 1, "Draining goes once");
    h.bridge.hang_up(0);
    eventually(|| h.bridge.session_count() == 2).await;
    h.bridge.send(1, welcome(vec![]));
    eventually(|| drainings(&h.bridge.messages(1)) == 1).await;
}

#[tokio::test]
async fn stop_ends_a_live_session() {
    let h = harness(false).await;
    eventually(|| h.bridge.session_count() == 1).await;
    h.bridge.send(0, welcome(vec![]));
    h.bridge.send(0, BridgeMessage { body: Some(bridge_message::Body::Ping(v1::Ping { nonce: 1 })) });
    eventually(|| h.bridge.messages(0).iter().any(|m| matches!(&m.body, Some(judge_message::Body::Pong(_)))))
        .await;
    h.stop.send(true).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), h.link).await.expect("link stopped").unwrap();
    assert_eq!(h.bridge.session_count(), 1, "no reconnect after stop");
}

fn order_of(messages: &[v1::JudgeMessage]) -> Vec<&'static str> {
    messages
        .iter()
        .filter_map(|m| match m.body {
            Some(judge_message::Body::Draining(_)) => Some("draining"),
            Some(judge_message::Body::Final(_)) => Some("final"),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn draining_goes_before_a_final_that_became_ready_with_it() {
    // Several sessions: without `biased` the select picks a ready branch at random.
    for _ in 0..8 {
        let h = harness(false).await;
        eventually(|| h.bridge.session_count() == 1).await;
        h.bridge.send(0, welcome(vec![]));
        h.bridge.send(0, BridgeMessage { body: Some(bridge_message::Body::Ping(v1::Ping { nonce: 1 })) });
        eventually(|| {
            h.bridge.messages(0).iter().any(|m| matches!(m.body, Some(judge_message::Body::Pong(_))))
        })
        .await;
        // No await between the two: the link sees both ready at its next poll.
        h.outboxes.open(&r("f"));
        h.drain.send(true).unwrap();
        h.outboxes.push(&r("f"), graded());
        eventually(|| order_of(&h.bridge.messages(0)).len() == 2).await;
        assert_eq!(order_of(&h.bridge.messages(0)), ["draining", "final"]);
        h.stop.send(true).unwrap();
    }
}

#[tokio::test]
async fn a_drain_begun_while_disconnected_is_announced_before_the_replay() {
    let h = harness(false).await;
    eventually(|| h.bridge.session_count() == 1).await;
    h.outboxes.open(&r("f"));
    h.outboxes.push(&r("f"), graded());
    h.drain.send(true).unwrap();
    h.bridge.send(0, welcome(vec![]));
    eventually(|| order_of(&h.bridge.messages(0)).len() == 2).await;
    assert_eq!(order_of(&h.bridge.messages(0)), ["draining", "final"]);
}

#[tokio::test]
async fn an_unknown_bridge_message_is_ignored() {
    let h = harness(false).await;
    eventually(|| h.bridge.session_count() == 1).await;
    h.bridge.send(0, welcome(vec![]));
    h.bridge.send(0, BridgeMessage { body: None });
    h.bridge.send(0, BridgeMessage { body: Some(bridge_message::Body::Ping(v1::Ping { nonce: 5 })) });
    let pong = |m: &v1::JudgeMessage| matches!(&m.body, Some(judge_message::Body::Pong(p)) if p.nonce == 5);
    eventually(|| h.bridge.messages(0).iter().any(pong)).await;
    assert_eq!(h.bridge.session_count(), 1, "the session survived");
}

fn generate_message(id: &str) -> BridgeMessage {
    BridgeMessage {
        body: Some(bridge_message::Body::Generate(v1::GenerateTests {
            generation_id: id.into(),
            bundle_size: 10,
            deadline_s: 60,
            ..Default::default()
        })),
    }
}

#[tokio::test]
async fn a_generation_goes_to_the_pool_with_its_session_and_a_cancel_follows() {
    let h = harness(false).await;
    eventually(|| h.bridge.session_count() == 1).await;
    h.bridge.send(0, welcome(vec![]));
    h.bridge.send(0, generate_message("g1"));
    let cancel = v1::CancelGeneration { generation_id: "g1".into() };
    h.bridge.send(0, BridgeMessage { body: Some(bridge_message::Body::CancelGeneration(cancel)) });
    eventually(|| h.pool.events.lock().unwrap().len() == 2).await;
    assert_eq!(*h.pool.events.lock().unwrap(), ["generate g1 in s", "cancel g1"]);
}

#[tokio::test]
async fn a_generation_without_a_free_slot_is_answered_slots_full_and_the_session_end_cancels_generations() {
    let h = harness(true).await;
    eventually(|| h.bridge.session_count() == 1).await;
    h.bridge.send(0, welcome(vec![]));
    h.bridge.send(0, generate_message("g2"));
    eventually(|| {
        h.bridge.messages(0).iter().any(|m| match &m.body {
            Some(judge_message::Body::Error(e)) => {
                e.code == v1::ErrorCode::SlotsFull as i32 && e.r#ref.is_none() && e.message.contains("g2")
            }
            _ => false,
        })
    })
    .await;
    h.bridge.hang_up(0);
    eventually(|| h.pool.events.lock().unwrap().iter().any(|e| e == "cancel all generations")).await;
}
