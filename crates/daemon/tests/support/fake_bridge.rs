//! A scripted bridge: records what the judge sends, lets the test send per session.
use judge_proto::v1::judge_link_server::{JudgeLink, JudgeLinkServer};
use judge_proto::v1::{
    BridgeMessage, Chunk, FetchSourceRequest, GenerationRequest, JudgeMessage, UploadChunk, UploadReceipt,
};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use tokio_stream::{
    Stream, StreamExt,
    wrappers::{TcpListenerStream, UnboundedReceiverStream},
};
use tonic::{Request, Response, Status, Streaming};

type Out = mpsc::UnboundedSender<Result<BridgeMessage, Status>>;

/// One upload: the first message's generation id and session id, then all the data.
pub type Upload = (String, String, Vec<u8>);

#[derive(Clone, Default)]
pub struct Shared {
    pub sessions: Arc<Mutex<Vec<Out>>>,
    pub received: Arc<Mutex<Vec<(usize, JudgeMessage)>>>,
    pub credentials: Arc<Mutex<Vec<(String, String)>>>,
    /// What FetchGenerationBundle serves, in chunks of 4 bytes.
    pub bundle: Arc<Mutex<Vec<u8>>>,
    /// Per UploadGeneratedTests call: the first message's generation and session ids, and all the data.
    pub uploads: Arc<Mutex<Vec<Upload>>>,
}

struct Service(Shared);

#[tonic::async_trait]
impl JudgeLink for Service {
    type SessionStream = Pin<Box<dyn Stream<Item = Result<BridgeMessage, Status>> + Send>>;
    type FetchSourceStream = Pin<Box<dyn Stream<Item = Result<Chunk, Status>> + Send>>;
    type FetchGenerationBundleStream = Pin<Box<dyn Stream<Item = Result<Chunk, Status>> + Send>>;

    async fn session(
        &self,
        request: Request<Streaming<JudgeMessage>>,
    ) -> Result<Response<Self::SessionStream>, Status> {
        let header =
            |k: &str| request.metadata().get(k).and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
        self.0.credentials.lock().unwrap().push((header("x-judge-name"), header("x-judge-key")));
        let (tx, rx) = mpsc::unbounded_channel();
        let session = {
            let mut s = self.0.sessions.lock().unwrap();
            s.push(tx);
            s.len() - 1
        };
        let received = self.0.received.clone();
        let mut inbound = request.into_inner();
        tokio::spawn(async move {
            while let Some(Ok(message)) = inbound.next().await {
                received.lock().unwrap().push((session, message));
            }
        });
        Ok(Response::new(Box::pin(UnboundedReceiverStream::new(rx))))
    }

    async fn fetch_source(
        &self,
        _request: Request<FetchSourceRequest>,
    ) -> Result<Response<Self::FetchSourceStream>, Status> {
        let chunks = vec![Ok(Chunk { data: b"print(".to_vec() }), Ok(Chunk { data: b"3)\n".to_vec() })];
        Ok(Response::new(Box::pin(tokio_stream::iter(chunks))))
    }

    async fn fetch_generation_bundle(
        &self,
        _request: Request<GenerationRequest>,
    ) -> Result<Response<Self::FetchGenerationBundleStream>, Status> {
        let bundle = self.0.bundle.lock().unwrap().clone();
        let chunks: Vec<_> = bundle.chunks(4).map(|part| Ok(Chunk { data: part.to_vec() })).collect();
        Ok(Response::new(Box::pin(tokio_stream::iter(chunks))))
    }

    async fn upload_generated_tests(
        &self,
        request: Request<Streaming<UploadChunk>>,
    ) -> Result<Response<UploadReceipt>, Status> {
        let mut inbound = request.into_inner();
        let (mut ids, mut data) = (None, Vec::new());
        while let Some(chunk) = inbound.next().await {
            let chunk = chunk?;
            ids.get_or_insert((chunk.generation_id, chunk.session_id));
            data.extend(chunk.data);
        }
        let (generation, session) = ids.unwrap_or_default();
        let bytes = data.len() as u64;
        self.0.uploads.lock().unwrap().push((generation, session, data));
        Ok(Response::new(UploadReceipt { bytes }))
    }
}

pub async fn start() -> (String, Shared) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let shared = Shared::default();
    let service = JudgeLinkServer::new(Service(shared.clone()));
    tokio::spawn(
        tonic::transport::Server::builder()
            .add_service(service)
            .serve_with_incoming(TcpListenerStream::new(listener)),
    );
    (url, shared)
}

impl Shared {
    pub fn send(&self, session: usize, message: BridgeMessage) {
        self.sessions.lock().unwrap()[session].send(Ok(message)).unwrap();
    }
    /// Ends a session's stream: the judge sees a disconnect.
    pub fn hang_up(&self, session: usize) {
        let (dead, _) = mpsc::unbounded_channel();
        self.sessions.lock().unwrap()[session] = dead;
    }
    pub fn session_count(&self) -> usize {
        self.sessions.lock().unwrap().len()
    }
    pub fn messages(&self, session: usize) -> Vec<JudgeMessage> {
        self.received.lock().unwrap().iter().filter(|(s, _)| *s == session).map(|(_, m)| m.clone()).collect()
    }
}

/// Polls until `check` holds (5 s max).
pub async fn eventually(mut check: impl FnMut() -> bool) {
    for _ in 0..500 {
        if check() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("condition not reached in 5 s");
}
