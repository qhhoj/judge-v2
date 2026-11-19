//! Dev-only gate: while `hold-ack` exists in `control_dir`, a Judging pauses before `Accepted`, so a test can abort or
//! reassign it mid-run (conformance scenario 28b). Config refuses it off loopback.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

const POLL: Duration = Duration::from_millis(50);

#[derive(Debug, Clone, Default)]
pub struct Gates {
    pub dir: Option<PathBuf>,
}

impl Gates {
    fn held(&self) -> bool {
        self.dir.as_ref().is_some_and(|dir| dir.join("hold-ack").exists())
    }

    /// Returns once `hold-ack` is gone or the job is aborted.
    pub async fn wait_for_ack(&self, abort: &AtomicBool) {
        while self.held() && !abort.load(Ordering::Relaxed) {
            tokio::time::sleep(POLL).await;
        }
    }
}
