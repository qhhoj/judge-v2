//! Shared pieces of the two transcript players, and self-play: the fake bridge
//! and the transcript judge run one scenario against each other over real gRPC.
use std::{future::Future, time::Duration};

use judge_proto::v1::judge_link_server::JudgeLinkServer;
use tokio::{net::TcpListener, time::Instant};
use tokio_stream::wrappers::TcpListenerStream;

use crate::{fake_bridge, host_stall, scenario::Scenario, transcript_judge};

const SELF_PLAY_MAX_MESSAGE: usize = 8 * 1024 * 1024;

/// The longest a player waits for the other side's next message, not counting host stalls.
pub const STEP_TIMEOUT: Duration = Duration::from_secs(5);
/// Host stalls extend a wait by at most this, so a host that never runs the process again still fails the step.
pub const MAX_STALL_EXTENSION: Duration = Duration::from_secs(60);

/// The key every scenario judge authenticates with (the backend runner stores its sha256).
pub fn key_of(judge: &str) -> String {
    format!("{judge}-key")
}

pub fn code_of(status: &str) -> Result<tonic::Code, String> {
    Ok(match status {
        "OK" => tonic::Code::Ok,
        "CANCELLED" => tonic::Code::Cancelled,
        "INVALID_ARGUMENT" => tonic::Code::InvalidArgument,
        "DEADLINE_EXCEEDED" => tonic::Code::DeadlineExceeded,
        "PERMISSION_DENIED" => tonic::Code::PermissionDenied,
        "RESOURCE_EXHAUSTED" => tonic::Code::ResourceExhausted,
        "FAILED_PRECONDITION" => tonic::Code::FailedPrecondition,
        "ABORTED" => tonic::Code::Aborted,
        "UNAVAILABLE" => tonic::Code::Unavailable,
        "UNAUTHENTICATED" => tonic::Code::Unauthenticated,
        "NOT_FOUND" => tonic::Code::NotFound,
        "INTERNAL" => tonic::Code::Internal,
        other => return Err(format!("unknown status {other}")),
    })
}

/// `future`'s output within `STEP_TIMEOUT` of time the host ran this process: each time the deadline passes,
/// it moves out by the host stall measured since the wait began (`host_stall`), up to `MAX_STALL_EXTENSION`.
pub async fn within<T>(what: &str, future: impl Future<Output = T>) -> Result<T, String> {
    let (began, stalled_before) = (Instant::now(), host_stall::stalled());
    let mut deadline = began + STEP_TIMEOUT;
    tokio::pin!(future);
    loop {
        if let Ok(output) = tokio::time::timeout_at(deadline, &mut future).await {
            return Ok(output);
        }
        if let Ok(output) = tokio::time::timeout(host_stall::SETTLE, &mut future).await {
            return Ok(output);
        }
        let stall = (host_stall::stalled() - stalled_before).min(MAX_STALL_EXTENSION);
        let extended = began + STEP_TIMEOUT + stall;
        if extended <= deadline {
            return Err(format!(
                "timed out waiting for {what} ({} ms of it a host stall)",
                stall.as_millis()
            ));
        }
        eprintln!(
            "waiting for {what}: the host stalled this process {} ms, wait extended",
            stall.as_millis()
        );
        deadline = extended;
    }
}

/// Plays both roles of `scenario` against each other; Err names the first mismatch.
pub async fn self_play(scenario: &Scenario) -> Result<(), String> {
    play_pair(scenario, scenario).await
}

/// The bridge role plays `bridge_side`, the judge role `judge_side` (they differ only in tests).
pub async fn play_pair(bridge_side: &Scenario, judge_side: &Scenario) -> Result<(), String> {
    let listener = TcpListener::bind("127.0.0.1:0").await.map_err(|error| error.to_string())?;
    let endpoint = format!("http://{}", listener.local_addr().map_err(|error| error.to_string())?);
    let bindings = bridge_side.sample_bindings();
    let (service, incoming) = fake_bridge::FakeBridge::new(bridge_side);
    let server = tokio::spawn(
        tonic::transport::Server::builder()
            // above the bridge's 4 MiB, so the transcript itself decides how an oversized message ends
            .add_service(JudgeLinkServer::new(service).max_decoding_message_size(SELF_PLAY_MAX_MESSAGE))
            .serve_with_incoming(TcpListenerStream::new(listener)),
    );
    let (bridge, judge) = tokio::join!(
        fake_bridge::play_bridge(bridge_side, &bindings, incoming),
        transcript_judge::play_judge(judge_side, &bindings, endpoint),
    );
    server.abort();
    // both sides keep their sessions until both are done, so neither sees an early end
    let _bridge_sessions = bridge.map_err(|error| format!("bridge role: {error}"))?;
    judge.map(drop).map_err(|error| format!("judge role: {error}"))
}
