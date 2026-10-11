//! A test generation on one slot: fetch the bundle, run the grader's generation, upload the result zip,
//! send `GenerationDone`. Not in the per-run outbox: a generation belongs to the session that assigned
//! it, and a lost session ends it (the bridge fails the row).

use crate::config::Config;
use crate::link::{Connection, MAX_MESSAGE, authed};
use crate::source::save_chunks;
use crate::wire::{self, JudgeMessage};
use crate::worker::{Release, Runner, WorkerDeps, aborted};
use judge_grader::generate::{Control, GenerationFailure, GenerationJob, GenerationPlan, Kind};
use judge_proto::v1::judge_link_client::JudgeLinkClient;
use judge_proto::v1::{GenerationRequest, UploadChunk};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::sync::{RwLock, mpsc};
use tokio::time::Instant;
use tokio_stream::wrappers::ReceiverStream;

/// The most a bundle may be (`GenerateTests.bundle_size`).
pub const MAX_BUNDLE_BYTES: u64 = 512 << 20;
const UPLOAD_CHUNK_BYTES: usize = 1 << 20;

/// What a generation is called on the wire: the job and the session that assigned it.
#[derive(Debug, Clone)]
pub struct Ticket {
    pub generation_id: String,
    pub session_id: String,
}

#[tonic::async_trait]
pub trait GenerationLink: Send + Sync {
    /// Writes the bundle to `dest`; more than `max_bytes` is an error.
    async fn fetch_bundle(&self, ticket: &Ticket, max_bytes: u64, dest: &Path) -> Result<(), String>;
    /// Uploads the file; the bridge's receipt must name every byte.
    async fn upload(&self, ticket: &Ticket, file: &Path) -> Result<(), String>;
    /// Sends a message on the session, if it is still the one that assigned the generation.
    async fn send(&self, ticket: &Ticket, message: JudgeMessage) -> Result<(), String>;
}

pub struct GrpcGeneration {
    pub config: Arc<Config>,
    pub connection: Arc<RwLock<Option<Connection>>>,
}

impl GrpcGeneration {
    async fn live(&self, ticket: &Ticket) -> Result<Connection, String> {
        let connection = self.connection.read().await.clone().ok_or("no bridge session")?;
        if connection.session_id != ticket.session_id {
            return Err("the session that assigned the generation has ended".into());
        }
        Ok(connection)
    }

    async fn client(&self, ticket: &Ticket) -> Result<JudgeLinkClient<tonic::transport::Channel>, String> {
        let connection = self.live(ticket).await?;
        Ok(JudgeLinkClient::new(connection.channel).max_decoding_message_size(MAX_MESSAGE))
    }
}

/// 1 MiB messages of the file; the first one names the generation and the session.
async fn read_chunks(path: PathBuf, ticket: Ticket, out: mpsc::Sender<UploadChunk>) {
    let Ok(mut file) = tokio::fs::File::open(path).await else { return };
    let mut first = true;
    loop {
        let mut data = vec![0u8; UPLOAD_CHUNK_BYTES];
        let Ok(read) = file.read(&mut data).await else { return };
        if read == 0 {
            return;
        }
        data.truncate(read);
        let (generation_id, session_id) = if first {
            (ticket.generation_id.clone(), ticket.session_id.clone())
        } else {
            Default::default()
        };
        first = false;
        if out.send(UploadChunk { generation_id, session_id, data }).await.is_err() {
            return;
        }
    }
}

#[tonic::async_trait]
impl GenerationLink for GrpcGeneration {
    async fn fetch_bundle(&self, ticket: &Ticket, max_bytes: u64, dest: &Path) -> Result<(), String> {
        let mut client = self.client(ticket).await?;
        let message = GenerationRequest {
            generation_id: ticket.generation_id.clone(),
            session_id: ticket.session_id.clone(),
        };
        let request = authed(&self.config, message).map_err(|e| e.to_string())?;
        let response = client.fetch_generation_bundle(request).await.map_err(|status| status.to_string())?;
        save_chunks(&mut response.into_inner(), dest, max_bytes).await
    }

    async fn upload(&self, ticket: &Ticket, file: &Path) -> Result<(), String> {
        let mut client = self.client(ticket).await?;
        let size = tokio::fs::metadata(file).await.map_err(|e| e.to_string())?.len();
        let (out, chunks) = mpsc::channel(2);
        tokio::spawn(read_chunks(file.to_path_buf(), ticket.clone(), out));
        let request = authed(&self.config, ReceiverStream::new(chunks)).map_err(|e| e.to_string())?;
        let receipt = client.upload_generated_tests(request).await.map_err(|status| status.to_string())?;
        match receipt.into_inner().bytes {
            bytes if bytes == size => Ok(()),
            bytes => Err(format!("the bridge received {bytes} of {size} bytes")),
        }
    }

    async fn send(&self, ticket: &Ticket, message: JudgeMessage) -> Result<(), String> {
        let connection = self.live(ticket).await?;
        connection.outbound.send(message).await.map_err(|_| "the session closed".to_string())
    }
}

/// A generation on a slot; `release` frees the slot right before the result is sent.
pub struct GenerationAssignment {
    pub plan: GenerationPlan,
    pub session_id: String,
    pub abort: Arc<AtomicBool>,
    pub release: Release,
}

/// Where a job's files go in the daemon's scratch directory.
struct Files {
    bundle: PathBuf,
    result: PathBuf,
}

