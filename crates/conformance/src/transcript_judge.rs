//! The judge role of a transcript: a tonic client that sends the scenario's
//! `judge` steps and checks its `bridge` steps. Self-play runs it against the
//! fake bridge; the backend's TypeScript fake judge mirrors it.
use std::{collections::HashMap, time::Duration};

use judge_proto::v1::{
    AssignmentRef, BridgeMessage, Chunk, FetchSourceRequest, JudgeMessage, bridge_message::Body,
    judge_link_client::JudgeLinkClient,
};
use tokio::sync::mpsc;
use tokio_stream::{StreamExt, wrappers::ReceiverStream};
use tonic::{Request, Streaming, metadata::MetadataValue, transport::Channel};

use crate::{
    expand::{Bindings, expand},
    matcher::matches,
    play::{code_of, key_of, within},
    scenario::{Scenario, Step, parse_judge},
};

/// Sessions still open after the last step. Dropping it cancels them, so a
/// caller keeps it until the bridge role has read everything.
pub struct OpenSessions(#[allow(dead_code)] HashMap<String, Conn>);

#[derive(Default)]
pub struct Conn {
    sender: Option<mpsc::Sender<JudgeMessage>>,
    inbound: Option<Streaming<BridgeMessage>>,
    /// From the last Welcome; FetchSource must carry it (the bridge checks the calling session).
    session_id: String,
}

pub async fn play_judge(
    scenario: &Scenario,
    bindings: &Bindings,
    endpoint: String,
) -> Result<OpenSessions, String> {
    let client = JudgeLinkClient::connect(endpoint).await.map_err(|error| error.to_string())?;
    let mut player = Player { scenario, bindings, client, conns: HashMap::new() };
    for (index, step) in scenario.steps.iter().enumerate() {
        player.step(step).await.map_err(|error| format!("step {}: {error}", index + 1))?;
    }
    Ok(OpenSessions(player.conns))
}

/// Everything the judge role needs while it walks the steps.
struct Player<'a> {
    scenario: &'a Scenario,
    bindings: &'a Bindings,
    client: JudgeLinkClient<Channel>,
    conns: HashMap<String, Conn>,
}

fn with_credentials<T>(message: T, judge: &str, wrong_key: bool) -> Result<Request<T>, String> {
    let mut request = Request::new(message);
    let key = if wrong_key { "not-the-key".to_string() } else { key_of(judge) };
    let parse = |text: &str| text.parse::<MetadataValue<_>>().map_err(|error| error.to_string());
    request.metadata_mut().insert("x-judge-name", parse(judge)?);
    request.metadata_mut().insert("x-judge-key", parse(&key)?);
    Ok(request)
}

impl Player<'_> {
    async fn step(&mut self, step: &Step) -> Result<(), String> {
        match step {
            Step::Open { judge, wrong_key, expect } => {
                let identity = self.scenario.identity(judge);
                if let Some(conn) = open(self.client.clone(), identity, *wrong_key, expect).await? {
                    self.conns.insert(judge.clone(), conn);
                }
                Ok(())
            }
            Step::Judge { from, message, repeat } => {
                let conn = self.conns.get(from).ok_or(format!("{from} has no session"))?;
                send(conn, &parse_judge(message, self.bindings)?, *repeat).await
            }
            Step::Bridge { to, message } => {
                expect_bridge(self.conns.entry(to.clone()).or_default(), &expand(message, self.bindings)?)
                    .await
            }
            Step::Quiet { to, ms } => quiet(self.conns.entry(to.clone()).or_default(), *ms).await,
            Step::Closed { judge, status } => {
                closed(self.conns.entry(judge.clone()).or_default(), status).await
            }
            Step::Drop { judge } => {
                self.conns.remove(judge);
                Ok(())
            }
            Step::Fetch { judge, reference, expect, bytes } => {
                self.fetch(judge, reference, (expect, *bytes)).await
            }
            Step::Wait { ms } => {
                tokio::time::sleep(Duration::from_millis(*ms)).await;
                Ok(())
            }
            Step::Api(_) | Step::RestartBridge {} => Ok(()),
        }
    }

    async fn fetch(
        &mut self,
        judge: &str,
        reference: &serde_json::Value,
        outcome: (&str, u64),
    ) -> Result<(), String> {
        let session_id = self.conns.get(judge).map(|conn| conn.session_id.clone()).unwrap_or_default();
        let caller = (self.scenario.identity(judge), session_id.as_str());
        fetch(self.client.clone(), caller, &expand(reference, self.bindings)?, outcome).await
    }
}

async fn send(conn: &Conn, message: &JudgeMessage, repeat: u32) -> Result<(), String> {
    let sender = conn.sender.as_ref().ok_or("the session is closed")?;
    for _ in 0..repeat {
        sender.send(message.clone()).await.map_err(|_| "the session is closed".to_string())?;
    }
    Ok(())
}

