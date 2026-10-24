//! How long this process has not been scheduled: the host's share of a step's wait.
//!
//! A step waits 5 s for the other side. On a starved host the whole process can stand still for
//! seconds, and that time is not the other side being slow. A plain thread (not a tokio task, so
//! a runtime blocked by the code under test is still counted against it) sleeps one tick at a
//! time; whatever it overslept is time the host did not run this process.
use std::{
    sync::{
        OnceLock,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

/// The stall clock's tick. Short, so a stall is measured to within a few ms; long enough that the
/// thread costs nothing next to a scenario. Normal timer slack adds well under 1 ms per second of wait.
const TICK: Duration = Duration::from_millis(10);
/// After a deadline passes, the stall thread may not have run yet to record the stall that just ended;
/// two ticks let it.
pub const SETTLE: Duration = Duration::from_millis(20);

/// Microseconds the stall thread overslept since it started.
static STALLED_MICROS: AtomicU64 = AtomicU64::new(0);
static STARTED: OnceLock<()> = OnceLock::new();

/// Host stall measured so far; the first call starts the clock.
pub fn stalled() -> Duration {
    STARTED.get_or_init(start);
    Duration::from_micros(STALLED_MICROS.load(Ordering::Relaxed))
}

fn start() {
    std::thread::Builder::new()
        .name("host-stall".into())
        .spawn(|| {
            loop {
                let before = Instant::now();
                std::thread::sleep(TICK);
                let overslept = before.elapsed().saturating_sub(TICK);
                STALLED_MICROS.fetch_add(overslept.as_micros() as u64, Ordering::Relaxed);
            }
        })
        .expect("spawn the host-stall thread");
}
