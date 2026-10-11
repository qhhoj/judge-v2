//! JSON lines to stdout; journald stores them (spec §7 "Logs").

use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::MakeWriter;

/// JSON lines to `writer`, filtered by `RUST_LOG` (default `info`).
pub fn json_subscriber<W>(writer: W) -> impl tracing::Subscriber + Send + Sync
where
    W: for<'a> MakeWriter<'a> + Send + Sync + 'static,
{
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .with_current_span(false)
        .with_writer(writer)
        .finish()
}

/// Installs the global subscriber. The start line names the judge; assignment lines carry `judge`
/// themselves (the worker adds it), so one journal can hold several judges.
pub fn init(judge_name: &str) {
    let subscriber = json_subscriber(std::io::stdout);
    if tracing::subscriber::set_global_default(subscriber).is_ok() {
        tracing::info!(judge = judge_name, version = env!("CARGO_PKG_VERSION"), "qhhoj-judge starting");
    }
}
