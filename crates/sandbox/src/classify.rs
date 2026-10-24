//! isolate meta → run verdict (docs/grading.md#verdict-rules).

use crate::meta::Meta;
use crate::spec::Limits;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunVerdict {
    Ok,
    Tle,
    Mle,
    Ole,
    Rte { reason: String },
    Ir { exit_code: i32 },
    Internal(String),
}

/// What a run did with its files: `written` bytes of output against its `cap`, and `own_files_kib`, the
/// page cache its own input and output files may hold (`Meta::memory_kib`; None when not measured).
#[derive(Debug, Clone, Copy)]
pub struct OutputUse {
    pub written: u64,
    pub cap: u64,
    pub own_files_kib: Option<u64>,
}

impl OutputUse {
    /// A run whose output is not capped and whose files are not measured (compilers, jury programs).
    pub const UNMEASURED: OutputUse = OutputUse { written: 0, cap: u64::MAX, own_files_kib: None };
}

const SIGXFSZ: i32 = 25;

pub fn signal_name(signal: i32) -> String {
    let name = match signal {
        4 => "SIGILL",
        6 => "SIGABRT",
        7 => "SIGBUS",
        8 => "SIGFPE",
        9 => "SIGKILL",
        11 => "SIGSEGV",
        13 => "SIGPIPE",
        15 => "SIGTERM",
        24 => "SIGXCPU",
        25 => "SIGXFSZ",
        31 => "SIGSYS",
        _ => return format!("signal {signal}"),
    };
    name.to_string()
}

