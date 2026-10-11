//! The bridge role of a transcript: a tonic `JudgeLink` server that sends the
//! scenario's `bridge` steps and checks its `judge` steps. judge-v2 points its
//! daemon at it; self-play points the transcript judge at it. It also checks what the
//! scenario cannot say as a message: the judge's key on every call, that a `fetch` step
//! is a FetchSource call for the right ref and session, and that nothing follows the last step.
use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex},
    time::Duration,
};

use judge_proto::v1::{
    BridgeMessage, Chunk, FetchSourceRequest, GenerationRequest, JudgeMessage, UploadChunk, UploadReceipt,
    bridge_message::Body, judge_link_server::JudgeLink,
};
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_stream::{StreamExt, wrappers::ReceiverStream};
use tonic::{Request, Response, Status, Streaming};

use crate::{
    expand::{Bindings, expand},
    matcher::matches,
    play::{code_of, key_of, within},
    scenario::{Scenario, Step, parse_bridge},
};

const CHUNK_BYTES: u64 = 1024 * 1024;
/// After the last step the judge must stay silent this long.
const TAIL_WINDOW: Duration = Duration::from_millis(120);
const TAIL_POLL: Duration = Duration::from_millis(15);
type Outbound = mpsc::Sender<Result<BridgeMessage, Status>>;

pub enum Incoming {
    Session {
        judge: String,
        inbound: Box<Streaming<JudgeMessage>>,
        outbound: Outbound,
    },
    Refused {
        judge: String,
    },
    Fetch {
        judge: String,
        request: FetchSourceRequest,
        authenticated: bool,
    },
    /// A call the scenario has no step for (a retry, a second connection).
    Unscripted {
        judge: String,
        call: &'static str,
    },
}

/// Per judge, the outcomes the transcript gives its Session and FetchSource calls, in order.
#[derive(Default)]
struct Answers {
    opens: HashMap<String, VecDeque<String>>,
    fetches: HashMap<String, VecDeque<(String, u64)>>,
}

impl Answers {
    fn of(scenario: &Scenario) -> Self {
        let mut answers = Self::default();
        for step in &scenario.steps {
            match step {
                Step::Open { judge, expect, .. } => {
                    let name = scenario.identity(judge).to_string();
                    answers.opens.entry(name).or_default().push_back(expect.clone())
                }
                Step::Fetch { judge, expect, bytes, .. } => {
                    let name = scenario.identity(judge).to_string();
                    answers.fetches.entry(name).or_default().push_back((expect.clone(), *bytes))
                }
                _ => {}
            }
        }
        answers
    }
}

pub struct FakeBridge {
    incoming: mpsc::UnboundedSender<Incoming>,
    answers: Arc<Mutex<Answers>>,
}

impl FakeBridge {
    pub fn new(scenario: &Scenario) -> (Self, mpsc::UnboundedReceiver<Incoming>) {
        let (incoming, receiver) = mpsc::unbounded_channel();
        let answers = Arc::new(Mutex::new(Answers::of(scenario)));
        (Self { incoming, answers }, receiver)
    }

    fn next_answer<T>(&self, pick: impl FnOnce(&mut Answers) -> Option<T>) -> Option<T> {
        pick(&mut self.answers.lock().expect("answers lock"))
    }
}

fn judge_name<T>(request: &Request<T>) -> String {
    let value = request.metadata().get("x-judge-name").and_then(|value| value.to_str().ok());
    value.unwrap_or_default().to_string()
}

/// Whether the call carries the key the scenario gives this judge.
fn key_is_right<T>(request: &Request<T>, judge: &str) -> bool {
    let sent = request.metadata().get("x-judge-key").and_then(|value| value.to_str().ok());
    sent == Some(key_of(judge).as_str())
}

fn refusal(expect: &str) -> Status {
    match code_of(expect) {
        Ok(code) => Status::new(code, "refused by the scenario"),
        Err(error) => Status::internal(error),
    }
}

/// `bytes` bytes of `x`, at most 1 MiB per chunk.
fn chunks(bytes: u64) -> ReceiverStream<Result<Chunk, Status>> {
    let (sender, receiver) = mpsc::channel(4);
    tokio::spawn(async move {
        let mut left = bytes;
        while left > 0 {
            let size = left.min(CHUNK_BYTES);
            left -= size;
            if sender.send(Ok(Chunk { data: vec![b'x'; size as usize] })).await.is_err() {
                return;
            }
        }
    });
    ReceiverStream::new(receiver)
}

#[tonic::async_trait]
impl JudgeLink for FakeBridge {
    type SessionStream = ReceiverStream<Result<BridgeMessage, Status>>;
    type FetchSourceStream = ReceiverStream<Result<Chunk, Status>>;
    type FetchGenerationBundleStream = ReceiverStream<Result<Chunk, Status>>;

