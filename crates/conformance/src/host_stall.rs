//! How long this process has not been scheduled: the host's share of a step's wait.
//!
//! A step waits 5 s for the other side. On a starved host the whole process can stand still for
//! seconds, and that time is not the other side being slow. A plain thread (not a tokio task, so
//! a runtime blocked by the code under test is still counted against it) sleeps one tick at a
//! time; an oversleep of a tick or more is time the host did not run this process.
use std::{
    sync::{
        OnceLock,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

/// The stall clock's tick. Short, so a stall is measured to within a few ms; long enough that the
/// thread costs nothing next to a scenario. An oversleep under one tick is not counted (`stall_of`).
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
                let stall = stall_of(before.elapsed().saturating_sub(TICK));
                STALLED_MICROS.fetch_add(stall.as_micros() as u64, Ordering::Relaxed);
            }
        })
        .expect("spawn the host-stall thread");
}

/// The stall one tick's oversleep stands for: none below a tick (timer slack, wakeup latency, which a
/// healthy host adds on every tick and which would stretch a real hang's wait to the cap), else all of it.
fn stall_of(overslept: Duration) -> Duration {
    if overslept >= TICK { overslept } else { Duration::ZERO }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jitter_below_a_tick_is_no_stall_and_a_tick_or_more_counts_in_full() {
        let jitter = [Duration::from_micros(50), Duration::from_millis(2), TICK - Duration::from_micros(1)];
        assert!(jitter.iter().all(|&j| stall_of(j).is_zero()), "timer slack and wakeup latency");
        assert_eq!(stall_of(TICK), TICK);
        assert_eq!(stall_of(Duration::from_millis(250)), Duration::from_millis(250), "a throttle period");
    }
}
