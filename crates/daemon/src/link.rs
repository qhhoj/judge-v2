//! One gRPC stream to the bridge at a time; reconnects and resumes (docs/protocol.md).

use crate::backoff::Backoff;
use crate::config::{Config, is_loopback_url};
use crate::metrics::Metrics;
use crate::outbox::{AssignmentRef, Outboxes};
use crate::wire::{self, Assignment, BridgeEvent, HelloInfo, JudgeMessage, ResumeDecision};
use judge_grader::generate::{GenerationFailure, GenerationPlan};
use judge_proto::v1::judge_link_client::JudgeLinkClient;
use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::sync::{RwLock, mpsc, oneshot, watch};
use tokio_stream::Stream;
use tonic::transport::{Certificate, Channel, ClientTlsConfig, Endpoint};

pub const HANDSHAKE: Duration = Duration::from_secs(15);
pub const IDLE: Duration = Duration::from_secs(60);
/// From the stop, a welcomed session has this long to close (Draining, our end, the bridge's end), or it is cut.
pub const CLOSE: Duration = Duration::from_secs(15);
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

/// Runs sessions until `stop` turns true; reconnects with backoff. A welcomed session ends its stream after
/// `Draining` and waits up to `CLOSE` for the bridge to end it (issue #94); one still connecting is cut at once.
pub async fn run_link(ctx: LinkContext, mut stop: watch::Receiver<bool>) {
    let reconnect = &ctx.config.reconnect;
    let mut backoff =
        Backoff::new(Duration::from_millis(reconnect.initial_ms), Duration::from_millis(reconnect.max_ms));
    while !*stop.borrow() {
        let started = std::time::Instant::now();
        log_end(session(&ctx, &mut stop).await);
        // Only a session that lasted resets the backoff: a Welcome-then-close loop keeps backing off.
        if started.elapsed() >= STABLE_SESSION {
            backoff.reset();
        }
        end_session(&ctx).await;
        until_stopped(tokio::time::sleep(backoff.next_delay()), &mut stop).await;
    }
}

