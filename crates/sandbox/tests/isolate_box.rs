#![cfg(feature = "isolate")]
use judge_sandbox::classify::{OutputUse, RunVerdict, classify};
use judge_sandbox::spec::{Limits, RunSpec, Stdio};
use judge_sandbox::{DiskQuota, IsolateBox, Room, SandboxConfig, SandboxError, disk_quota_works};

fn box_id(offset: u32) -> u32 {
    let base: u32 = std::env::var("JUDGE_TEST_BOX_BASE").ok().and_then(|v| v.parse().ok()).unwrap_or(500);
    base + offset
}

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
        inherit_fds: false,
    }
}

#[test]
fn reset_clears_sticky_oom_and_peak_memory() {
    let meta = tempfile::tempdir().unwrap();
    let mut b = IsolateBox::open(&config(meta.path()), box_id(1), None).unwrap();
    let hog = b.run(&spec(&["/usr/bin/python3", "-c", "a = bytearray(50_000_000)"], 8192)).unwrap();
    assert_eq!(classify(&hog, OutputUse::UNMEASURED, &spec(&[], 8192).limits), RunVerdict::Mle);
    b.reset().unwrap();
    let clean = b.run(&spec(&["/bin/true"], 65_536)).unwrap();
    assert!(!clean.oom_killed, "a reset box must not report the previous OOM");
    assert!(clean.cg_mem_kib < 4096, "peak memory carried over: {}", clean.cg_mem_kib);
}

#[test]
fn reset_empties_the_box_and_put_files_appear() {
    let meta = tempfile::tempdir().unwrap();
    let mut b = IsolateBox::open(&config(meta.path()), box_id(2), None).unwrap();
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
        let b = IsolateBox::open(&config(meta.path()), box_id(3), None).unwrap();
        b.dir().to_path_buf()
    };
    assert!(!dir.exists());
}

#[test]
fn file_names_that_leave_the_box_are_refused() {
    let meta = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let b = IsolateBox::open(&config(meta.path()), box_id(5), None).unwrap();
    let absolute = outside.path().join("x");
    for bad in [absolute.to_str().unwrap(), "../x", "a/b", "./x", "..", ".", "", "x/"] {
        assert!(matches!(b.put_bytes(bad, b"p"), Err(SandboxError::BadName(_))), "{bad:?}");
        assert!(matches!(b.read_output(bad, 10), Err(SandboxError::BadName(_))), "{bad:?}");
    }
    assert!(!absolute.exists());
}

#[test]
fn a_symlink_planted_in_the_box_is_replaced_not_followed() {
    let meta = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let victim = outside.path().join("victim");
    std::fs::write(&victim, "keep").unwrap();
    let b = IsolateBox::open(&config(meta.path()), box_id(6), None).unwrap();
    std::os::unix::fs::symlink(&victim, b.dir().join("input")).unwrap();
    b.put_bytes("input", b"new").unwrap();
    assert_eq!(std::fs::read_to_string(&victim).unwrap(), "keep");
    assert_eq!(std::fs::read_to_string(b.dir().join("input")).unwrap(), "new");
    std::os::unix::fs::symlink(&victim, b.dir().join("output")).unwrap();
    assert!(matches!(b.read_output("output", 100), Err(SandboxError::BadName(_))));
}

#[test]
fn put_tree_refuses_symlinks() {
    let meta = tempfile::tempdir().unwrap();
    let src = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink("/etc/hostname", src.path().join("link")).unwrap();
    let b = IsolateBox::open(&config(meta.path()), box_id(8), None).unwrap();
    assert!(b.put_tree(src.path()).is_err());
}

#[test]
fn a_second_run_without_reset_is_refused() {
    let meta = tempfile::tempdir().unwrap();
    let mut b = IsolateBox::open(&config(meta.path()), box_id(9), None).unwrap();
    b.run(&spec(&["/bin/true"], 65_536)).unwrap();
    assert!(matches!(b.run(&spec(&["/bin/true"], 65_536)), Err(SandboxError::Dirty)));
    b.reset().unwrap();
    b.run(&spec(&["/bin/true"], 65_536)).unwrap();
}

#[test]
fn put_tree_does_not_write_through_a_directory_symlink_in_the_box() {
    let meta = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let src = tempfile::tempdir().unwrap();
    std::fs::create_dir(src.path().join("sub")).unwrap();
    std::fs::write(src.path().join("sub/f"), "x").unwrap();
    let b = IsolateBox::open(&config(meta.path()), box_id(0), None).unwrap();
    std::os::unix::fs::symlink(outside.path(), b.dir().join("sub")).unwrap();
    assert!(b.put_tree(src.path()).is_err());
    assert!(!outside.path().join("f").exists());
}

#[test]
fn read_output_refuses_a_fifo_without_hanging() {
    let meta = tempfile::tempdir().unwrap();
    let b = IsolateBox::open(&config(meta.path()), box_id(1), None).unwrap();
    let fifo = std::ffi::CString::new(b.dir().join("output").to_str().unwrap()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o644) }, 0);
    assert!(matches!(b.read_output("output", 10), Err(SandboxError::BadName(_))));
}

