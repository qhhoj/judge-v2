//! isolate's own config file (`/usr/local/etc/isolate`): the settings the judge reads from it.

use std::path::{Path, PathBuf};

/// `key = value` settings of an isolate config, comments and blank lines skipped.
pub fn settings(isolate_config: &str) -> impl Iterator<Item = (&str, &str)> {
    isolate_config
        .lines()
        .map(str::trim)
        .filter(|line| !line.starts_with('#'))
        .filter_map(|line| line.split_once('='))
        .map(|(key, value)| (key.trim(), value.trim()))
}

/// isolate's cgroup parent: `cg_root`, or for `auto:FILE` the path isolate's cg keeper wrote to FILE.
pub fn cgroup_parent(isolate_config: &str) -> Option<PathBuf> {
    let cg_root = settings(isolate_config).find(|(key, _)| *key == "cg_root")?.1;
    match cg_root.strip_prefix("auto:") {
        Some(file) => Some(std::fs::read_to_string(file).ok()?.lines().next()?.trim().into()),
        None => Some(cg_root.into()),
    }
}

/// The `memory.stat` file of box `id`'s cgroup, from the isolate config at `isolate_config`.
pub fn box_memory_stat(isolate_config: &Path, id: u32) -> Option<PathBuf> {
    let parent = cgroup_parent(&std::fs::read_to_string(isolate_config).ok()?)?;
    Some(parent.join(format!("box-{id}")).join("memory.stat"))
}
