//! The bridge role of a transcript: a tonic `JudgeLink` server that sends the
//! scenario's `bridge` steps and checks its `judge` steps. judge-v2 points its
//! daemon at it; self-play points the transcript judge at it.
use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex},
};

use judge_proto::v1::{BridgeMessage, Chunk, FetchSourceRequest, JudgeMessage, judge_link_server::JudgeLink};
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_stream::{StreamExt, wrappers::ReceiverStream};
use tonic::{Request, Response, Status, Streaming};

use crate::{
    expand::{Bindings, expand},
    matcher::matches,
    play::{code_of, within},
    scenario::{Scenario, Step, parse_bridge},
};

const CHUNK_BYTES: u64 = 1024 * 1024;
type Outbound = mpsc::Sender<Result<BridgeMessage, Status>>;

pub enum Incoming {
    Session { judge: String, inbound: Box<Streaming<JudgeMessage>>, outbound: Outbound },
    Refused { judge: String },
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
                    answers.opens.entry(judge.clone()).or_default().push_back(expect.clone())
                }
                Step::Fetch { judge, expect, bytes, .. } => {
                    answers.fetches.entry(judge.clone()).or_default().push_back((expect.clone(), *bytes))
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

    async fn session(
        &self,
        request: Request<Streaming<JudgeMessage>>,
    ) -> Result<Response<Self::SessionStream>, Status> {
        let judge = judge_name(&request);
        let expect = self.next_answer(|answers| answers.opens.get_mut(&judge)?.pop_front());
        let expect = expect.unwrap_or_else(|| "OK".into());
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
        let (expect, bytes) = self
            .next_answer(|answers| answers.fetches.get_mut(&judge)?.pop_front())
            .ok_or_else(|| Status::failed_precondition("no fetch in the scenario"))?;
        if expect != "OK" {
            return Err(refusal(&expect));
        }
        Ok(Response::new(chunks(bytes)))
    }
}

/// Sessions still open after the last step; see `transcript_judge::OpenSessions`.
pub struct OpenSessions(#[allow(dead_code)] HashMap<String, Conn>);

#[derive(Default)]
pub struct Conn {
    inbound: Option<Box<Streaming<JudgeMessage>>>,
    outbound: Option<Outbound>,
}

/// Walks the steps in the bridge role; `incoming` comes from `FakeBridge::new`.
pub async fn play_bridge(
    scenario: &Scenario,
    bindings: &Bindings,
    mut incoming: mpsc::UnboundedReceiver<Incoming>,
) -> Result<OpenSessions, String> {
    let mut conns: HashMap<String, Conn> = HashMap::new();
    for (index, step) in scenario.steps.iter().enumerate() {
        bridge_step(step, bindings, &mut conns, &mut incoming)
            .await
            .map_err(|error| format!("step {}: {error}", index + 1))?;
    }
    Ok(OpenSessions(conns))
}

async fn bridge_step(
    step: &Step,
    bindings: &Bindings,
    conns: &mut HashMap<String, Conn>,
    incoming: &mut mpsc::UnboundedReceiver<Incoming>,
) -> Result<(), String> {
    match step {
        Step::Open { judge, expect, .. } => accept(judge, expect, conns, incoming).await,
        Step::Judge { from, message, repeat } => {
            let expected = expand(message, bindings)?;
            expect_judge(conns.entry(from.clone()).or_default(), &expected, *repeat).await
        }
        Step::Bridge { to, message } => {
            let message = parse_bridge(message, bindings)?;
            let outbound = conns.get(to).and_then(|conn| conn.outbound.clone());
            let outbound = outbound.ok_or(format!("{to} has no session"))?;
            outbound.send(Ok(message)).await.map_err(|_| format!("{to} went away"))
        }
        Step::Closed { judge, status } => close(conns.entry(judge.clone()).or_default(), status).await,
        Step::Drop { judge } => {
            conns.remove(judge);
            Ok(())
        }
        Step::Quiet { ms, .. } | Step::Wait { ms } => {
            tokio::time::sleep(std::time::Duration::from_millis(*ms)).await;
            Ok(())
        }
        Step::Fetch { .. } | Step::Api(_) | Step::RestartBridge {} => Ok(()),
    }
}

async fn accept(
    judge: &str,
    expect: &str,
    conns: &mut HashMap<String, Conn>,
    incoming: &mut mpsc::UnboundedReceiver<Incoming>,
) -> Result<(), String> {
    match within("a session", incoming.recv()).await? {
        Some(Incoming::Session { judge: name, inbound, outbound }) if name == judge && expect == "OK" => {
            conns.insert(name, Conn { inbound: Some(inbound), outbound: Some(outbound) });
            Ok(())
        }
        Some(Incoming::Refused { judge: name }) if name == judge && expect != "OK" => Ok(()),
        _ => Err(format!("expected {judge} to open a session answered {expect}")),
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