#[test]
fn the_command_line_of_a_used_box_is_refused() {
    let meta = tempfile::tempdir().unwrap();
    let mut b = IsolateBox::open(&config(meta.path()), box_id(2), None).unwrap();
    b.run(&spec(&["/bin/true"], 65_536)).unwrap();
    assert!(matches!(b.command(&spec(&["/bin/true"], 65_536)), Err(SandboxError::Dirty)));
}

/// Bytes of the files in `dir`.
fn bytes_in(dir: &std::path::Path) -> u64 {
    std::fs::read_dir(dir).unwrap().flatten().map(|e| e.metadata().unwrap().len()).sum()
}

/// isolate keeps a box uid's last quota on the filesystem, so the test lifts it again for later users of the box.
const NO_LIMIT: DiskQuota = DiskQuota { headroom_kib: i32::MAX as u32, headroom_inodes: i32::MAX as u32 };

#[test]
fn a_run_writing_many_files_stays_inside_the_disk_quota() {
    let meta = tempfile::tempdir().unwrap();
    let quota = DiskQuota { headroom_kib: 4096, headroom_inodes: 64 };
    let config = SandboxConfig { disk_quota: Some(quota), ..config(meta.path()) };
    if !disk_quota_works(&config, box_id(90)).unwrap() {
        eprintln!("skip: isolate's box root has no user quotas on this host");
        return;
    }
    let mut b = IsolateBox::open(&config, box_id(90), None).unwrap();
    // 16 files of 1 MiB each in /box, every one under --fsize: only the quota stops the total.
    let flood =
        "for i in range(16):\n  try: open(f'f{i}', 'wb').write(b'x' * (1 << 20))\n  except OSError: pass";
    b.run(&spec(&["/usr/bin/python3", "-c", flood], 65_536)).unwrap();
    let written = bytes_in(b.dir());
    drop(b);
    disk_quota_works(&SandboxConfig { disk_quota: Some(NO_LIMIT), ..config }, box_id(90)).unwrap();
    assert!(written <= u64::from(quota.headroom_kib) * 1024, "{written} bytes under the box");
}

#[test]
fn an_input_larger_than_the_headroom_is_echoed_in_full() {
    let meta = tempfile::tempdir().unwrap();
    let config = SandboxConfig {
        disk_quota: Some(DiskQuota { headroom_kib: 4096, headroom_inodes: 64 }),
        ..config(meta.path())
    };
    if !disk_quota_works(&config, box_id(91)).unwrap() {
        eprintln!("skip: isolate's box root has no user quotas on this host");
        return;
    }
    let host = tempfile::tempdir().unwrap();
    let input = host.path().join("input");
    let len = 6u64 << 20;
    std::fs::write(&input, vec![b'7'; len as usize]).unwrap();
    let mut b = IsolateBox::open(&config, box_id(91), None).unwrap();
    // isolate charges the staged input to the box uid at --run; the room keeps it off the headroom.
    b.reset_with_room(Room::for_files(&[&input]).unwrap().plus_file(len)).unwrap();
    b.put("input", &input, false).unwrap();
    let mut cat = spec(&["/bin/cat", "input"], 65_536);
    cat.limits = Limits::for_solution(1.0, 65_536, 1, len);
    let meta = b.run(&cat).unwrap();
    let echoed = b.read_output("output", len + 1).unwrap().len() as u64;
    drop(b);
    disk_quota_works(&SandboxConfig { disk_quota: Some(NO_LIMIT), ..config }, box_id(91)).unwrap();
    assert_eq!((meta.exit_code, echoed), (Some(0), len));
}

/// The `cpus` isolate's config gives box `id`, when it gives one.
fn configured_cpus(id: u32) -> Option<String> {
    let config = std::fs::read_to_string("/usr/local/etc/isolate").ok()?;
    let key = format!("box{id}.cpus");
    config
        .lines()
        .filter_map(|l| l.split_once('='))
        .find(|(k, _)| k.trim() == key)
        .map(|(_, v)| v.trim().into())
}

#[test]
fn a_box_pinned_by_cpuset_cannot_move_to_another_core() {
    let Some(core) = configured_cpus(box_id(11)).and_then(|c| c.parse::<u32>().ok()) else {
        eprintln!("skip: isolate's config pins box {} to no single core", box_id(11));
        return;
    };
    let meta = tempfile::tempdir().unwrap();
    let mut b = IsolateBox::open(&config(meta.path()), box_id(11), Some(core)).unwrap();
    let escape =
        "import os\nos.sched_setaffinity(0, range(os.cpu_count()))\nprint(*sorted(os.sched_getaffinity(0)))";
    b.run(&spec(&["/usr/bin/python3", "-c", escape], 65_536)).unwrap();
    assert_eq!(b.read_output("output", 100).unwrap(), format!("{core}\n").into_bytes());
}