/// `work`'s output, or None when `stop` turned true first; work that is ready wins over a stop.
async fn until_stopped<T>(work: impl Future<Output = T>, stop: &mut watch::Receiver<bool>) -> Option<T> {
    tokio::select! {
        biased;
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
        Ok(()) => tracing::info!("bridge session ended"),
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

/// What a session's loop watches besides the stream: the drain signal, stop, and the idle deadline.
struct Watch {
    draining: watch::Receiver<bool>,
    /// Until Draining is sent.
    watching: bool,
    stop: watch::Receiver<bool>,
    deadline: tokio::time::Instant,
}

/// What the session's `Watch` saw: the drain signal changed (`open` false: its channel is gone), or `stop`.
enum Signal {
    Drain { open: bool },
    Stop,
}

impl Watch {
    /// The next drain change (while watching) or stop (until `closing`); the drain goes first when both are ready.
    async fn next_signal(&mut self, closing: bool) -> Signal {
        tokio::select! {
            biased;
            changed = self.draining.changed(), if self.watching => Signal::Drain { open: changed.is_ok() },
            _ = stopped(&mut self.stop), if !closing => Signal::Stop,
            else => std::future::pending().await,
        }
    }
}

/// The session's request stream: the queued messages, then its end once `close` fires (or its sender is gone).
struct Outbound {
    rx: mpsc::Receiver<JudgeMessage>,
    close: Option<oneshot::Receiver<()>>,
}

impl Stream for Outbound {
    type Item = JudgeMessage;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<JudgeMessage>> {
        let closed = self.close.as_mut().is_some_and(|close| Pin::new(close).poll(cx).is_ready());
        if closed {
            // What is already queued still goes out; nothing new is taken.
            self.rx.close();
            self.close = None;
        }
        self.rx.poll_recv(cx)
    }
}

/// Per-session state: what has been sent per ref, and the send queue.
struct Session<'a> {
    ctx: &'a LinkContext,
    session_id: String,
    tx: mpsc::Sender<JudgeMessage>,
    /// Ends the request stream (`Outbound`).
    close: Option<oneshot::Sender<()>>,
    sent: HashMap<AssignmentRef, u32>,
}

struct Welcomed {
    session_id: String,
    resume: Vec<(AssignmentRef, ResumeDecision)>,
}

/// A stream the bridge has welcomed, not yet visible to the rest of the daemon.
struct Opened {
    channel: Channel,
    inbound: tonic::Streaming<wire::BridgeMessage>,
    welcomed: Welcomed,
    tx: mpsc::Sender<JudgeMessage>,
    close: oneshot::Sender<()>,
    /// The runs the Hello listed, which the Welcome's resume decisions answer.
    in_flight: Vec<(AssignmentRef, u32)>,
}

/// Opens the stream and reads the first bridge message, which must be Welcome; its limits apply from now on.
async fn handshake(
    ctx: &LinkContext,
    client: &mut JudgeLinkClient<Channel>,
    outbound: Outbound,
) -> Result<(tonic::Streaming<wire::BridgeMessage>, Welcomed), LinkError> {
    let mut inbound = client.session(authed(&ctx.config, outbound)?).await?.into_inner();
    let first = inbound.message().await?;
    let event = first.map(wire::from_bridge).transpose().map_err(|e| LinkError::Protocol(e.to_string()))?;
    let Some(BridgeEvent::Welcome { session_id, resume, max_cases_per_message, max_log_bytes, .. }) = event
    else {
        return Err(LinkError::Protocol("first bridge message was not Welcome".into()));
    };
    ctx.outboxes.limits().update(max_cases_per_message, max_log_bytes);
    Ok((inbound, Welcomed { session_id, resume }))
}

/// One session: only connecting and the handshake are cut at once by `stop`. From the Welcome on, the session
/// closes itself, so `Draining` is never lost (issue #94); `CLOSE` after the stop it is cut wherever it waits.
async fn session(ctx: &LinkContext, stop: &mut watch::Receiver<bool>) -> Result<(), LinkError> {
    let Some(opened) = until_stopped(open(ctx), stop).await else {
        return Ok(());
    };
    let opened = opened?;
    let mut bound = stop.clone();
    tokio::select! {
        biased;
        ended = welcomed(ctx, opened, stop.clone()) => ended,
        _ = close_bound(&mut bound) => Err(LinkError::Protocol("the session did not close in 15 s after stop".into())),
    }
}

async fn welcomed(ctx: &LinkContext, opened: Opened, stop: watch::Receiver<bool>) -> Result<(), LinkError> {
    let (mut state, mut inbound) = start(ctx, opened).await;
    state.run(&mut inbound, stop).await
}

/// Resolves `CLOSE` after `stop` turned true.
async fn close_bound(stop: &mut watch::Receiver<bool>) {
    stopped(stop).await;
    tokio::time::sleep(CLOSE).await;
}

/// Connects, sends Hello and waits for the Welcome.
async fn open(ctx: &LinkContext) -> Result<Opened, LinkError> {
    let channel = endpoint(&ctx.config)?.connect().await?;
    let mut client = JudgeLinkClient::new(channel.clone()).max_decoding_message_size(MAX_MESSAGE);
    let (tx, rx) = mpsc::channel(64);
    let (close, close_rx) = oneshot::channel();
    let in_flight = ctx.outboxes.in_flight();
    tx.send(wire::hello(&ctx.hello, ctx.pool.slots(), &in_flight)).await.ok();
    // The whole handshake (response headers and Welcome) gets 15 s: a wedged bridge is left.
    let (inbound, welcomed) =
        tokio::time::timeout(HANDSHAKE, handshake(ctx, &mut client, Outbound { rx, close: Some(close_rx) }))
            .await
            .map_err(|_| LinkError::Protocol("no Welcome in 15 s".into()))??;
    Ok(Opened { channel, inbound, welcomed, tx, close, in_flight })
}

/// Applies the Welcome's resume decisions, then makes the session visible (`Connection`).
async fn start(ctx: &LinkContext, opened: Opened) -> (Session<'_>, tonic::Streaming<wire::BridgeMessage>) {
    let Opened { channel, inbound, welcomed, tx, close, in_flight } = opened;
    let outbound = tx.clone();
    let mut state = Session {
        ctx,
        session_id: welcomed.session_id.clone(),
        tx,
        close: Some(close),
        sent: HashMap::new(),
    };
    // Resume decisions apply before the session is visible, so no fetch or worker races them.
    state.apply_resume(in_flight, welcomed.resume);
    *ctx.connection.write().await = Some(Connection { channel, session_id: welcomed.session_id, outbound });
    // The backend's local-stack script (scripts/judge/judge-v2-up.sh) waits for this line.
    tracing::info!(judge = %ctx.config.name, "bridge session started");
    (state, inbound)
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

    async fn run(
        &mut self,
        inbound: &mut tonic::Streaming<wire::BridgeMessage>,
        stop: watch::Receiver<bool>,
    ) -> Result<(), LinkError> {
        let mut draining = self.ctx.draining.clone();
        // A drain that began while disconnected is announced before anything is replayed.
        let watching = self.announce_if_draining(&mut draining).await;
        // Only bridge messages (pings included) keep the session alive; our own sends do not.
        let deadline = tokio::time::Instant::now() + IDLE;
        let mut watch = Watch { draining, watching, stop, deadline };
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
        let deadline = w.deadline;
        tokio::select! {
            // Draining first: when a drain and a Final are both ready, the bridge sees Draining first.
            biased;
            signal = w.next_signal(self.closing()) => self.on_signal(signal, w).await,
            // A message that is ready wins over its deadline.
            message = tokio::time::timeout_at(deadline, inbound.message()) => self.on_inbound(message, w).await,
            _ = self.ctx.outboxes.notified() => self.on_outbox_change().await,
        }
    }

    async fn on_signal(&mut self, signal: Signal, w: &mut Watch) -> Result<bool, LinkError> {
        match signal {
            Signal::Drain { open } => self.on_drain_change(open, w).await,
            Signal::Stop => self.end_our_stream(w).await,
        }
    }

    /// `open` is false when the drain channel is closed: the daemon never drains.
    async fn on_drain_change(&mut self, open: bool, w: &mut Watch) -> Result<bool, LinkError> {
        w.watching = open && self.announce_if_draining(&mut w.draining).await;
        Ok(true)
    }

    /// A bridge message, or the idle deadline passed (`Elapsed`).
    async fn on_inbound(
        &mut self,
        message: Result<Result<Option<wire::BridgeMessage>, tonic::Status>, tokio::time::error::Elapsed>,
        w: &mut Watch,
    ) -> Result<bool, LinkError> {
        let Ok(message) = message else {
            return Err(LinkError::Protocol("idle for 60 s".into()));
        };
        w.deadline = tokio::time::Instant::now() + IDLE;
        self.on_message(message).await
    }

    async fn on_outbox_change(&mut self) -> Result<bool, LinkError> {
        self.flush().await;
        Ok(true)
    }

    /// Stop: `Draining` is queued if not sent yet, then any ready outbox message, then our stream ends after them.
    /// The session ends when the bridge ends it, which it does only after reading our end, so `Draining` is never
    /// lost to the stop (issue #94). `session` bounds this by `CLOSE`.
    async fn end_our_stream(&mut self, w: &mut Watch) -> Result<bool, LinkError> {
        if w.watching {
            self.announce_if_draining(&mut w.draining).await;
            w.watching = false;
        }
        self.flush().await;
        if let Some(close) = self.close.take() {
            let _ = close.send(());
        }
        Ok(true)
    }

    /// Our stream has ended at stop: nothing more can be sent, so no new work is taken.
    fn closing(&self) -> bool {
        self.close.is_none()
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
        if self.closing() {
            // The bridge takes it back when the session ends.
            tracing::info!(generation = %id, "generation dropped: the judge is stopping");
            return;
        }
        if self.ctx.pool.generate(plan, self.session_id.clone()).is_err() {
            self.tx.send(wire::generation_slots_full(&id)).await.ok();
            let refused = GenerationFailure::internal("no free slot");
            self.tx.send(wire::generation_done(&id, &Err(refused))).await.ok();
        }
    }

    async fn assign(&mut self, assignment: Assignment) {
        let r = assignment.r.clone();
        if self.closing() {
            // The bridge takes it back when the session ends.
            tracing::info!(submission = %r.submission_id, "assignment dropped: the judge is stopping");
            return;
        }
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