    async fn session(
        &self,
        request: Request<Streaming<JudgeMessage>>,
    ) -> Result<Response<Self::SessionStream>, Status> {
        let judge = judge_name(&request);
        let expect = self.next_answer(|answers| answers.opens.get_mut(&judge)?.pop_front());
        let Some(expect) = expect else {
            let _ = self.incoming.send(Incoming::Unscripted { judge, call: "Session" });
            return Err(Status::failed_precondition("no open step left for this judge"));
        };
        if !key_is_right(&request, &judge) {
            let _ = self.incoming.send(Incoming::Refused { judge });
            return Err(Status::unauthenticated("wrong or missing x-judge-key"));
        }
        if expect != "OK" {
            let _ = self.incoming.send(Incoming::Refused { judge });
            return Err(refusal(&expect));
        }
        let (outbound, receiver) = mpsc::channel(64);
        let session = Incoming::Session { judge, inbound: Box::new(request.into_inner()), outbound };
        self.incoming.send(session).map_err(|_| Status::internal("scenario driver stopped"))?;
        Ok(Response::new(ReceiverStream::new(receiver)))
    }

    async fn fetch_source(
        &self,
        request: Request<FetchSourceRequest>,
    ) -> Result<Response<Self::FetchSourceStream>, Status> {
        let judge = judge_name(&request);
        let answer = self.next_answer(|answers| answers.fetches.get_mut(&judge)?.pop_front());
        let Some((expect, bytes)) = answer else {
            let _ = self.incoming.send(Incoming::Unscripted { judge, call: "FetchSource" });
            return Err(Status::failed_precondition("no fetch step left for this judge"));
        };
        let authenticated = key_is_right(&request, &judge);
        let fetch = Incoming::Fetch { judge, request: request.into_inner(), authenticated };
        let _ = self.incoming.send(fetch);
        if !authenticated {
            return Err(Status::unauthenticated("wrong or missing x-judge-key"));
        }
        if expect != "OK" {
            return Err(refusal(&expect));
        }
        Ok(Response::new(chunks(bytes)))
    }

    /// Scenarios do not script generation transfers yet: a call is reported as unscripted.
    async fn fetch_generation_bundle(
        &self,
        request: Request<GenerationRequest>,
    ) -> Result<Response<Self::FetchGenerationBundleStream>, Status> {
        let _ = self
            .incoming
            .send(Incoming::Unscripted { judge: judge_name(&request), call: "FetchGenerationBundle" });
        Err(Status::failed_precondition("no generation transfer in the scenario"))
    }

    async fn upload_generated_tests(
        &self,
        request: Request<Streaming<UploadChunk>>,
    ) -> Result<Response<UploadReceipt>, Status> {
        let _ = self
            .incoming
            .send(Incoming::Unscripted { judge: judge_name(&request), call: "UploadGeneratedTests" });
        Err(Status::failed_precondition("no generation transfer in the scenario"))
    }
}

/// Sessions still open after the last step; see `transcript_judge::OpenSessions`.
pub struct OpenSessions(#[allow(dead_code)] HashMap<String, Conn>);

#[derive(Default)]
pub struct Conn {
    inbound: Option<Box<Streaming<JudgeMessage>>>,
    outbound: Option<Outbound>,
}

/// Everything the bridge role needs while it walks the steps.
struct Driver<'a> {
    scenario: &'a Scenario,
    bindings: &'a Bindings,
    conns: HashMap<String, Conn>,
    incoming: mpsc::UnboundedReceiver<Incoming>,
}

/// Walks the steps in the bridge role; `incoming` comes from `FakeBridge::new`.
pub async fn play_bridge(
    scenario: &Scenario,
    bindings: &Bindings,
    incoming: mpsc::UnboundedReceiver<Incoming>,
) -> Result<OpenSessions, String> {
    let mut driver = Driver { scenario, bindings, conns: HashMap::new(), incoming };
    for (index, step) in scenario.steps.iter().enumerate() {
        driver.step(step).await.map_err(|error| format!("step {}: {error}", index + 1))?;
    }
    driver.expect_nothing_more().await.map_err(|error| format!("after the last step: {error}"))?;
    Ok(OpenSessions(driver.conns))
}

