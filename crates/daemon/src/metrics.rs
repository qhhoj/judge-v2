//! Prometheus text on loopback (spec §7 "Metrics").

use judge_grader::judge::{Outcome, RetryReason};
use prometheus::{
    Encoder, Histogram, HistogramOpts, HistogramVec, IntCounter, IntCounterVec, IntGauge, Opts, Registry,
    TextEncoder,
};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub struct Metrics {
    registry: Registry,
    pub assignments: IntCounterVec,
    pub case_seconds: Histogram,
    pub compile_seconds: HistogramVec,
    pub slots_busy: IntGauge,
    pub outbox_bytes: IntGauge,
    pub reconnects: IntCounter,
    pub sandbox_failures: IntCounterVec,
    pub cache_hits_total: IntGauge,
    pub data_wait: Histogram,
}

fn register<T: prometheus::core::Collector + Clone + 'static>(registry: &Registry, metric: T) -> T {
    registry.register(Box::new(metric.clone())).expect("unique metric name");
    metric
}

/// The `outcome` label of metrics and log lines.
pub fn outcome_label(outcome: &Outcome) -> &'static str {
    match outcome {
        Outcome::Graded => "graded",
        Outcome::CompileError(_) => "compile_error",
        Outcome::InternalError(_) => "internal_error",
        Outcome::Aborted => "aborted",
        Outcome::Retry(_) => "retry",
    }
}

impl Default for Metrics {
    fn default() -> Self {
        Self::new()
    }
}

const SECONDS: [f64; 10] = [0.01, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0];

fn histogram(r: &Registry, name: &str, help: &str) -> Histogram {
    register(r, Histogram::with_opts(HistogramOpts::new(name, help).buckets(SECONDS.to_vec())).unwrap())
}

fn counters(r: &Registry, name: &str, help: &str, label: &str) -> IntCounterVec {
    register(r, IntCounterVec::new(Opts::new(name, help), &[label]).unwrap())
}

fn gauge(r: &Registry, name: &str, help: &str) -> IntGauge {
    register(r, IntGauge::new(name, help).unwrap())
}

impl Metrics {
    pub fn new() -> Metrics {
        let r = Registry::new();
        let compile =
            HistogramOpts::new("judge_compile_seconds", "Compile wall seconds").buckets(SECONDS.to_vec());
        Metrics {
            assignments: counters(&r, "judge_assignments_total", "Finished assignments", "outcome"),
            case_seconds: histogram(&r, "judge_case_seconds", "CPU seconds per case"),
            compile_seconds: register(&r, HistogramVec::new(compile, &["lang"]).unwrap()),
            slots_busy: gauge(&r, "judge_slots_busy", "Busy slots"),
            outbox_bytes: gauge(&r, "judge_outbox_bytes", "Unacked outbox bytes"),
            reconnects: register(&r, IntCounter::new("judge_reconnects_total", "Bridge reconnects").unwrap()),
            sandbox_failures: counters(&r, "judge_sandbox_failures_total", "Sandbox failures", "kind"),
            cache_hits_total: gauge(&r, "judge_cache_hits_total", "Cache leases served without a build"),
            data_wait: histogram(&r, "judge_data_wait_seconds", "Problem resolve seconds"),
            registry: r,
        }
    }

    pub fn assignment_finished(&self, outcome: &Outcome) {
        self.assignments.with_label_values(&[outcome_label(outcome)]).inc();
        if matches!(outcome, Outcome::Retry(RetryReason::SandboxFailure)) {
            self.sandbox_failures.with_label_values(&["isolate"]).inc();
        }
    }

    pub fn render(&self) -> String {
        let mut buffer = Vec::new();
        TextEncoder::new().encode(&self.registry.gather(), &mut buffer).expect("text encoding");
        String::from_utf8(buffer).expect("utf-8")
    }
}

/// Answers every connection on `listen` with the rendered text, after `sample` refreshes sampled gauges.
pub async fn serve(listen: String, metrics: Arc<Metrics>, sample: Arc<dyn Fn(&Metrics) + Send + Sync>) {
    let listener = match tokio::net::TcpListener::bind(&listen).await {
        Ok(listener) => listener,
        Err(error) => return tracing::error!(%error, listen, "metrics listener failed"),
    };
    while let Ok((mut stream, _)) = listener.accept().await {
        sample(&metrics);
        let body = metrics.render();
        tokio::spawn(async move {
            let mut request = [0u8; 1024];
            let _ = stream.read(&mut request).await;
            let head = format!(
                "HTTP/1.0 200 OK\r\nContent-Type: text/plain; version=0.0.4\r\nContent-Length: {}\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(head.as_bytes()).await;
            let _ = stream.write_all(body.as_bytes()).await;
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use judge_grader::judge::{Outcome, RetryReason};

    #[test]
    fn outcomes_are_counted_by_label_and_rendered() {
        let m = Metrics::new();
        m.assignment_finished(&Outcome::Graded);
        m.assignment_finished(&Outcome::Graded);
        m.assignment_finished(&Outcome::Retry(RetryReason::SandboxFailure));
        let text = m.render();
        assert!(text.contains("judge_assignments_total{outcome=\"graded\"} 2"), "{text}");
        assert!(text.contains("judge_assignments_total{outcome=\"retry\"} 1"));
        assert!(text.contains("judge_sandbox_failures_total{kind=\"isolate\"} 1"));
        for name in [
            "judge_case_seconds",
            "judge_slots_busy",
            "judge_outbox_bytes",
            "judge_reconnects_total",
            "judge_data_wait_seconds",
            "judge_cache_hits_total",
        ] {
            assert!(text.contains(name), "{name} missing");
        }
    }

    #[tokio::test]
    async fn serves_text_with_sampled_gauges() {
        let m = Arc::new(Metrics::new());
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        tokio::spawn(serve(addr.to_string(), m.clone(), Arc::new(|m: &Metrics| m.slots_busy.set(3))));
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        stream.write_all(b"GET /metrics HTTP/1.0\r\n\r\n").await.unwrap();
        let mut body = String::new();
        stream.read_to_string(&mut body).await.unwrap();
        assert!(body.starts_with("HTTP/1.0 200 OK"));
        assert!(body.contains("judge_slots_busy 3"));
    }
}