/// The verdict of a run made with `limits`, in order MLE, OLE, TLE, then how the program ended.
///
/// MLE is the program's own memory (`Meta::memory_kib`) over `limits.memory_kib`, or an OOM kill by the
/// cgroup, whose limit has room above the memory limit for page cache (`spec::cgroup_memory_kib`). The
/// cgroup peak less the page cache of the run's own files (`output.own_files_kib`) is the program's, so
/// tmpfs or memfd pages it wrote and read back unmapped count, the page cache of its input and output not.
///
/// Output over the cap is OLE even when the run then timed out: the JVM ignores SIGXFSZ and `PrintStream`
/// swallows the write error, so a Java or Kotlin print loop runs on to the time limit once its output is cut.
///
/// A run killed at the time limit is TLE (or OLE), not MLE, unless the cgroup OOM-killed it: on a TO isolate's
/// max-rss is its own proxy's (~820 KiB), so `Meta::memory_kib` stays under the limit.
pub fn classify(meta: &Meta, output: OutputUse, limits: &Limits) -> RunVerdict {
    let status = meta.status.as_deref();
    if status == Some("XX") {
        return RunVerdict::Internal(meta.message.clone());
    }
    if meta.oom_killed || meta.memory_kib(output.own_files_kib) > limits.memory_kib {
        return RunVerdict::Mle;
    }
    if output.written > output.cap || meta.exit_signal == Some(SIGXFSZ) {
        return RunVerdict::Ole;
    }
    if status == Some("TO") {
        return RunVerdict::Tle;
    }
    match status {
        Some("SG") => RunVerdict::Rte { reason: signal_name(meta.exit_signal.unwrap_or(0)) },
        Some("RE") => RunVerdict::Ir { exit_code: meta.exit_code.unwrap_or(-1) },
        _ => RunVerdict::Ok,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::meta::parse_meta;

    /// The verdict under a `memory_kib` limit of a run whose own files hold `files_kib` of page cache.
    fn verdict_with_files(meta: &str, files_kib: u64, memory_kib: u64) -> RunVerdict {
        let limits = Limits::for_solution(1.0, memory_kib, 1, 24 << 20);
        let output = OutputUse { written: 0, cap: 24 << 20, own_files_kib: Some(files_kib) };
        classify(&parse_meta(meta).unwrap(), output, &limits)
    }

    /// The verdict under a 16 MiB limit and a 4096-byte output cap of a run that wrote `written` bytes.
    fn verdict(meta: &str, written: u64) -> RunVerdict {
        let limits = Limits::for_solution(1.0, 16_384, 1, 4096);
        let output = OutputUse { written, cap: 4096, own_files_kib: Some(written.div_ceil(1024)) };
        classify(&parse_meta(meta).unwrap(), output, &limits)
    }

    #[test]
    fn tmpfs_pages_count_against_the_limit_and_the_page_cache_of_the_runs_own_files_does_not() {
        // 60 MiB of heap and 22 MiB in /dev/shm read back with pread: under the cgroup limit, no OOM kill
        let shm = "exitcode:0\nmax-rss:62840\ncg-mem:85472\n";
        assert_eq!(verdict_with_files(shm, 0, 65_536), RunVerdict::Mle);
        assert_eq!(
            verdict_with_files(shm, 22 << 10, 65_536),
            RunVerdict::Ok,
            "the same peak, 22 MiB of output"
        );
        let evicted_input = "exitcode:0\nmax-rss:7460\ncg-mem:42720\n";
        assert_eq!(
            verdict_with_files(evicted_input, 40 << 10, 32_768),
            RunVerdict::Ok,
            "40 MiB of input read"
        );
    }

    #[test]
    fn maps_isolate_outcomes() {
        assert_eq!(verdict("exitcode:0\n", 10), RunVerdict::Ok);
        assert_eq!(verdict("status:TO\nkilled:1\n", 0), RunVerdict::Tle);
        assert_eq!(verdict("status:SG\nexitsig:9\ncg-oom-killed:1\n", 0), RunVerdict::Mle);
        assert_eq!(verdict("status:SG\nexitsig:11\n", 0), RunVerdict::Rte { reason: "SIGSEGV".into() });
        assert_eq!(verdict("status:RE\nexitcode:3\n", 0), RunVerdict::Ir { exit_code: 3 });
        assert!(matches!(verdict("status:XX\nmessage:boom\n", 0), RunVerdict::Internal(m) if m == "boom"));
    }

    #[test]
    fn output_at_the_cap_is_ole_even_without_sigxfsz() {
        // Python ignores SIGXFSZ and exits 120 after EFBIG (seen on isolate 2.6).
        assert_eq!(verdict("status:RE\nexitcode:120\n", 4097), RunVerdict::Ole);
        assert_eq!(verdict("exitcode:0\n", 4096), RunVerdict::Ok, "exactly the cap is allowed");
        assert_eq!(verdict("status:SG\nexitsig:25\n", 100), RunVerdict::Ole);
    }

    #[test]
    fn mle_is_the_programs_own_memory_over_the_limit_or_an_oom_kill() {
        assert_eq!(verdict("exitcode:0\nmax-rss:16385\ncg-mem:17000\n", 0), RunVerdict::Mle, "over by 1 KiB");
        let at_the_limit = "exitcode:0\nmax-rss:16384\ncg-mem:17000\n";
        assert_eq!(verdict_with_files(at_the_limit, 616, 16_384), RunVerdict::Ok, "exactly the limit");
        let page_cache = "exitcode:0\nmax-rss:1800\ncg-mem:40000\n";
        assert_eq!(
            verdict_with_files(page_cache, 38 << 10, 16_384),
            RunVerdict::Ok,
            "38 MiB of output written"
        );
        // As PyPy's a+b (max-rss 56 MiB, cg-mem 19 MiB): ~37 MiB of the RSS is libpypy, cached, not charged
        let shared_objects = "exitcode:0\nmax-rss:40000\ncg-mem:9000\n";
        assert_eq!(verdict(shared_objects, 0), RunVerdict::Ok, "a peak RSS of mapped, already cached files");
        assert_eq!(verdict("status:SG\nexitsig:9\nmax-rss:900\ncg-oom-killed:1\n", 0), RunVerdict::Mle);
    }

    #[test]
    fn the_memory_limit_holds_when_isolate_reports_only_one_counter() {
        // isolate prints cg-mem only from a readable, non-zero memory.peak (kernel 5.19+)
        assert_eq!(verdict("exitcode:0\nmax-rss:16385\n", 0), RunVerdict::Mle, "no cg-mem: the peak RSS");
        assert_eq!(verdict("exitcode:0\nmax-rss:16384\n", 0), RunVerdict::Ok, "no cg-mem, exactly the limit");
        assert_eq!(verdict("exitcode:0\ncg-mem:16385\n", 0), RunVerdict::Mle, "no max-rss: the cgroup peak");
    }

    #[test]
    fn memory_wins_over_output_and_output_over_time() {
        // A JVM print loop (isolate 2.6): output cut at --fsize, no SIGXFSZ, killed at the time limit. The
        // max-rss of a TO is isolate's proxy's, so a cgroup peak 8x the memory limit is still not MLE.
        let jvm_flood = "time:1.078\ntime-wall:0.908\nmax-rss:824\ncg-mem:131072\nkilled:1\nstatus:TO\nmessage:Time limit exceeded\n";
        assert_eq!(verdict(jvm_flood, 4097), RunVerdict::Ole);
        assert_eq!(verdict(jvm_flood, 4096), RunVerdict::Tle, "exactly the cap is not over it");
        assert_eq!(verdict("status:SG\nexitsig:9\ncg-oom-killed:1\n", 5000), RunVerdict::Mle);
    }
}
