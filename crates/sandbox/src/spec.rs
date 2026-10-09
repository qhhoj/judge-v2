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
            fsize_kib: output_cap_bytes.div_ceil(1024),
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

fn limit_args(limits: &Limits) -> Vec<String> {
    vec![
        format!("--cg-mem={}", limits.memory_kib),
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
    fn solution_limits_follow_the_spec() {
        let limits = Limits::for_solution(1.5, 262_144, 1, 5000);
        assert_eq!(limits.wall_s, 5.5);
        assert_eq!(limits.fsize_kib, 5);
        assert_eq!(limits.extra_time_s, 0.5);
    }

    #[test]
    fn argv_has_cgroup_limits_files_and_the_command_last() {
        let args = isolate_args(4, &spec(), Path::new("/tmp/4.meta"));
        for flag in [
            "--cg",
            "-b4",
            "--cg-mem=262144",
            "--time=1.5",
            "--extra-time=0.5",
            "--wall-time=5.5",
            "--processes=1",
            "--fsize=5",
            "--open-files=64",
            "--meta=/tmp/4.meta",
            "--stdin=input",
            "--stdout=output",
            "--stderr=/dev/null",
            "--env=PATH=/usr/bin:/bin",
            "--env=HOME=/box",
            "--dir=/opt/jdk",
        ] {
            assert!(has(&args, flag), "missing {flag} in {args:?}");
        }
        let run = args.iter().position(|a| a == "--run").unwrap();
        assert_eq!(&args[run + 1..], ["--", "./main"]);
    }

    #[test]
    fn the_network_is_never_shared_and_stdio_can_be_inherited() {
        let mut s = spec();
        s.stdio = Stdio::default();
        let args = isolate_args(0, &s, Path::new("/m"));
        assert!(!has(&args, "--share-net"));
        assert!(!args.iter().any(|a| a.starts_with("--stdin") || a.starts_with("--stdout")));
    }

    #[test]
    fn extra_env_is_passed() {
        let mut s = spec();
        s.env.push(("JAVA_TOOL_OPTIONS".into(), "-Xss64m".into()));
        assert!(has(&isolate_args(0, &s, Path::new("/m")), "--env=JAVA_TOOL_OPTIONS=-Xss64m"));
    }
}
