//! One gRPC stream to the bridge at a time; reconnects and resumes (spec §3.4-§3.6).

use crate::backoff::Backoff;
use crate::config::{Config, is_loopback_url};
use crate::metrics::Metrics;
use crate::outbox::{AssignmentRef, Outboxes};
use crate::wire::{self, Assignment, BridgeEvent, HelloInfo, JudgeMessage, ResumeDecision};
use judge_proto::v1::judge_link_client::JudgeLinkClient;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{RwLock, mpsc, watch};
use tokio_stream::wrappers::ReceiverStream;
use tonic::transport::{Certificate, Channel, ClientTlsConfig, Endpoint};

pub const HANDSHAKE: Duration = Duration::from_secs(15);
pub const IDLE: Duration = Duration::from_secs(60);
pub const MAX_MESSAGE: usize = 4 << 20;

#[derive(Debug)]
pub struct SlotsFull;

pub trait SlotPool: Send + Sync {
    fn assign(&self, assignment: Assignment) -> Result<(), SlotsFull>;
    fn abort(&self, r: &AssignmentRef);
    fn drop_run(&self, r: &AssignmentRef);
    fn busy(&self) -> u32;
    fn slots(&self) -> u32;
}

#[derive(Debug, thiserror::Error)]
pub enum LinkError {
    #[error("transport: {0}")]
    Transport(#[from] tonic::transport::Error),
    #[error("rpc: {0}")]
    Rpc(#[from] tonic::Status),
    #[error("protocol: {0}")]
    Protocol(String),
    #[error("config: {0}")]
    Config(String),
}

/// The live channel and the session id `FetchSource` must name.
#[derive(Clone)]
pub struct Connection {
    pub channel: Channel,
    pub session_id: String,
}

#[derive(Clone)]
pub struct LinkContext {
    pub config: Arc<Config>,
    pub outboxes: Arc<Outboxes>,
    pub pool: Arc<dyn SlotPool>,
    pub hello: HelloInfo,
    pub connection: Arc<RwLock<Option<Connection>>>,
    pub metrics: Arc<Metrics>,
    pub draining: watch::Receiver<bool>,
}

pub fn endpoint(config: &Config) -> Result<Endpoint, LinkError> {
    let mut endpoint = Endpoint::from_shared(config.bridge_url.clone())?
        .connect_timeout(Duration::from_secs(10))
        .http2_keep_alive_interval(Duration::from_secs(10))
        .keep_alive_timeout(Duration::from_secs(20));
    if let Some(tls) = &config.tls {
        let ca = std::fs::read(&tls.ca_file).map_err(|e| LinkError::Config(format!("ca_file: {e}")))?;
        let mut tls_config = ClientTlsConfig::new().ca_certificate(Certificate::from_pem(ca));
        if let Some(name) = &tls.server_name {
            tls_config = tls_config.domain_name(name.clone());
        }
        endpoint = endpoint.tls_config(tls_config)?;
    } else if !is_loopback_url(&config.bridge_url) {
        return Err(LinkError::Config("plaintext is loopback-only".into()));
    }
    Ok(endpoint)
}

pub fn authed<T>(config: &Config, message: T) -> Result<tonic::Request<T>, LinkError> {
    let key = config.read_key().map_err(|e| LinkError::Config(e.to_string()))?;
    let mut request = tonic::Request::new(message);
    let parse = |v: &str| {
        v.parse::<tonic::metadata::AsciiMetadataValue>()
            .map_err(|_| LinkError::Config("bad metadata value".into()))
    };
    let mut key_value = parse(&key)?;
    key_value.set_sensitive(true);
    request.metadata_mut().insert("x-judge-name", parse(&config.name)?);
    request.metadata_mut().insert("x-judge-key", key_value);
    Ok(request)
}

/// Runs sessions until `stop` turns true (a live session is cut at once); reconnects with backoff.
pub async fn run_link(ctx: LinkContext, mut stop: watch::Receiver<bool>) {
    let reconnect = &ctx.config.reconnect;
    let mut backoff =
        Backoff::new(Duration::from_millis(reconnect.initial_ms), Duration::from_millis(reconnect.max_ms));
    while !*stop.borrow() {
        if let Some(ended) = until_stopped(session(&ctx, &mut backoff), &mut stop).await {
            log_end(ended);
        }
        end_session(&ctx).await;
        until_stopped(tokio::time::sleep(backoff.next_delay()), &mut stop).await;
    }
}

/// `work`'s output, or None when `stop` turned true first.
async fn until_stopped<T>(work: impl Future<Output = T>, stop: &mut watch::Receiver<bool>) -> Option<T> {
    tokio::select! {
        output = work => Some(output),
        _ = stopped(stop) => None,
    }
}

/// Resolves once `stop` is true; never, if its sender is gone.
async fn stopped(stop: &mut watch::Receiver<bool>) {
    if stop.wait_for(|stop| *stop).await.is_err() {
        std::future::pending::<()>().await;
    }
}

fn log_end(ended: Result<(), LinkError>) {
    match ended {
        Ok(()) => tracing::info!("bridge closed the session"),
        Err(error) => tracing::warn!(%error, "bridge session ended"),
    }
}

/// No FetchSource without a session; aborted runs whose Final never went out are forgotten.
async fn end_session(ctx: &LinkContext) {
    *ctx.connection.write().await = None;
    ctx.outboxes.drop_aborted_finals();
    ctx.metrics.reconnects.inc();
}

/// Per-session state: what has been sent per ref, and the send queue.
struct Session<'a> {
    ctx: &'a LinkContext,
    tx: mpsc::Sender<JudgeMessage>,
    sent: HashMap<AssignmentRef, u32>,
}

struct Welcomed {
    session_id: String,
    resume: Vec<(AssignmentRef, ResumeDecision)>,
}

/// The first bridge message must be Welcome within 15 s; its limits apply from now on.
async fn await_welcome(
    ctx: &LinkContext,
    inbound: &mut tonic::Streaming<wire::BridgeMessage>,
) -> Result<Welcomed, LinkError> {
    let first = tokio::time::timeout(HANDSHAKE, inbound.message())
        .await
        .map_err(|_| LinkError::Protocol("no Welcome in 15 s".into()))??;
    let event = first.map(wire::from_bridge).transpose().map_err(|e| LinkError::Protocol(e.to_string()))?;
    let Some(BridgeEvent::Welcome { session_id, resume, max_cases_per_message, max_log_bytes, .. }) = event
    else {
        return Err(LinkError::Protocol("first bridge message was not Welcome".into()));
    };
    ctx.outboxes.limits().update(max_cases_per_message, max_log_bytes);
    Ok(Welcomed { session_id, resume })
}

async fn session(ctx: &LinkContext, backoff: &mut Backoff) -> Result<(), LinkError> {
    let channel = endpoint(&ctx.config)?.connect().await?;
    let mut client = JudgeLinkClient::new(channel.clone()).max_decoding_message_size(MAX_MESSAGE);
    let (tx, rx) = mpsc::channel(64);
    let in_flight = ctx.outboxes.in_flight();
    tx.send(wire::hello(&ctx.hello, ctx.pool.slots(), &in_flight)).await.ok();
    let request = authed(&ctx.config, ReceiverStream::new(rx))?;
    let mut inbound = client.session(request).await?.into_inner();
    let welcomed = await_welcome(ctx, &mut inbound).await?;
    let mut state = Session { ctx, tx, sent: HashMap::new() };
    // Resume decisions apply before the session is visible, so no fetch or worker races them.
    state.apply_resume(in_flight, welcomed.resume);
    *ctx.connection.write().await = Some(Connection { channel, session_id: welcomed.session_id });
    backoff.reset();
    // judge-v2-up.sh (plan D) waits for this line.
    tracing::info!(judge = %ctx.config.name, "bridge session started");
    state.run(&mut inbound).await
}

impl Session<'_> {
    fn apply_resume(
        &mut self,
        in_flight: Vec<(AssignmentRef, u32)>,
        resume: Vec<(AssignmentRef, ResumeDecision)>,
    ) {
        let decisions: HashMap<_, _> = resume.into_iter().collect();
        for (r, _) in in_flight {
            match decisions.get(&r) {
                Some(ResumeDecision::Continue(from_seq)) => {
                    self.sent.insert(r, from_seq.saturating_sub(1));
                }
                _ => {
                    // Nothing already queued goes out; the run ends with one Final{ABORTED} (preflight C2).
                    self.sent.insert(r.clone(), self.ctx.outboxes.last_seq(&r));
                    self.ctx.outboxes.mark_dropped(&r);
                    self.ctx.pool.drop_run(&r);
                }
            }
        }
    }

