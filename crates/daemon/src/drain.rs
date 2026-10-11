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
