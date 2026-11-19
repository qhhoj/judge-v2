//! The memory of a `file_io` run (docs/grading.md, memory).
//!
//! A `file_io` program writes its answer to a box file, and the page cache of that file is charged to the run's
//! cgroup: an honest 64 MiB answer would add 64 MiB to `memory.peak`. The judge subtracts it, but never more
//! than the answer's own size and never more than the regular-file page cache the box cgroup ever held, and the
//! figure is never below either floor:
//!
//! memory = max(P - min(F, out), N, min(P, rss))
//!
//! - `P`: the cgroup's `memory.peak`, exact.
//! - `F`: the largest regular-file page cache the samples saw (`MemorySample::file_cache_kib`).
//! - `out`: the answer file's size at the end (its allocated bytes when smaller).
//! - `N`: the largest memory other than regular-file page cache the samples saw (`MemorySample::own_kib`), at
//!   most `P`. memfd and tmpfs pages count here.
//! - `rss`: the peak resident set, at most `P`.
//!
//! The input's pages are the judge's for the run (`PinnedInput`), so the box is not charged for reading them.

use judge_sandbox::IsolateBox;
use judge_sandbox::memory_sampler::MemorySample;
use judge_sandbox::meta::Meta;
use std::os::unix::fs::MetadataExt;

/// `max(P - min(F, out), N, min(P, rss))`, all in KiB and saturating.
pub fn file_io_memory_kib(peak: u64, rss: u64, sample: MemorySample, out_kib: u64) -> u64 {
    let less_output = peak.saturating_sub(sample.file_cache_kib.min(out_kib));
    less_output.max(sample.own_kib.min(peak)).max(rss.min(peak))
}

/// The answer file's size in KiB: its length, or its allocated bytes when fewer (a sparse file); 0 when it is
/// absent or not a regular file.
pub fn output_kib(sandbox: &IsolateBox, found: &str) -> u64 {
    match std::fs::symlink_metadata(sandbox.dir().join(found)) {
        Ok(meta) if meta.is_file() => meta.len().min(meta.blocks() * 512) >> 10,
        _ => 0,
    }
}

/// Puts the `file_io` figure in `meta.cg_mem_kib`. An OOM kill keeps the peak that hit the cgroup limit, and a
/// timeout (whose memory is the smaller of peak and rss, `Meta::memory_kib`) and a missing peak stay as they are.
pub fn apply(meta: &mut Meta, sample: MemorySample, out_kib: u64) {
    if meta.oom_killed || meta.cg_mem_kib == 0 || meta.status.as_deref() == Some("TO") {
        return;
    }
    meta.cg_mem_kib = file_io_memory_kib(meta.cg_mem_kib, meta.max_rss_kib, sample, out_kib);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(own_kib: u64, file_cache_kib: u64) -> MemorySample {
        MemorySample { own_kib, file_cache_kib }
    }

    #[test]
    fn the_peak_drops_by_at_most_the_answer_and_the_cache_seen_and_never_below_either_floor() {
        // 12 MiB heap + a 64 MiB answer: P 80 MiB, F 64 MiB, out 64 MiB.
        assert_eq!(file_io_memory_kib(81_920, 13_000, sample(12_800, 65_536), 65_536), 16_384);
        // 20 MiB in another box file and a 3-byte answer: only the answer's size comes off.
        assert_eq!(file_io_memory_kib(40_000, 3_000, sample(4_000, 30_000), 0), 40_000);
        assert_eq!(file_io_memory_kib(40_000, 3_000, sample(4_000, 30_000), 8_000), 32_000);
        // A cache smaller than the answer (some of it already written back and dropped).
        assert_eq!(file_io_memory_kib(40_000, 3_000, sample(4_000, 6_000), 8_000), 34_000);
        // The rss floor and the sampled floor (a memfd held while the answer's cache was there).
        assert_eq!(file_io_memory_kib(40_000, 35_000, sample(4_000, 30_000), 30_000), 35_000);
        assert_eq!(file_io_memory_kib(40_000, 3_000, sample(38_000, 30_000), 30_000), 38_000);
        // Saturating, and no floor above the peak.
        assert_eq!(file_io_memory_kib(5_000, 3_000, sample(0, 9_000), 9_000), 3_000);
        assert_eq!(file_io_memory_kib(5_000, 7_000, sample(6_000, 0), 0), 5_000);
    }

    #[test]
    fn an_oom_kill_a_timeout_and_a_missing_peak_keep_their_figure() {
        let figure = |text: &str| {
            let mut meta = judge_sandbox::meta::parse_meta(text).unwrap();
            apply(&mut meta, sample(1_000, 60_000), 60_000);
            meta.cg_mem_kib
        };
        assert_eq!(figure("max-rss:3000\ncg-mem:70000\nexitcode:0\n"), 10_000);
        assert_eq!(figure("max-rss:3000\ncg-mem:70000\ncg-oom-killed:1\n"), 70_000);
        assert_eq!(figure("max-rss:3000\ncg-mem:70000\nstatus:TO\n"), 70_000);
        assert_eq!(figure("max-rss:3000\n"), 0);
    }
}
