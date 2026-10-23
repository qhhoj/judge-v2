//! Test-only stand-in for isolate: every call is logged to `calls` beside the script, `--init` answers with a box
//! root there, `--run` sleeps 30 s (as `exec`, so killing the isolate process kills the sleep), every other call
//! succeeds.

use judge_sandbox::{IsolateBox, SandboxConfig};
use std::path::Path;
use std::process::Command;

const SLEEPING_ISOLATE: &str = r#"#!/bin/sh
d=$(dirname "$0")
echo "$*" >> "$d/calls"
case "$*" in *--init*) mkdir -p "$d/root/box"; echo "$d/root" ;; esac
case "$*" in *--run*) exec sleep 30 ;; esac
"#;

/// A box opened through the sleeping fake written to `dir`.
pub fn fake_box(dir: &Path, id: u32) -> IsolateBox {
    let isolate_bin = dir.join("isolate");
    write_executable(&isolate_bin, SLEEPING_ISOLATE);
    let config = SandboxConfig { isolate_bin, meta_dir: dir.join("meta"), ..SandboxConfig::default() };
    IsolateBox::open(&config, id, None).unwrap()
}

/// Writes `script` as an executable without this process ever holding it open for writing: a test thread that
/// forks meanwhile would keep that fd until its exec, and running the script would then fail with ETXTBSY.
fn write_executable(path: &Path, script: &str) {
    let status = Command::new("/bin/sh")
        .args(["-c", r#"printf '%s' "$1" > "$0" && chmod 755 "$0""#])
        .arg(path)
        .arg(script)
        .status()
        .unwrap();
    assert!(status.success());
}
