//! isolate meta → run verdict (spec §2 "Verdict from isolate meta", Review Focus 2).

use crate::meta::Meta;

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

pub fn classify(meta: &Meta, output: OutputUse) -> RunVerdict {
    let status = meta.status.as_deref();
    if status == Some("XX") {
        return RunVerdict::Internal(meta.message.clone());
    }
    if meta.oom_killed {
        return RunVerdict::Mle;
    }
    if status == Some("TO") {
        return RunVerdict::Tle;
    }
    if output.written >= output.cap || meta.exit_signal == Some(SIGXFSZ) {
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
        classify(&parse_meta(meta).unwrap(), OutputUse { written, cap: 4096 })
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
        assert_eq!(verdict("status:RE\nexitcode:120\n", 4096), RunVerdict::Ole);
        assert_eq!(verdict("status:SG\nexitsig:25\n", 100), RunVerdict::Ole);
    }

    #[test]
    fn memory_and_time_win_over_output() {
        assert_eq!(verdict("status:TO\n", 4096), RunVerdict::Tle);
        assert_eq!(verdict("status:SG\nexitsig:9\ncg-oom-killed:1\n", 4096), RunVerdict::Mle);
    }

    #[test]
    fn signal_names() {
        assert_eq!(signal_name(6), "SIGABRT");
        assert_eq!(signal_name(8), "SIGFPE");
        assert_eq!(signal_name(77), "signal 77");
    }
}
