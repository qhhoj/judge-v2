#![cfg(feature = "isolate")]
//! The whole daemon (`run_until`) against the small fake bridge: real isolate boxes, a real C++ grade,
//! runtime detection, and the drain on shutdown.
mod support;

use judge_daemon::config::parse;
use judge_daemon::run::run_until;
use judge_proto::v1::{self, BridgeMessage, JudgeMessage, bridge_message, judge_message};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use support::fake_bridge::{Shared, eventually, start};

const SUM: &str = "#include <cstdio>\nint main(){long long a,b;std::scanf(\"%lld %lld\",&a,&b);std::printf(\"%lld\\n\",a+b);}\n";

fn box_base() -> u32 {
    let base: u32 = std::env::var("JUDGE_TEST_BOX_BASE").ok().and_then(|v| v.parse().ok()).unwrap_or(500);
    base + 30
}

fn problem(root: &Path) {
    let dir = root.join("sum");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("init.yml"), "test_cases:\n- {in: 1.in, out: 1.out, points: 1}\n").unwrap();
    std::fs::write(dir.join("1.in"), "1 2\n").unwrap();
    std::fs::write(dir.join("1.out"), "3\n").unwrap();
    std::fs::write(dir.join(".data-version"), "4\n").unwrap();
}

fn config_text(url: &str, dirs: &[tempfile::TempDir]) -> String {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    std::fs::write(dirs[0].path().join("key"), "k\n").unwrap();
    // the cache dir is the judge user's alone, as systemd makes it (`config::check_cache_dir_private`)
    std::fs::set_permissions(dirs[2].path(), std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
    format!(
        "name = 'j1'\nbridge_url = '{url}'\nkey_file = '{}'\nproblem_data_root = '{}'\ncache_dir = '{}'\n\
         languages_file = '{}'\ntestlib_dir = '{}'\nslots = 1\ncores = [0]\nbox_base = {}\n\
         metrics_listen = '127.0.0.1:0'\ndrain_timeout_s = 5\n[isolate]\nallow_unbounded_disk = true\n",
        dirs[0].path().join("key").display(),
        dirs[1].path().display(),
        dirs[2].path().display(),
        repo.join("languages.toml").display(),
        repo.join("assets/testlib").display(),
        box_base(),
    )
}

fn assign(id: &str, language: &str, source: &str) -> BridgeMessage {
    let assign = v1::Assign {
        r#ref: Some(v1::AssignmentRef { submission_id: id.into(), generation: 1 }),
        problem_code: "sum".into(),
        data_version: 4,
        language_key: language.into(),
        source: source.as_bytes().to_vec(),
        time_limit_s: 2.0,
        memory_limit_kib: 262_144,
        ..Default::default()
    };
    BridgeMessage { body: Some(bridge_message::Body::Assign(assign)) }
}

fn final_of(messages: &[JudgeMessage], id: &str) -> Option<v1::Final> {
    messages.iter().find_map(|m| match &m.body {
        Some(judge_message::Body::Final(f)) if f.r#ref.as_ref().is_some_and(|r| r.submission_id == id) => {
            Some(f.clone())
        }
        _ => None,
    })
}

fn ack(bridge: &Shared, f: &v1::Final) {
    let ack = v1::ResultAck { r#ref: f.r#ref.clone(), seq: f.seq };
    bridge.send(0, BridgeMessage { body: Some(bridge_message::Body::Ack(ack)) });
}

async fn graded(bridge: &Shared, id: &str) -> v1::Final {
    for _ in 0..3000 {
        if let Some(f) = final_of(&bridge.messages(0), id) {
            return f;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("no Final for {id}: {:?}", bridge.messages(0));
}

fn hello(bridge: &Shared) -> v1::Hello {
    match &bridge.messages(0)[0].body {
        Some(judge_message::Body::Hello(h)) => h.clone(),
        other => panic!("hello first, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_daemon_grades_refuses_unknown_languages_and_drains() {
    let (url, bridge) = start().await;
    let dirs: Vec<_> = (0..3).map(|_| tempfile::tempdir().unwrap()).collect();
    problem(dirs[1].path());
    let config = Arc::new(parse(&config_text(&url, &dirs)).unwrap());
    let (shutdown, stopped) = tokio::sync::oneshot::channel::<()>();
    let daemon = tokio::spawn(run_until(config, async { stopped.await.unwrap_or(()) }));
    eventually(|| !bridge.messages(0).is_empty()).await;
    let hello = hello(&bridge);
    assert_eq!((hello.slots, hello.protocol_minor), (1, judge_proto::PROTOCOL_MINOR));
    assert!(hello.runtimes.iter().any(|r| r.language_key == "CPP17" && !r.version.is_empty()), "{hello:?}");
    assert!(!hello.runtimes.iter().any(|r| r.language_key == "SCRATCH"));
    let welcome = v1::Welcome { session_id: "s".into(), ..Default::default() };
    bridge.send(0, BridgeMessage { body: Some(bridge_message::Body::Welcome(welcome)) });

    bridge.send(0, assign("a", "CPP17", SUM));
    let f = graded(&bridge, "a").await;
    assert_eq!((f.outcome, f.data_version_used, f.seq), (v1::Outcome::Graded as i32, 4, 5));
    let cases: Vec<_> = bridge
        .messages(0)
        .into_iter()
        .filter_map(|m| match m.body {
            Some(judge_message::Body::Cases(c)) => Some(c),
            _ => None,
        })
        .collect();
    assert_eq!(cases.len(), 1);
    assert_eq!(cases[0].cases[0].status, v1::Status::Ac as i32);
    ack(&bridge, &f);

    bridge.send(0, assign("scratch", "SCRATCH", "{}"));
    let f = graded(&bridge, "scratch").await;
    assert_eq!((f.outcome, f.reason), (v1::Outcome::Retry as i32, v1::RetryReason::RuntimeMissing as i32));
    ack(&bridge, &f);

    shutdown.send(()).unwrap();
    eventually(|| {
        bridge.messages(0).iter().any(|m| matches!(m.body, Some(judge_message::Body::Draining(_))))
    })
    .await;
    let ended = tokio::time::timeout(Duration::from_secs(5), daemon).await.expect("drained in time");
    ended.unwrap().unwrap();
}
