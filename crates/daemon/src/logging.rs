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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    struct Buffer(Arc<Mutex<Vec<u8>>>);
    impl std::io::Write for Buffer {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn events_are_json_with_fields() {
        let buffer = Buffer::default();
        let writer = buffer.clone();
        let subscriber = json_subscriber(move || writer.clone());
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(submission_id = "s1", generation = 2, outcome = "graded", "assignment finished");
        });
        let line = String::from_utf8(buffer.0.lock().unwrap().clone()).unwrap();
        let json: serde_json::Value = serde_json::from_str(line.lines().next().unwrap()).unwrap();
        assert_eq!(json["fields"]["submission_id"], "s1");
        assert_eq!(json["fields"]["outcome"], "graded");
        assert_eq!(json["level"], "INFO");
    }
}