impl Driver<'_> {
    async fn step(&mut self, step: &Step) -> Result<(), String> {
        match step {
            Step::Open { judge, expect, .. } => self.accept(judge, expect).await,
            Step::Judge { from, message, repeat } => {
                let expected = expand(message, self.bindings)?;
                expect_judge(self.conns.entry(from.clone()).or_default(), &expected, *repeat).await
            }
            Step::Bridge { to, message } => self.send_bridge(to, message).await,
            Step::Closed { judge, status } => {
                close(self.conns.entry(judge.clone()).or_default(), status).await
            }
            Step::Drop { judge } => {
                self.conns.remove(judge);
                Ok(())
            }
            Step::Quiet { ms, .. } | Step::Wait { ms } => {
                tokio::time::sleep(Duration::from_millis(*ms)).await;
                Ok(())
            }
            Step::Fetch { judge, reference, .. } => self.expect_fetch(judge, reference).await,
            Step::Api(_) | Step::RestartBridge {} => Ok(()),
        }
    }

    async fn send_bridge(&mut self, to: &str, message: &Value) -> Result<(), String> {
        let mut message = parse_bridge(message, self.bindings)?;
        // the scenario writes "*" for a session id; the daemon gets the one the transcript binds
        if let Some(Body::Welcome(welcome)) = &mut message.body
            && welcome.session_id == "*"
        {
            welcome.session_id = self.bindings.get(&format!("session:{to}")).cloned().unwrap_or_default();
        }
        let outbound = self.conns.get(to).and_then(|conn| conn.outbound.clone());
        let outbound = outbound.ok_or(format!("{to} has no session"))?;
        outbound.send(Ok(message)).await.map_err(|_| format!("{to} went away"))
    }

    async fn accept(&mut self, judge: &str, expect: &str) -> Result<(), String> {
        let identity = self.scenario.identity(judge);
        match within("a session", self.incoming.recv()).await? {
            Some(Incoming::Session { judge: name, inbound, outbound })
                if name == identity && expect == "OK" =>
            {
                let conn = Conn { inbound: Some(inbound), outbound: Some(outbound) };
                self.conns.insert(judge.to_string(), conn);
                Ok(())
            }
            Some(Incoming::Refused { judge: name }) if name == identity && expect != "OK" => Ok(()),
            Some(Incoming::Refused { judge: name }) if name == identity => {
                Err(format!("{judge} was refused (is its x-judge-key the scenario's?), expected a session"))
            }
            other => {
                Err(format!("expected {judge} to open a session answered {expect}, got {}", describe(&other)))
            }
        }
    }

    /// The `fetch` step: the daemon must call FetchSource for this ref with its session's id.
    async fn expect_fetch(&mut self, judge: &str, reference: &Value) -> Result<(), String> {
        let Some(Incoming::Fetch { judge: name, request, authenticated }) =
            within("a FetchSource call", self.incoming.recv()).await?
        else {
            return Err(format!("expected {judge} to call FetchSource"));
        };
        if name != self.scenario.identity(judge) || !authenticated {
            return Err(format!(
                "FetchSource came from {name} (key accepted: {authenticated}), expected {judge}"
            ));
        }
        let seen = serde_json::to_value(&request).map_err(|error| error.to_string())?;
        let expected = serde_json::json!({
            "ref": expand(reference, self.bindings)?,
            "sessionId": self.bindings.get(&format!("session:{judge}")).cloned().unwrap_or_default(),
        });
        matches(&expected, &seen).map_err(|error| format!("FetchSource request: {error}"))
    }

    /// No judge message, session or call may follow the last step.
    async fn expect_nothing_more(&mut self) -> Result<(), String> {
        tokio::time::sleep(TAIL_WINDOW).await;
        if let Ok(call) = self.incoming.try_recv() {
            return Err(format!("unexpected {}", describe(&Some(call))));
        }
        for (judge, conn) in &mut self.conns {
            let Some(inbound) = conn.inbound.as_mut() else { continue };
            if let Ok(Some(Ok(message))) = tokio::time::timeout(TAIL_POLL, inbound.next()).await {
                return Err(format!("{judge} sent {message:?}"));
            }
        }
        Ok(())
    }
}

fn describe(incoming: &Option<Incoming>) -> String {
    match incoming {
        None => "nothing".into(),
        Some(Incoming::Session { judge, .. }) => format!("a session of {judge}"),
        Some(Incoming::Refused { judge }) => format!("a refused session of {judge}"),
        Some(Incoming::Fetch { judge, .. }) => format!("a FetchSource call of {judge}"),
        Some(Incoming::Unscripted { judge, call }) => format!("an unscripted {call} call of {judge}"),
    }
}

async fn expect_judge(conn: &mut Conn, expected: &Value, repeat: u32) -> Result<(), String> {
    for _ in 0..repeat {
        matches(expected, &next_judge_message(conn).await?)?;
    }
    Ok(())
}

/// Ends the session with `status`; `OK` ends the response stream cleanly.
async fn close(conn: &mut Conn, status: &str) -> Result<(), String> {
    let code = code_of(status)?;
    if let Some(outbound) = conn.outbound.take()
        && code != tonic::Code::Ok
    {
        let _ = outbound.send(Err(Status::new(code, "closed by the scenario"))).await;
    }
    conn.inbound = None;
    Ok(())
}

async fn next_judge_message(conn: &mut Conn) -> Result<Value, String> {
    let inbound = conn.inbound.as_mut().ok_or("no open session")?;
    match within("a judge message", inbound.next()).await? {
        Some(Ok(message)) => serde_json::to_value(message).map_err(|error| error.to_string()),
        Some(Err(status)) => Err(format!("session failed: {status}")),
        None => Err("session ended".into()),
    }
}
