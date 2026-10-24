//! File-only submissions (OUTPUT zips) come over FetchSource, naming the live session (docs/protocol.md#source-and-generation-transfer).

use crate::config::Config;
use crate::link::{Connection, MAX_MESSAGE, authed};
use crate::outbox::AssignmentRef;
use judge_proto::v1::judge_link_client::JudgeLinkClient;
use judge_proto::v1::{AssignmentRef as ProtoRef, Chunk, FetchSourceRequest};
use std::path::Path;
use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use tokio::sync::RwLock;
use tonic::Streaming;

pub const MAX_FILE_SOURCE: u64 = 64 << 20;

#[tonic::async_trait]
pub trait SourceFetch: Send + Sync {
    /// Writes the source of `r` to `dest`; more than `max_bytes` (capped at 64 MiB) is an error.
    async fn fetch(&self, r: &AssignmentRef, max_bytes: u64, dest: &Path) -> Result<(), String>;
}

pub struct GrpcFetch {
    pub config: Arc<Config>,
    pub connection: Arc<RwLock<Option<Connection>>>,
}

impl GrpcFetch {
    async fn open(&self, r: &AssignmentRef) -> Result<Streaming<Chunk>, String> {
        let connection = self.connection.read().await.clone().ok_or("no bridge session")?;
        let mut client = JudgeLinkClient::new(connection.channel).max_decoding_message_size(MAX_MESSAGE);
        let r#ref = Some(ProtoRef { submission_id: r.submission_id.clone(), generation: r.generation });
        let request = authed(&self.config, FetchSourceRequest { r#ref, session_id: connection.session_id })
            .map_err(|e| e.to_string())?;
        let response = client.fetch_source(request).await.map_err(|status| status.to_string())?;
        Ok(response.into_inner())
    }
}

#[tonic::async_trait]
impl SourceFetch for GrpcFetch {
    async fn fetch(&self, r: &AssignmentRef, max_bytes: u64, dest: &Path) -> Result<(), String> {
        let mut stream = self.open(r).await?;
        save_chunks(&mut stream, dest, max_bytes.min(MAX_FILE_SOURCE)).await
    }
}

/// Writes the stream to `dest`; more than `limit` bytes is an error.
pub async fn save_chunks(stream: &mut Streaming<Chunk>, dest: &Path, limit: u64) -> Result<(), String> {
    let mut file = tokio::fs::File::create(dest).await.map_err(|e| e.to_string())?;
    let mut total = 0u64;
    while let Some(chunk) = stream.message().await.map_err(|status| status.to_string())? {
        total += chunk.data.len() as u64;
        if total > limit {
            return Err(format!("larger than {limit} bytes"));
        }
        file.write_all(&chunk.data).await.map_err(|e| e.to_string())?;
    }
    file.flush().await.map_err(|e| e.to_string())
}
