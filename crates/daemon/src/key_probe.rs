//! The start check that isolate refuses the key syscalls in a box: `add_key` and `request_key` as well
//! as `keyctl`, as isolate built with `packaging/isolate-2.6-keys.patch` does (DEPLOY.md step 2). One
//! run of this binary's hidden `key-probe` command in the probe box asks for a key and prints what the
//! kernel answered.

use crate::config::Config;
use anyhow::Context;
use judge_sandbox::spec::{Limits, RunSpec, Stdio};
use judge_sandbox::{IsolateBox, SandboxConfig};

/// The hidden `qhhoj-judge` command the probe runs in the box.
pub const COMMAND: &str = "key-probe";
/// What the probe prints when `add_key` succeeded.
pub const ADDED: &str = "added";

/// The probe's answer: `ADDED`, or the errno `add_key` failed with. The key goes to the probe's own process
/// keyring, which ends with the probe.
pub fn add_key_answer() -> String {
    const KEY_SPEC_PROCESS_KEYRING: libc::c_long = -2;
    // SAFETY: every pointer is to a NUL-terminated literal or a live one-byte payload whose length is passed.
    let id = unsafe {
        libc::syscall(
            libc::SYS_add_key,
            c"user".as_ptr(),
            c"qhhoj-key-probe".as_ptr(),
            b"x".as_ptr(),
            1usize,
            KEY_SPEC_PROCESS_KEYRING,
        )
    };
    if id >= 0 {
        return ADDED.into();
    }
    format!("errno {}", std::io::Error::last_os_error().raw_os_error().unwrap_or(0))
}

/// Ok when the box refused `add_key`: ENOSYS (patched isolate) or EPERM (a container's syscall filter).
fn verdict(answer: &str) -> anyhow::Result<()> {
    let refused = [libc::ENOSYS, libc::EPERM].map(|errno| format!("errno {errno}"));
    match answer.trim() {
        answer if refused.iter().any(|r| r == answer) => Ok(()),
        ADDED => anyhow::bail!(
            "a box may add kernel keys: isolate is built without packaging/isolate-2.6-keys.patch. \
             Rebuild it with the patch (DEPLOY.md step 2) and run check-config again"
        ),
        other => anyhow::bail!(
            "the key probe in a box answered {other:?}, not \"errno {}\": check isolate and DEPLOY.md step 2",
            libc::ENOSYS
        ),
    }
}

/// Runs the probe on `probe_box` (opened, so wiped, and cleaned again); `allow_unpatched_isolate` skips it.
pub fn check(config: &Config, sandbox: &SandboxConfig, probe_box: u32) -> anyhow::Result<()> {
    if config.isolate.allow_unpatched_isolate {
        tracing::warn!("isolate key syscalls not checked (allow_unpatched_isolate)");
        return Ok(());
    }
    let exe = std::env::current_exe().context("the key probe: this binary")?;
    let mut probe = IsolateBox::open(sandbox, probe_box, None).context("the key probe box")?;
    probe.put("qhhoj-judge", &exe, true)?;
    let spec = RunSpec {
        argv: vec!["./qhhoj-judge".into(), COMMAND.into()],
        limits: Limits::for_solution(2.0, 256 << 10, 1, 4096),
        stdio: Stdio { stdin: None, stdout: Some("output".into()), stderr: None },
        env: Vec::new(),
        dirs: Vec::new(),
        preload: Vec::new(),
        inherit_fds: false,
    };
    let meta = probe.run(&spec).context("the key probe run")?;
    let answer = String::from_utf8_lossy(&probe.read_output("output", 256)?).into_owned();
    let refused = verdict(&answer);
    match meta.status {
        Some(status) => {
            refused.with_context(|| format!("the key probe ended with {status} {}", meta.message))
        }
        None => refused,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_refused_add_key_passes() {
        assert!(verdict(&format!("errno {}\n", libc::ENOSYS)).is_ok(), "patched isolate");
        assert!(verdict(&format!("errno {}\n", libc::EPERM)).is_ok(), "a container's syscall filter");
        let added = verdict("added\n").expect_err("an unpatched isolate");
        assert!(added.to_string().contains("packaging/isolate-2.6-keys.patch"), "{added}");
        assert!(verdict("").is_err(), "a probe that printed nothing");
        assert!(verdict(&format!("errno {}", libc::EACCES)).is_err());
    }
}