    async fn run(&mut self, inbound: &mut tonic::Streaming<wire::BridgeMessage>) -> Result<(), LinkError> {
        let mut draining = self.ctx.draining.clone();
        // Until Draining is sent; a closed drain channel means the daemon never drains.
        let mut watching = true;
        // Only bridge messages (pings included) keep the session alive; our own sends do not.
        let mut idle_at = tokio::time::Instant::now() + IDLE;
        self.flush().await;
        loop {
            tokio::select! {
                // Draining first: when a drain and a Final are both ready, the bridge sees Draining first.
                biased;
                changed = draining.changed(), if watching => {
                    watching = changed.is_ok() && !*draining.borrow_and_update();
                    if changed.is_ok() && !watching {
                        self.tx.send(wire::draining()).await.ok();
                    }
                }
                message = inbound.message() => {
                    let Some(message) = message? else { return Ok(()) };
                    idle_at = tokio::time::Instant::now() + IDLE;
                    if !self.handle(message).await? {
                        return Ok(());
                    }
                }
                _ = tokio::time::sleep_until(idle_at) => return Err(LinkError::Protocol("idle for 60 s".into())),
                _ = self.ctx.outboxes.notified() => self.flush().await,
            }
        }
    }

    /// Sends every outbox message the bridge has not seen in this session, in seq order.
    async fn flush(&mut self) {
        for r in self.ctx.outboxes.refs() {
            let after = *self.sent.get(&r).unwrap_or(&0);
            for envelope in self.ctx.outboxes.pending_after(&r, after) {
                if self.tx.send(wire::to_judge(&r, &envelope)).await.is_err() {
                    return;
                }
                self.sent.insert(r.clone(), envelope.seq);
                self.ctx.outboxes.sent(&r, envelope.seq);
            }
        }
    }

