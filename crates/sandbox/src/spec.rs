//! One isolate run: limits, stdio and the command (docs/grading.md#box-lifecycle).

use std::path::Path;

#[derive(Debug, Clone, PartialEq)]
pub struct Limits {
    pub time_s: f64,
    pub wall_s: f64,
    pub extra_time_s: f64,
    pub memory_kib: u64,
    pub processes: u32,
    pub fsize_kib: u64,
    pub open_files: u32,
}

/// CPU seconds at which a contestant run is killed: the larger of the time limit plus 0.5 s and twice the limit.
/// isolate's `--extra-time` is the CPU time since the start (isolate.c `check_timeout`: killed once past both
/// `--time` and `--extra-time`), so a run that goes past the limit is still TLE (status TO), but its recorded
/// time is the real one up to this cap, and a run that ends on its own under the cap tells a slow program from
/// a hopeless one (`judge_grader::rerun`). Twice is above the slowdown measured under full load on production
/// (x1.46-1.63), with room for the cloud host's own noise.
pub fn hard_cap_s(time_limit_s: f64) -> f64 {
    (time_limit_s + 0.5).max(2.0 * time_limit_s)
}

/// `seconds` in whole milliseconds, the unit isolate counts, limits and reports time in: compare times as these.
pub fn whole_ms(seconds: f64) -> u64 {
    (seconds * 1000.0).round() as u64
}

/// The isolate argument for a time limit of `seconds`: its whole ms plus half a ms. isolate reads a limit as
/// `(int)(1000 * atof(arg))` (isolate.c), which truncates, so `2.006` would be 2005 ms; the half ms makes it land
/// on exactly `whole_ms(seconds)` whatever the float rounding.
fn isolate_seconds(seconds: f64) -> String {
    format!("{}", (whole_ms(seconds) as f64 + 0.5) / 1000.0)
}

impl Limits {
    pub fn for_solution(time_limit_s: f64, memory_kib: u64, processes: u32, output_cap_bytes: u64) -> Limits {
        Limits {
            time_s: time_limit_s,
            wall_s: 3.0 * time_limit_s + 1.0,
            extra_time_s: hard_cap_s(time_limit_s),
            memory_kib,
            processes,
            // one KiB of slack so output of exactly the cap is not cut off
            fsize_kib: output_cap_bytes.div_ceil(1024) + 1,
            open_files: 64,
        }
    }

