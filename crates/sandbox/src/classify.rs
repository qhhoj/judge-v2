//! isolate meta → run verdict (spec §2 "Verdict from isolate meta", Review Focus 2).

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

#[derive(Debug, Clone, Copy)]
pub struct OutputUse {
    pub written: u64,
    pub cap: u64,
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

/// The verdict of a run made with `limits`. MLE is a peak RSS over `limits.memory_kib`, or an OOM kill by
/// the cgroup, whose limit has room above the memory limit for page cache (`spec::cgroup_memory_kib`).
pub fn classify(meta: &Meta, output: OutputUse, limits: &Limits) -> RunVerdict {
    let status = meta.status.as_deref();
    if status == Some("XX") {
        return RunVerdict::Internal(meta.message.clone());
    }
    if meta.oom_killed || meta.max_rss_kib > limits.memory_kib {
        return RunVerdict::Mle;
    }
    if status == Some("TO") {
        return RunVerdict::Tle;
    }
    if output.written > output.cap || meta.exit_signal == Some(SIGXFSZ) {
        return RunVerdict::Ole;
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

    fn verdict(meta: &str, written: u64) -> RunVerdict {
        let limits = Limits::for_solution(1.0, 16_384, 1, 4096);
        classify(&parse_meta(meta).unwrap(), OutputUse { written, cap: 4096 }, &limits)
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
    fn mle_is_a_peak_rss_over_the_limit_or_an_oom_kill_not_the_cgroup_peak() {
        assert_eq!(verdict("exitcode:0\nmax-rss:16385\ncg-mem:17000\n", 0), RunVerdict::Mle, "over by 1 KiB");
        assert_eq!(verdict("exitcode:0\nmax-rss:16384\n", 0), RunVerdict::Ok, "exactly the limit");
        let page_cache = "exitcode:0\nmax-rss:1800\ncg-mem:40000\n";
        assert_eq!(verdict(page_cache, 0), RunVerdict::Ok, "a cgroup peak of written-file page cache");
        assert_eq!(verdict("status:SG\nexitsig:9\nmax-rss:900\ncg-oom-killed:1\n", 0), RunVerdict::Mle);
    }

    #[test]
    fn memory_and_time_win_over_output() {
        assert_eq!(verdict("status:TO\n", 5000), RunVerdict::Tle);
        assert_eq!(verdict("status:SG\nexitsig:9\ncg-oom-killed:1\n", 5000), RunVerdict::Mle);
    }
}
