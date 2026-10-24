//! Dev-only gates: while a hold file (`hold-ack`, `hold-begin` or `hold-end`) exists in `control_dir`, a Judging pauses
//! before `Accepted`, `GradingStarted` or `Final`, so the real-backend end-to-end journey can abort or rejudge it
//! mid-run. Config refuses them off loopback.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

const POLL: Duration = Duration::from_millis(50);

#[derive(Debug, Clone, Copy)]
pub enum Stage {
    Ack,
    Begin,
    End,
}

#[derive(Debug, Clone, Default)]
pub struct Gates {
    pub dir: Option<PathBuf>,
}

impl Gates {
    fn held(&self, stage: Stage) -> bool {
        let name = match stage {
            Stage::Ack => "hold-ack",
            Stage::Begin => "hold-begin",
            Stage::End => "hold-end",
        };
        self.dir.as_ref().is_some_and(|dir| dir.join(name).exists())
    }

    pub async fn wait(&self, stage: Stage, abort: &AtomicBool) {
        while self.held(stage) && !abort.load(Ordering::Relaxed) {
            tokio::time::sleep(POLL).await;
        }
    }

    pub fn wait_blocking(&self, stage: Stage, abort: &AtomicBool) {
        while self.held(stage) && !abort.load(Ordering::Relaxed) {
            std::thread::sleep(POLL);
        }
    }
}