    /// false = the bridge asked to disconnect.
    async fn handle(&mut self, message: wire::BridgeMessage) -> Result<bool, LinkError> {
        let event = wire::from_bridge(message).map_err(|e| LinkError::Protocol(e.to_string()))?;
        match event {
            BridgeEvent::Ping(nonce) => {
                self.tx.send(wire::pong(nonce, self.ctx.pool.busy(), self.ctx.pool.slots())).await.ok();
            }
            BridgeEvent::Assign(assignment) => self.assign(assignment).await,
            BridgeEvent::Abort(r) => self.ctx.pool.abort(&r),
            BridgeEvent::Ack(r, seq) => {
                self.ctx.outboxes.ack(&r, seq);
            }
            BridgeEvent::Disconnect => return Ok(false),
            BridgeEvent::Error(message) => tracing::warn!(%message, "bridge error"),
            BridgeEvent::Welcome { .. } => return Err(LinkError::Protocol("second Welcome".into())),
        }
        Ok(true)
    }

    async fn assign(&mut self, assignment: Assignment) {
        let r = assignment.r.clone();
        if self.ctx.pool.assign(assignment).is_err() {
            self.tx.send(wire::slots_full(&r)).await.ok();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_key_metadata_is_sensitive_and_the_name_is_not() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("k"), "sekret\n").unwrap();
        let mut config = crate::config::parse(crate::config::tests_support::MIN).unwrap();
        config.key_file = dir.path().join("k");
        let request = authed(&config, ()).unwrap();
        assert!(request.metadata().get("x-judge-key").unwrap().is_sensitive());
        assert!(!request.metadata().get("x-judge-name").unwrap().is_sensitive());
        assert!(!format!("{:?}", request.metadata()).contains("sekret"), "debug output hides the key");
    }

    #[test]
    fn plaintext_is_loopback_only_and_tls_needs_its_ca() {
        let mut config = crate::config::parse(crate::config::tests_support::MIN).unwrap();
        assert!(endpoint(&config).is_ok(), "plaintext on loopback");
        config.bridge_url = "http://10.0.0.7:9990".into();
        assert!(matches!(endpoint(&config), Err(LinkError::Config(_))));
        config.bridge_url = "https://bridge.example:9990".into();
        let tls = crate::config::TlsConfig { ca_file: "/nonexistent/ca.pem".into(), server_name: None };
        config.tls = Some(tls);
        assert!(matches!(endpoint(&config), Err(LinkError::Config(m)) if m.contains("ca_file")));
    }
}
