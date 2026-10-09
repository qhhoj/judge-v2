#![cfg(feature = "isolate")]
use judge_sandbox::classify::{OutputUse, RunVerdict, classify};
use judge_sandbox::spec::{Limits, RunSpec, Stdio};
use judge_sandbox::{IsolateBox, SandboxConfig};

fn config(meta: &std::path::Path) -> SandboxConfig {
    SandboxConfig { meta_dir: meta.into(), ..SandboxConfig::default() }
}

fn spec(argv: &[&str], memory_kib: u64) -> RunSpec {
    RunSpec {
        argv: argv.iter().map(|s| s.to_string()).collect(),
        limits: Limits::for_solution(1.0, memory_kib, 1, 1 << 20),
        stdio: Stdio { stdin: None, stdout: Some("output".into()), stderr: Some("/dev/null".into()) },
        env: vec![],
        dirs: vec![],
    }
}

#[test]
fn runs_a_program_and_reads_its_output() {
    let meta = tempfile::tempdir().unwrap();
    let mut b = IsolateBox::open(&config(meta.path()), 40, None).unwrap();
    let m = b.run(&spec(&["/bin/sh", "-c", "echo hi"], 65_536)).unwrap();
    assert_eq!(m.exit_code, Some(0));
    assert_eq!(std::fs::read_to_string(b.dir().join("output")).unwrap(), "hi\n");
}

#[test]
fn reset_clears_sticky_oom_and_peak_memory() {
    let meta = tempfile::tempdir().unwrap();
    let mut b = IsolateBox::open(&config(meta.path()), 41, None).unwrap();
    let hog = b.run(&spec(&["/usr/bin/python3", "-c", "a = bytearray(50_000_000)"], 8192)).unwrap();
    assert_eq!(classify(&hog, OutputUse { written: 0, cap: 1 << 20 }), RunVerdict::Mle);
    b.reset().unwrap();
    let clean = b.run(&spec(&["/bin/true"], 65_536)).unwrap();
    assert!(!clean.oom_killed, "a reset box must not report the previous OOM");
    assert!(clean.cg_mem_kib < 4096, "peak memory carried over: {}", clean.cg_mem_kib);
}

#[test]
fn reset_empties_the_box_and_put_files_appear() {
    let meta = tempfile::tempdir().unwrap();
    let mut b = IsolateBox::open(&config(meta.path()), 42, None).unwrap();
    b.put_bytes("input", b"5\n").unwrap();
    b.run(&spec(&["/bin/sh", "-c", "exec cat input"], 65_536)).unwrap();
    assert_eq!(std::fs::read_to_string(b.dir().join("output")).unwrap(), "5\n");
    b.reset().unwrap();
    assert!(!b.dir().join("input").exists());
}

#[test]
fn drop_cleans_the_box() {
    let meta = tempfile::tempdir().unwrap();
    let dir = {
        let b = IsolateBox::open(&config(meta.path()), 43, None).unwrap();
        b.dir().to_path_buf()
    };
    assert!(!dir.exists());
}

#[test]
fn pinned_runs_go_through_taskset() {
    let meta = tempfile::tempdir().unwrap();
    let b = IsolateBox::open(&config(meta.path()), 44, Some(0)).unwrap();
    let command = b.command(&spec(&["/bin/true"], 65_536));
    assert_eq!(command.get_program(), "/usr/bin/taskset");
    let args: Vec<_> = command.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
    assert_eq!(&args[..3], ["-c", "0", "/usr/local/bin/isolate"]);
}
