//! isolate meta → run verdict (docs/grading.md#verdict-rules).

use crate::meta::{MemoryCount, Meta};
use crate::spec::{Limits, whole_ms};

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

/// What a run did with its output: `written` bytes against its `cap`, whether a drained stdout or stderr
/// reached the file size limit (`cut`, `drain`), and which counter its memory is (`MemoryCount`).
#[derive(Debug, Clone, Copy)]
pub struct OutputUse {
    pub written: u64,
    pub cap: u64,
    pub cut: bool,
    pub memory: MemoryCount,
}

impl OutputUse {
    /// A run whose output is not capped and whose memory is the smaller counter (compilers, jury programs).
    pub const UNMEASURED: OutputUse =
        OutputUse { written: 0, cap: u64::MAX, cut: false, memory: MemoryCount::Smaller };
}

const SIGPIPE: i32 = 13;
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
/// MLE is the run's memory (`Meta::memory_kib`, counted as `output.memory` says) over `limits.memory_kib`,
/// or an OOM kill by the cgroup, whose limit has room above the memory limit for page cache
/// (`spec::cgroup_memory_kib`).
///
/// Output over the cap is OLE even when the run then timed out: the JVM ignores SIGPIPE and SIGXFSZ and
/// `PrintStream` swallows the write error, so a Java or Kotlin print loop runs on to the time limit once its
/// output is cut. A run killed past the file size limit is OLE too: by SIGXFSZ for a box file, by SIGPIPE
/// for a drained stdout or stderr the judge closed there (`drain`).
///
/// A run isolate stopped on time (status TO) with its CPU over the limit is TLE (or OLE), not MLE, even when its
/// memory was over the limit or the OOM flag is set: it had already failed on time. A run the cgroup OOM-killed
/// first (status SG) is MLE, even when its CPU had passed the limit on the way to the hard cap
/// (`spec::hard_cap_s`). On a TO isolate's max-rss is its own proxy's (~820 KiB), so `Meta::memory_kib` stays
/// under the limit.
pub fn classify(meta: &Meta, output: OutputUse, limits: &Limits) -> RunVerdict {
    let status = meta.status.as_deref();
    if status == Some("XX") {
        return RunVerdict::Internal(meta.message.clone());
    }
    let past_time_limit = status == Some("TO") && whole_ms(meta.time_s) > whole_ms(limits.time_s);
    let over_memory = meta.oom_killed || meta.memory_kib(output.memory) > limits.memory_kib;
    if over_memory && !past_time_limit {
        return RunVerdict::Mle;
    }
    if over_output(meta, output) {
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

/// Output over the cap, or a run killed by writing past the file size limit.
fn over_output(meta: &Meta, output: OutputUse) -> bool {
    let signal = meta.exit_signal;
    output.written > output.cap || signal == Some(SIGXFSZ) || (output.cut && signal == Some(SIGPIPE))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::meta::parse_meta;

    /// The verdict under a 16 MiB limit and a 4096-byte output cap of a contestant's run that wrote `written`
    /// bytes, its drained output `cut` or not.
    fn verdict_cut(meta: &str, written: u64, cut: bool) -> RunVerdict {
        let limits = Limits::for_solution(1.0, 16_384, 1, 4096);
        let output = OutputUse { written, cap: 4096, cut, memory: MemoryCount::CgroupPeak };
        classify(&parse_meta(meta).unwrap(), output, &limits)
    }

    fn verdict(meta: &str, written: u64) -> RunVerdict {
        verdict_cut(meta, written, false)
    }

    #[test]
    fn pages_a_contestant_holds_unmapped_count_against_the_limit() {
        // 10 MiB of heap and 7 MiB in a memfd read back with pread: under the cgroup limit, no OOM kill
        assert_eq!(verdict("exitcode:0\nmax-rss:10240\ncg-mem:17408\n", 0), RunVerdict::Mle);
        let limits = Limits::for_solution(1.0, 16_384, 1, 4096);
        let meta = parse_meta("exitcode:0\nmax-rss:10240\ncg-mem:17408\n").unwrap();
        assert_eq!(
            classify(&meta, OutputUse::UNMEASURED, &limits),
            RunVerdict::Ok,
            "a jury program: the smaller counter"
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
    fn a_run_killed_writing_to_an_output_closed_at_the_file_size_limit_is_ole() {
        // A kept stderr past its allowance: the judge closed the pipe, the next write got SIGPIPE
        assert_eq!(verdict_cut("status:SG\nexitsig:13\n", 10, true), RunVerdict::Ole);
        let own_pipe = verdict_cut("status:SG\nexitsig:13\n", 10, false);
        assert_eq!(own_pipe, RunVerdict::Rte { reason: "SIGPIPE".into() }, "a pipe of the program's own");
        assert_eq!(verdict_cut("exitcode:0\n", 10, true), RunVerdict::Ok, "no write after the cut");
    }

    #[test]
    fn mle_is_the_programs_own_memory_over_the_limit_or_an_oom_kill() {
        assert_eq!(verdict("exitcode:0\nmax-rss:16385\ncg-mem:17000\n", 0), RunVerdict::Mle, "over by 1 KiB");
        assert_eq!(
            verdict("exitcode:0\nmax-rss:16000\ncg-mem:16384\n", 0),
            RunVerdict::Ok,
            "exactly the limit"
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

    #[test]
    fn a_run_past_the_time_limit_is_tle_even_when_the_cgroup_then_oom_killed_it() {
        let past = "time:1.400\nstatus:TO\nexitsig:9\nmax-rss:900\ncg-mem:17000\ncg-oom-killed:1\n";
        assert_eq!(verdict(past, 0), RunVerdict::Tle, "failed on time before the memory ran out");
        let wall_only = "time:0.300\nstatus:TO\nexitsig:9\nmax-rss:900\ncg-mem:17000\ncg-oom-killed:1\n";
        assert_eq!(verdict(wall_only, 0), RunVerdict::Mle, "under the CPU limit the OOM kill decides");
        let limits = Limits::for_solution(2.01, 16_384, 1, 4096);
        let one_ms_past =
            parse_meta("time:2.011\nstatus:TO\nexitsig:9\ncg-mem:17000\ncg-oom-killed:1\n").unwrap();
        let output = OutputUse { written: 0, cap: 4096, cut: false, memory: MemoryCount::CgroupPeak };
        assert_eq!(classify(&one_ms_past, output, &limits), RunVerdict::Tle, "TL 2.01: 2.011 s is past it");
    }
}