/// The open session, or None when the scenario expects (and got) a refusal.
async fn open(
    mut client: JudgeLinkClient<Channel>,
    judge: &str,
    wrong_key: bool,
    expect: &str,
) -> Result<Option<Conn>, String> {
    let (sender, receiver) = mpsc::channel(64);
    let request = with_credentials(ReceiverStream::new(receiver), judge, wrong_key)?;
    let answer = within("the session answer", client.session(request)).await?;
    let expected = code_of(expect)?;
    match answer {
        Ok(response) if expected == tonic::Code::Ok => {
            Ok(Some(Conn { sender: Some(sender), inbound: Some(response.into_inner()), ..Conn::default() }))
        }
        Ok(response) => {
            let mut conn = Conn { inbound: Some(response.into_inner()), ..Conn::default() };
            closed(&mut conn, expect).await.map(|()| None)
        }
        Err(status) if status.code() == expected => Ok(None),
        Err(status) => Err(format!("session refused with {status}, expected {expect}")),
    }
}

async fn expect_bridge(conn: &mut Conn, expected: &serde_json::Value) -> Result<(), String> {
    let seen = next_bridge_message(conn).await?.ok_or("session ended")?;
    if let Some(Body::Welcome(welcome)) = &seen.body {
        conn.session_id = welcome.session_id.clone();
    }
    matches(expected, &serde_json::to_value(seen).map_err(|error| error.to_string())?)
}

/// The next bridge message that is not a Ping; None when the session ended cleanly.
async fn next_bridge_message(conn: &mut Conn) -> Result<Option<BridgeMessage>, String> {
    let inbound = conn.inbound.as_mut().ok_or("no open session")?;
    loop {
        match within("a bridge message", inbound.next()).await? {
            Some(Ok(BridgeMessage { body: Some(Body::Ping(_)) })) => continue,
            Some(Ok(message)) => return Ok(Some(message)),
            Some(Err(status)) => return Err(format!("session failed: {status}")),
            None => return Ok(None),
        }
    }
}

async fn quiet(conn: &mut Conn, ms: u64) -> Result<(), String> {
    match tokio::time::timeout(Duration::from_millis(ms), next_bridge_message(conn)).await {
        Err(_) => Ok(()),
        Ok(seen) => Err(format!("expected silence, got {seen:?}")),
    }
}

/// The session must end next (pings aside) with `status`.
async fn closed(conn: &mut Conn, status: &str) -> Result<(), String> {
    let expected = code_of(status)?;
    let inbound = conn.inbound.as_mut().ok_or("no open session")?;
    let result = loop {
        match within("the session end", inbound.next()).await? {
            Some(Ok(BridgeMessage { body: Some(Body::Ping(_)) })) => continue,
            Some(Ok(message)) => break Err(format!("expected the end, got {message:?}")),
            Some(Err(seen)) if seen.code() == expected => break Ok(()),
            Some(Err(seen)) => break Err(format!("ended with {seen}, expected {status}")),
            None if expected == tonic::Code::Ok => break Ok(()),
            None => break Err(format!("ended cleanly, expected {status}")),
        }
    };
    *conn = Conn::default();
    result
}

/// `caller` is the judge name and its session id from Welcome.
async fn fetch(
    mut client: JudgeLinkClient<Channel>,
    (judge, session_id): (&str, &str),
    reference: &serde_json::Value,
    (expect, bytes): (&str, u64),
) -> Result<(), String> {
    let reference: AssignmentRef =
        serde_json::from_value(reference.clone()).map_err(|error| error.to_string())?;
    let message = FetchSourceRequest { r#ref: Some(reference), session_id: session_id.to_string() };
    let (outcome, received) = match client.fetch_source(with_credentials(message, judge, false)?).await {
        Ok(response) => drain(response.into_inner()).await?,
        Err(status) => (status.code(), 0),
    };
    if outcome != code_of(expect)? || (outcome == tonic::Code::Ok && received != bytes) {
        return Err(format!(
            "fetch answered {outcome:?} with {received} bytes, expected {expect} with {bytes}"
        ));
    }
    Ok(())
}

/// How a chunk stream ended and how many bytes it carried.
async fn drain(mut stream: Streaming<Chunk>) -> Result<(tonic::Code, u64), String> {
    let mut received = 0u64;
    loop {
        match within("a chunk", stream.next()).await? {
            Some(Ok(chunk)) => received += chunk.data.len() as u64,
            Some(Err(status)) => return Ok((status.code(), received)),
            None => return Ok((tonic::Code::Ok, received)),
        }
    }
}
