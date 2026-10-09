//! SIGTERM drain: finish what the judge holds, up to a deadline (spec §7 "Drain").

use std::time::{Duration, Instant};

/// True when `done` held before `timeout`; polls every `poll`.
pub async fn wait_until(done: impl Fn() -> bool, timeout: Duration, poll: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if done() {
            return true;
        }
        tokio::time::sleep(poll).await;
    }
    done()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    #[tokio::test]
    async fn finishes_when_done_and_times_out_otherwise() {
        let polls = AtomicU32::new(0);
        let done = || polls.fetch_add(1, Ordering::SeqCst) >= 3;
        assert!(wait_until(done, Duration::from_secs(1), Duration::from_millis(5)).await);
        assert_eq!(polls.load(Ordering::SeqCst), 4, "stops polling once done");
        let started = std::time::Instant::now();
        assert!(!wait_until(|| false, Duration::from_millis(100), Duration::from_millis(10)).await);
        assert!(started.elapsed() >= Duration::from_millis(100));
    }
}