fn files_of(deps: &WorkerDeps, generation_id: &str) -> Files {
    let name: String =
        generation_id.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
    Files {
        bundle: deps.scratch.join(format!("generation-{name}.zip")),
        result: deps.scratch.join(format!("generation-{name}-result.zip")),
    }
}

/// `work` unless the job is cancelled or its deadline passes first.
async fn bounded<T>(
    job: &GenerationAssignment,
    deadline: Instant,
    work: impl Future<Output = Result<T, GenerationFailure>>,
) -> Result<T, GenerationFailure> {
    tokio::select! {
        done = tokio::time::timeout_at(deadline, work) => done.unwrap_or_else(|_| {
            Err(GenerationFailure::job(Kind::JobTimeLimit, "the generation deadline passed"))
        }),
        _ = aborted(&job.abort) => Err(GenerationFailure::cancelled()),
    }
}

fn transfer_failure(what: &str, error: String) -> GenerationFailure {
    GenerationFailure::internal(format!("{what} failed: {error}"))
}

async fn fetch_bundle(
    deps: &WorkerDeps,
    (job, ticket): (&GenerationAssignment, &Ticket),
    (files, deadline): (&Files, Instant),
) -> Result<(), GenerationFailure> {
    let bundle_size = job.plan.bundle_size;
    if bundle_size > MAX_BUNDLE_BYTES {
        let detail = format!("the bundle is {bundle_size} bytes, over {MAX_BUNDLE_BYTES}");
        return Err(GenerationFailure::job(Kind::OutputLimit, detail));
    }
    let fetch = deps.generation.fetch_bundle(ticket, bundle_size, &files.bundle);
    bounded(job, deadline, async { fetch.await.map_err(|e| transfer_failure("the bundle fetch", e)) }).await
}

/// The grader's generation on a blocking thread; a panic is an internal failure and the slot keeps its runner.
async fn generate(
    mut runner: Box<dyn Runner>,
    (job, deadline): (&GenerationAssignment, Instant),
    files: &Files,
) -> (Box<dyn Runner>, Result<(), GenerationFailure>) {
    let grading = GenerationJob {
        plan: job.plan.clone(),
        bundle_zip: files.bundle.clone(),
        result_zip: files.result.clone(),
    };
    let abort = job.abort.clone();
    let blocking = tokio::task::spawn_blocking(move || {
        let control = Control { deadline: deadline.into_std(), abort: &abort };
        let ran = catch_unwind(AssertUnwindSafe(|| runner.generate(&grading, &control)));
        let failed = || Err(GenerationFailure::internal("the generator panicked"));
        (runner, ran.unwrap_or_else(|_| failed()))
    });
    blocking.await.expect("the generation thread is joined")
}

async fn upload_result(
    deps: &WorkerDeps,
    (job, ticket): (&GenerationAssignment, &Ticket),
    (files, deadline): (&Files, Instant),
) -> Result<(), GenerationFailure> {
    let upload = deps.generation.upload(ticket, &files.result);
    bounded(job, deadline, async { upload.await.map_err(|e| transfer_failure("the upload", e)) }).await
}

async fn stages(
    deps: &WorkerDeps,
    runner: Box<dyn Runner>,
    (job, ticket): (&GenerationAssignment, &Ticket),
    (files, deadline): (&Files, Instant),
) -> (Box<dyn Runner>, Result<(), GenerationFailure>) {
    if job.abort.load(Ordering::SeqCst) {
        return (runner, Err(GenerationFailure::cancelled()));
    }
    if let Err(failure) = fetch_bundle(deps, (job, ticket), (files, deadline)).await {
        return (runner, Err(failure));
    }
    let (runner, generated) = generate(runner, (job, deadline), files).await;
    if generated.is_err() {
        return (runner, generated);
    }
    (runner, upload_result(deps, (job, ticket), (files, deadline)).await)
}

/// The result goes out with its detail clipped to the bridge's log limit. A cancelled job says nothing:
/// the bridge asked for it, or the session it would answer on is gone.
async fn report(
    deps: &WorkerDeps,
    (job, ticket): (&GenerationAssignment, &Ticket),
    result: Result<(), GenerationFailure>,
) {
    if job.abort.load(Ordering::SeqCst) {
        return;
    }
    let result = result.map_err(|mut failure| {
        failure.detail = deps.outboxes.limits().clip_log(&failure.detail);
        failure
    });
    tracing::info!(judge = %deps.judge, generation_id = %ticket.generation_id, ok = result.is_ok(), "generation finished");
    let message = wire::generation_done(&ticket.generation_id, &result);
    if let Err(error) = deps.generation.send(ticket, message).await {
        tracing::warn!(judge = %deps.judge, generation_id = %ticket.generation_id, %error, "GenerationDone not sent");
    }
}

pub async fn process_generation(
    deps: &WorkerDeps,
    runner: Box<dyn Runner>,
    job: GenerationAssignment,
) -> Box<dyn Runner> {
    let ticket = Ticket { generation_id: job.plan.id.clone(), session_id: job.session_id.clone() };
    let deadline = Instant::now() + Duration::from_secs(job.plan.deadline_s.into());
    let files = files_of(deps, &ticket.generation_id);
    tracing::info!(judge = %deps.judge, generation_id = %ticket.generation_id, tests = job.plan.tests.len(), "generation started");
    let (runner, result) = stages(deps, runner, (&job, &ticket), (&files, deadline)).await;
    let _ = tokio::fs::remove_file(&files.bundle).await;
    let _ = tokio::fs::remove_file(&files.result).await;
    (job.release)();
    report(deps, (&job, &ticket), result).await;
    runner
}
