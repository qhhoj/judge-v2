//! What keeps the runs from filling the disk (DEPLOY.md "Disk quota on the box root"). `--fsize` caps one
//! file and the cgroup only the page cache, so a run that writes many files is held only by a disk quota on
//! its box or by a box root on a filesystem of its own. The daemon refuses to start with neither.

use crate::config::Config;
use crate::cpuset::settings;
use anyhow::Context;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

/// isolate's built-in box root, used when its config sets none.
const DEFAULT_BOX_ROOT: &str = "/var/local/lib/isolate";

/// The size bound on isolate's box root the daemon starts with.
#[derive(Debug, PartialEq, Eq)]
pub enum DiskBound {
    /// `disk_quota` is set and isolate can set it: every box is capped.
    Quota,
    /// No quota, but the box root is its own filesystem: all boxes together are capped by its size.
    OwnFilesystem,
    /// `allow_unbounded_disk` (dev and test stacks only).
    Unbounded,
}

impl DiskBound {
    pub fn log(&self) {
        match self {
            DiskBound::Quota => tracing::info!("every box has a disk quota"),
            DiskBound::OwnFilesystem => {
                tracing::warn!("box root on its own filesystem, no disk quota (DEPLOY.md)")
            }
            DiskBound::Unbounded => tracing::warn!("no disk bound on the box root (allow_unbounded_disk)"),
        }
    }
}

/// `box_root` of an isolate config.
pub fn box_root(isolate_config: &str) -> PathBuf {
    let configured = settings(isolate_config).find(|(key, _)| *key == "box_root");
    configured.map_or(DEFAULT_BOX_ROOT, |(_, value)| value).into()
}

fn device(path: &Path) -> anyhow::Result<u64> {
    Ok(std::fs::metadata(path).with_context(|| format!("{}", path.display()))?.dev())
}

/// True when the box root is on another filesystem than `/` and the cache dir.
fn own_filesystem(box_root: &Path, cache_dir: &Path) -> anyhow::Result<bool> {
    let boxes = device(box_root).context("isolate's box root")?;
    Ok(boxes != device(Path::new("/"))? && boxes != device(cache_dir)?)
}

/// The bound when no disk quota is configured: the box root's own filesystem, else the dev escape hatch.
pub fn without_quota(config: &Config) -> anyhow::Result<DiskBound> {
    let isolate_config = std::fs::read_to_string(&config.isolate.config).unwrap_or_default();
    let root = box_root(&isolate_config);
    if own_filesystem(&root, &config.cache_dir)? {
        return Ok(DiskBound::OwnFilesystem);
    }
    if config.isolate.allow_unbounded_disk {
        return Ok(DiskBound::Unbounded);
    }
    anyhow::bail!(
        "isolate's box root {} shares a filesystem with / or the cache and [isolate] has no disk_quota: one run \
         could fill the disk. Set disk_quota on a box root with user quotas (DEPLOY.md)",
        root.display()
    )
}