    pub fn compile() -> Limits {
        Limits {
            time_s: 10.0,
            wall_s: 20.0,
            extra_time_s: 0.0,
            memory_kib: 1 << 20,
            processes: 32,
            fsize_kib: 64 << 10,
            open_files: 64,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Stdio {
    pub stdin: Option<String>,
    pub stdout: Option<String>,
    pub stderr: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RunSpec {
    pub argv: Vec<String>,
    pub limits: Limits,
    pub stdio: Stdio,
    pub env: Vec<(String, String)>,
    pub dirs: Vec<String>,
    /// Runtime files the judge reads into the page cache before the run (`preload`), so the box is not charged
    /// for them.
    pub preload: Vec<String>,
    /// `--inherit-fds`: the box keeps the fds the command was started with (only `command_with_channel` sets it;
    /// isolate otherwise closes every fd above 2).
    pub inherit_fds: bool,
}

/// Most cgroup room a run gets above its memory limit for the page cache of the files it writes.
pub const PAGE_CACHE_ROOM_MAX_KIB: u64 = 64 << 10;

/// The cgroup memory limit of a run: its memory limit plus room for the page cache of the files it writes
/// (one `--fsize` worth, at most `PAGE_CACHE_ROOM_MAX_KIB`). The kernel charges the page cache of written
/// files to the cgroup, and dirty pages wait for writeback, so without the room a program with a small
/// memory limit and a large output is OOM-killed while its own memory is far under the limit. The memory
/// limit itself is checked against the program's own memory (`classify`, `Meta::memory_kib`).
pub fn cgroup_memory_kib(limits: &Limits) -> u64 {
    limits.memory_kib + limits.fsize_kib.min(PAGE_CACHE_ROOM_MAX_KIB)
}

fn limit_args(limits: &Limits) -> Vec<String> {
    vec![
        format!("--cg-mem={}", cgroup_memory_kib(limits)),
        format!("--time={}", isolate_seconds(limits.time_s)),
        format!("--extra-time={}", isolate_seconds(limits.extra_time_s)),
        format!("--wall-time={}", isolate_seconds(limits.wall_s)),
        format!("--processes={}", limits.processes),
        format!("--fsize={}", limits.fsize_kib),
        format!("--open-files={}", limits.open_files),
    ]
}

fn stdio_args(stdio: &Stdio) -> Vec<String> {
    [("--stdin", &stdio.stdin), ("--stdout", &stdio.stdout), ("--stderr", &stdio.stderr)]
        .into_iter()
        .filter_map(|(flag, name)| name.as_ref().map(|n| format!("{flag}={n}")))
        .collect()
}

pub fn isolate_args(box_id: u32, spec: &RunSpec, meta: &Path) -> Vec<String> {
    let mut args =
        vec!["--cg".to_string(), format!("-b{box_id}"), "-s".into(), format!("--meta={}", meta.display())];
    args.extend(limit_args(&spec.limits));
    args.extend(stdio_args(&spec.stdio));
    args.push("--env=PATH=/usr/bin:/bin".into());
    args.push("--env=HOME=/box".into());
    args.extend(spec.env.iter().map(|(k, v)| format!("--env={k}={v}")));
    // No tmpfs on /dev/shm (isolate's default rule): /dev stays read-only. Pages the program keeps in a
    // file it does not map are still counted (`Meta::memory_kib`); this only takes away the easy place.
    args.push("--dir=dev/shm=".into());
    args.extend(spec.dirs.iter().map(|d| format!("--dir={d}")));
    if spec.inherit_fds {
        args.push("--inherit-fds".into());
    }
    args.extend(["--run".into(), "--".into()]);
    args.extend(spec.argv.iter().cloned());
    args
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn spec() -> RunSpec {
        RunSpec {
            argv: vec!["./main".into()],
            limits: Limits::for_solution(1.5, 262_144, 1, 5000),
            stdio: Stdio {
                stdin: Some("input".into()),
                stdout: Some("output".into()),
                stderr: Some("/dev/null".into()),
            },
            env: vec![],
            dirs: vec!["/opt/jdk".into()],
            preload: vec![],
            inherit_fds: false,
        }
    }

    fn has(args: &[String], flag: &str) -> bool {
        args.iter().any(|a| a == flag)
    }

    #[test]
    fn the_network_is_never_shared_stdio_can_be_inherited_and_the_command_is_last() {
        let mut s = spec();
        s.stdio = Stdio::default();
        let args = isolate_args(0, &s, Path::new("/m"));
        assert!(!has(&args, "--share-net"));
        assert!(!args.iter().any(|a| a.starts_with("--stdin") || a.starts_with("--stdout")));
        let run = args.iter().position(|a| a == "--run").unwrap();
        assert_eq!(&args[run + 1..], ["--", "./main"], "the command comes last, after `--`");
    }

    #[test]
    fn the_cgroup_gets_page_cache_room_of_one_fsize_up_to_a_cap() {
        let small = Limits::for_solution(1.0, 8192, 1, 4 << 20);
        let args = isolate_args(0, &RunSpec { limits: small.clone(), ..spec() }, Path::new("/m"));
        assert!(has(&args, &format!("--cg-mem={}", 8192 + small.fsize_kib)), "{args:?}");
        let huge = Limits::for_solution(1.0, 8192, 1, 1 << 30);
        assert_eq!(cgroup_memory_kib(&huge), 8192 + PAGE_CACHE_ROOM_MAX_KIB);
    }

    /// The milliseconds isolate makes of `flag`'s argument, as isolate.c does: `(int)(1000 * atof(arg))`.
    fn isolate_ms(args: &[String], flag: &str) -> u64 {
        let arg = args.iter().find_map(|a| a.strip_prefix(flag)).unwrap();
        (1000.0 * arg.parse::<f64>().unwrap()) as u64
    }

    #[test]
    fn isolate_reads_every_limit_as_its_whole_milliseconds_and_kills_past_the_hard_cap() {
        for ms in 1..=60_000 {
            let limits = Limits::for_solution(ms as f64 / 1000.0, 1, 1, 1);
            let args = isolate_args(0, &RunSpec { limits: limits.clone(), ..spec() }, Path::new("/m"));
            assert_eq!(isolate_ms(&args, "--time="), ms, "TL {ms} ms");
            assert_eq!(isolate_ms(&args, "--extra-time="), whole_ms(hard_cap_s(limits.time_s)), "TL {ms} ms");
            assert_eq!(isolate_ms(&args, "--wall-time="), whole_ms(limits.wall_s), "TL {ms} ms");
        }
        assert_eq!(
            (whole_ms(hard_cap_s(0.3)), whole_ms(hard_cap_s(2.0))),
            (800, 4000),
            "TL + 0.5 s, then 2 x TL"
        );
    }

    #[test]
    fn inherited_fds_are_asked_for_only_by_a_channel_run() {
        let plain = isolate_args(0, &spec(), Path::new("/m"));
        assert!(!has(&plain, "--inherit-fds"));
        let channel = isolate_args(0, &RunSpec { inherit_fds: true, ..spec() }, Path::new("/m"));
        let run = channel.iter().position(|a| a == "--run").unwrap();
        assert_eq!(channel[run - 1], "--inherit-fds", "an isolate option, before --run");
    }
}
