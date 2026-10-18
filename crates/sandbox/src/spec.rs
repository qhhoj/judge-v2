//! One isolate run: limits, stdio and the command (spec §6).

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

impl Limits {
    pub fn for_solution(time_limit_s: f64, memory_kib: u64, processes: u32, output_cap_bytes: u64) -> Limits {
        Limits {
            time_s: time_limit_s,
            wall_s: 3.0 * time_limit_s + 1.0,
            extra_time_s: 0.5,
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
        format!("--time={}", limits.time_s),
        format!("--extra-time={}", limits.extra_time_s),
        format!("--wall-time={}", limits.wall_s),
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
    args.extend(spec.dirs.iter().map(|d| format!("--dir={d}")));
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
}
