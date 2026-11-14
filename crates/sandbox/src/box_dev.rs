//! The `/dev` of a box: a judge-made directory of empty placeholders and stream links, bound read-only, with
//! each host device bound over its placeholder. A box sees these devices and links and nothing else of the
//! host's `/dev` (docs/grading.md#box-lifecycle).

use crate::error::SandboxError;
use std::fs::OpenOptions;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

/// The host devices a box gets.
pub const DEVICES: [&str; 5] = ["null", "zero", "full", "random", "urandom"];

/// The links a box gets, as on a host: the open fds and the three standard streams.
pub const LINKS: [(&str, &str); 4] = [
    ("fd", "/proc/self/fd"),
    ("stdin", "/proc/self/fd/0"),
    ("stdout", "/proc/self/fd/1"),
    ("stderr", "/proc/self/fd/2"),
];

/// Makes the directory at `dir` (mode 0755, so the box uid can enter it) with one empty file per device and
/// the links. Leaves what is already there.
pub fn prepare(dir: &Path) -> Result<(), SandboxError> {
    std::fs::create_dir_all(dir)?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o755))?;
    for device in DEVICES {
        OpenOptions::new().write(true).create(true).truncate(false).mode(0o644).open(dir.join(device))?;
    }
    for (name, target) in LINKS {
        match std::os::unix::fs::symlink(target, dir.join(name)) {
            Err(error) if error.kind() != std::io::ErrorKind::AlreadyExists => return Err(error.into()),
            _ => {}
        }
    }
    Ok(())
}

/// isolate's rules for this `/dev`: `dir` in place of the host's, then each host device over its placeholder
/// (`dev`: device nodes work on that mount only).
pub fn dir_rules(dir: &Path) -> Vec<String> {
    let mut rules = vec![format!("dev={}", dir.display())];
    rules.extend(DEVICES.iter().map(|device| format!("dev/{device}=/dev/{device}:dev")));
    rules
}
