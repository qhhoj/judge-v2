//! Reads a box cgroup's `memory.stat` every `SAMPLE_PERIOD` while a run is live, and once after it ended
//! (`IsolateBox::run_drained`). `memory.peak` is one number for every page charged to the run; the samples
//! split it into the program's own memory and the page cache of regular files, at each sample's moment.

use crate::error::SandboxError;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

/// One `memory.stat` read costs about 80 µs, so a sampler uses under a tenth of a judge core per run.
pub const SAMPLE_PERIOD: Duration = Duration::from_millis(1);

/// The largest values the reads saw, in KiB.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MemorySample {
    /// `anon + shmem + kernel + sock`: every page but regular-file page cache (memfd and tmpfs pages are shmem).
    pub own_kib: u64,
    /// Regular-file page cache, `file - shmem` (saturating: the counters are flushed lazily).
    pub file_cache_kib: u64,
}

/// The two figures of one `memory.stat` text, in KiB.
fn parse_stat(text: &str) -> MemorySample {
    let value = |key: &str| {
        let parsed =
            text.lines().find_map(|line| line.strip_prefix(key)?.strip_prefix(' ')?.parse::<u64>().ok());
        parsed.unwrap_or(0)
    };
    let shmem = value("shmem");
    let own = value("anon") + shmem + value("kernel") + value("sock");
    MemorySample { own_kib: own >> 10, file_cache_kib: value("file").saturating_sub(shmem) >> 10 }
}

pub struct MemorySampler {
    stop: Arc<AtomicBool>,
    thread: JoinHandle<(MemorySample, u64)>,
}

impl MemorySampler {
    /// Starts reading `stat` (the box cgroup's `memory.stat`, which isolate makes at `--init`).
    pub fn start(stat: &Path) -> Result<MemorySampler, SandboxError> {
        let file = File::open(stat).map_err(|e| SandboxError::Io(format!("{}: {e}", stat.display())))?;
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let thread = std::thread::Builder::new()
            .name("qhhoj-memstat".into())
            .spawn(move || sample_until(file, &stopped))?;
        Ok(MemorySampler { stop, thread })
    }

    /// Call once the run has ended: one more read, then the largest values seen. No successful read is an
    /// error, so a run is never judged on figures nobody measured.
    pub fn finish(self) -> Result<MemorySample, SandboxError> {
        self.stop.store(true, Ordering::Release);
        let (seen, reads) =
            self.thread.join().map_err(|_| SandboxError::Io("memory sampler panicked".into()))?;
        if reads == 0 {
            return Err(SandboxError::Io("the box cgroup's memory.stat could not be read".into()));
        }
        Ok(seen)
    }
}

/// Reads `file` every `SAMPLE_PERIOD` until `stop`, then once more; returns the largest values and the reads.
fn sample_until(mut file: File, stop: &AtomicBool) -> (MemorySample, u64) {
    let (mut seen, mut reads, mut text) = (MemorySample::default(), 0u64, String::new());
    loop {
        let last = stop.load(Ordering::Acquire);
        text.clear();
        if file.seek(SeekFrom::Start(0)).is_ok() && file.read_to_string(&mut text).is_ok() {
            let now = parse_stat(&text);
            seen.own_kib = seen.own_kib.max(now.own_kib);
            seen.file_cache_kib = seen.file_cache_kib.max(now.file_cache_kib);
            reads += 1;
        }
        if last {
            return (seen, reads);
        }
        std::thread::sleep(SAMPLE_PERIOD);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn own_memory_counts_shmem_and_file_cache_excludes_it_saturating() {
        let stat = "anon 4194304\nfile 9437184\nkernel 1048576\nkernel_stack 65536\nshmem 2097152\n\
                    file_mapped 4096\nsock 0\nanon_thp 0\n";
        assert_eq!(parse_stat(stat), MemorySample { own_kib: 7168, file_cache_kib: 7168 });
        let stale = "anon 0\nfile 1048576\nshmem 2097152\n";
        assert_eq!(parse_stat(stale), MemorySample { own_kib: 2048, file_cache_kib: 0 });
    }
}
