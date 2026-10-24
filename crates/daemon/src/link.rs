//! One gRPC stream to the bridge at a time; reconnects and resumes (docs/protocol.md).

use crate::backoff::Backoff;
use crate::config::{Config, is_loopback_url};
use crate::metrics::Metrics;
use crate::outbox::{AssignmentRef, Outboxes};
use crate::wire::{self, Assignment, BridgeEvent, HelloInfo, JudgeMessage, ResumeDecision};
use judge_grader::generate::{GenerationFailure, GenerationPlan};
use judge_proto::v1::judge_link_client::JudgeLinkClient;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{RwLock, mpsc, watch};
use tokio_stream::wrappers::ReceiverStream;
use tonic::transport::{Certificate, Channel, ClientTlsConfig, Endpoint};

pub const HANDSHAKE: Duration = Duration::from_secs(15);
pub const IDLE: Duration = Duration::from_secs(60);
/// A session at least this long resets the reconnect backoff.
pub const STABLE_SESSION: Duration = Duration::from_secs(30);
pub const MAX_MESSAGE: usize = 4 << 20;

#[derive(Debug)]
pub struct SlotsFull;

pub trait SlotPool: Send + Sync {
    fn assign(&self, assignment: Assignment) -> Result<(), SlotsFull>;
    fn abort(&self, r: &AssignmentRef);
    fn drop_run(&self, r: &AssignmentRef);
    /// Takes a free slot for a generation of the session `session_id`.
    fn generate(&self, plan: GenerationPlan, session_id: String) -> Result<(), SlotsFull>;
    fn cancel_generation(&self, generation_id: &str);
    /// A generation does not survive its session.
    fn cancel_generations(&self);
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
    /// The session's send queue, for what is not in an outbox (`GenerationDone`).
    pub outbound: mpsc::Sender<JudgeMessage>,
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
        if !config.bridge_url.starts_with("https://") {
            return Err(LinkError::Config("tls needs an https:// bridge_url".into()));
        }
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
        let started = std::time::Instant::now();
        if let Some(ended) = until_stopped(session(&ctx), &mut stop).await {
            log_end(ended);
        }
        // Only a session that lasted resets the backoff: a Welcome-then-close loop keeps backing off.
        if started.elapsed() >= STABLE_SESSION {
            backoff.reset();
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

/// No FetchSource without a session; finished runs the bridge never acks (aborted, or aborted by it) are forgotten.
async fn end_session(ctx: &LinkContext) {
    *ctx.connection.write().await = None;
    ctx.pool.cancel_generations();
    ctx.outboxes.drop_never_acked();
    ctx.metrics.reconnects.inc();
}

/// What a session's loop watches besides the stream: the drain signal and the idle deadline.
struct Watch {
    draining: watch::Receiver<bool>,
    /// Until Draining is sent.
    watching: bool,
    idle_at: tokio::time::Instant,
}

/// Per-session state: what has been sent per ref, and the send queue.
struct Session<'a> {
    ctx: &'a LinkContext,
    session_id: String,
    tx: mpsc::Sender<JudgeMessage>,
    sent: HashMap<AssignmentRef, u32>,
}

struct Welcomed {
    session_id: String,
    resume: Vec<(AssignmentRef, ResumeDecision)>,
}

/// Opens the stream and reads the first bridge message, which must be Welcome; its limits apply from now on.
async fn handshake(
    ctx: &LinkContext,
    client: &mut JudgeLinkClient<Channel>,
    rx: mpsc::Receiver<JudgeMessage>,
) -> Result<(tonic::Streaming<wire::BridgeMessage>, Welcomed), LinkError> {
    let mut inbound = client.session(authed(&ctx.config, ReceiverStream::new(rx))?).await?.into_inner();
    let first = inbound.message().await?;
    let event = first.map(wire::from_bridge).transpose().map_err(|e| LinkError::Protocol(e.to_string()))?;
    let Some(BridgeEvent::Welcome { session_id, resume, max_cases_per_message, max_log_bytes, .. }) = event
    else {
        return Err(LinkError::Protocol("first bridge message was not Welcome".into()));
    };
    ctx.outboxes.limits().update(max_cases_per_message, max_log_bytes);
    Ok((inbound, Welcomed { session_id, resume }))
}

async fn session(ctx: &LinkContext) -> Result<(), LinkError> {
    let channel = endpoint(&ctx.config)?.connect().await?;
    let mut client = JudgeLinkClient::new(channel.clone()).max_decoding_message_size(MAX_MESSAGE);
    let (tx, rx) = mpsc::channel(64);
    let in_flight = ctx.outboxes.in_flight();
    tx.send(wire::hello(&ctx.hello, ctx.pool.slots(), &in_flight)).await.ok();
    // The whole handshake (response headers and Welcome) gets 15 s: a wedged bridge is left.
    let (mut inbound, welcomed) = tokio::time::timeout(HANDSHAKE, handshake(ctx, &mut client, rx))
        .await
        .map_err(|_| LinkError::Protocol("no Welcome in 15 s".into()))??;
    let outbound = tx.clone();
    let mut state = Session { ctx, session_id: welcomed.session_id.clone(), tx, sent: HashMap::new() };
    // Resume decisions apply before the session is visible, so no fetch or worker races them.
    state.apply_resume(in_flight, welcomed.resume);
    *ctx.connection.write().await = Some(Connection { channel, session_id: welcomed.session_id, outbound });
    // The backend's local-stack script (scripts/judge/judge-v2-up.sh) waits for this line.
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
                    // Nothing already queued goes out; the run ends with one Final{ABORTED} (docs/protocol.md#reconnect-and-resume).
                    self.sent.insert(r.clone(), self.ctx.outboxes.last_seq(&r));
                    self.ctx.outboxes.mark_dropped(&r);
                    self.ctx.pool.drop_run(&r);
                }
            }
        }
    }

    async fn run(&mut self, inbound: &mut tonic::Streaming<wire::BridgeMessage>) -> Result<(), LinkError> {
        let mut draining = self.ctx.draining.clone();
        // A drain that began while disconnected is announced before anything is replayed.
        let watching = self.announce_if_draining(&mut draining).await;
        // Only bridge messages (pings included) keep the session alive; our own sends do not.
        let mut watch = Watch { draining, watching, idle_at: tokio::time::Instant::now() + IDLE };
        self.flush().await;
        while self.step(inbound, &mut watch).await? {}
        Ok(())
    }

    /// One wakeup of the session; false when it ends.
    async fn step(
        &mut self,
        inbound: &mut tonic::Streaming<wire::BridgeMessage>,
        w: &mut Watch,
    ) -> Result<bool, LinkError> {
        tokio::select! {
            // Draining first: when a drain and a Final are both ready, the bridge sees Draining first.
            biased;
            changed = w.draining.changed(), if w.watching => {
                // A closed drain channel means the daemon never drains.
                w.watching = changed.is_ok() && self.announce_if_draining(&mut w.draining).await;
                Ok(true)
            }
            message = inbound.message() => {
                w.idle_at = tokio::time::Instant::now() + IDLE;
                self.on_message(message).await
            }
            _ = tokio::time::sleep_until(w.idle_at) => Err(LinkError::Protocol("idle for 60 s".into())),
            _ = self.ctx.outboxes.notified() => {
                self.flush().await;
                Ok(true)
            }
        }
    }

    /// Sends `Draining` if the daemon drains; true while it does not (keep watching).
    async fn announce_if_draining(&mut self, draining: &mut watch::Receiver<bool>) -> bool {
        if !*draining.borrow_and_update() {
            return true;
        }
        self.tx.send(wire::draining()).await.ok();
        false
    }

    /// false when the session ends (stream closed or Disconnect).
    async fn on_message(
        &mut self,
        message: Result<Option<wire::BridgeMessage>, tonic::Status>,
    ) -> Result<bool, LinkError> {
        match message? {
            Some(message) => self.handle(message).await,
            None => Ok(false),
        }
    }

    /// Sends every outbox message the bridge has not seen in this session, in seq order.
    async fn flush(&mut self) {
        let refs = self.ctx.outboxes.refs();
        // Acked and removed outboxes are forgotten here too.
        self.sent.retain(|r, _| refs.contains(r));
        for r in refs {
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
            BridgeEvent::Generate(plan) => self.generate(*plan).await,
            BridgeEvent::CancelGeneration(id) => self.ctx.pool.cancel_generation(&id),
            BridgeEvent::Abort(r) => {
                self.ctx.pool.abort(&r);
                self.ctx.outboxes.bridge_aborted(&r);
            }
            BridgeEvent::Ack(r, seq) => {
                self.ctx.outboxes.ack(&r, seq);
            }
            BridgeEvent::Disconnect => return Ok(false),
            BridgeEvent::Error(message) => tracing::warn!(%message, "bridge error"),
            BridgeEvent::Unknown => tracing::debug!("unknown bridge message ignored"),
            BridgeEvent::Welcome { .. } => return Err(LinkError::Protocol("second Welcome".into())),
        }
        Ok(true)
    }

    async fn generate(&mut self, plan: GenerationPlan) {
        let id = plan.id.clone();
        if self.ctx.pool.generate(plan, self.session_id.clone()).is_err() {
            self.tx.send(wire::generation_slots_full(&id)).await.ok();
            let refused = GenerationFailure::internal("no free slot");
            self.tx.send(wire::generation_done(&id, &Err(refused))).await.ok();
        }
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
        let tls_on_http =
            crate::config::TlsConfig { ca_file: "/nonexistent/ca.pem".into(), server_name: None };
        config.tls = Some(tls_on_http);
        assert!(matches!(endpoint(&config), Err(LinkError::Config(m)) if m.contains("https")));
        config.bridge_url = "https://bridge.example:9990".into();
        let tls = crate::config::TlsConfig { ca_file: "/nonexistent/ca.pem".into(), server_name: None };
        config.tls = Some(tls);
        assert!(matches!(endpoint(&config), Err(LinkError::Config(m)) if m.contains("ca_file")));
    }
}
