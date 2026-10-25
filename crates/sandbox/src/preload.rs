//! Loads a language runtime's files into the page cache from the judge's own process, right before a run.
//!
//! The kernel charges a page-cache page to the cgroup that first reads it in. A run's memory includes its
//! cgroup peak (`Meta::memory_kib`), so a JVM whose `libjvm.so`, CDS archive and `modules` were not in the
//! page cache reported them as its own: 46 MiB more for a hello world than when another run had read them.
//! Read in here first, those pages are charged to the judge and every run reports the same memory.
//! A page evicted between this call and the run's first touch of it is still charged to the box.

use rustix::fs::{Advice, fadvise};
use std::num::NonZeroU64;
use std::path::Path;

/// One `WILLNEED` call reads at most the device's readahead window (`force_page_cache_ra`), often 128 KiB:
/// a file is asked for in windows of that size, or only its start is read.
const WINDOW: NonZeroU64 = NonZeroU64::new(128 << 10).unwrap();

/// Asks the kernel to read every regular file under `paths` (files or directories, symlinks not followed)
/// into the page cache. Best effort: a path that cannot be read is skipped; it changes accounting only.
pub fn preload(paths: &[String]) {
    for path in paths {
        preload_tree(Path::new(path));
    }
}

fn preload_tree(path: &Path) {
    let Ok(entries) = std::fs::read_dir(path) else {
        preload_file(path);
        return;
    };
    for entry in entries.flatten() {
        match entry.file_type() {
            Ok(kind) if kind.is_dir() => preload_tree(&entry.path()),
            Ok(kind) if kind.is_file() => preload_file(&entry.path()),
            _ => {}
        }
    }
}

fn preload_file(path: &Path) {
    let Ok(file) = std::fs::File::open(path) else { return };
    let Ok(len) = file.metadata().map(|meta| meta.len()) else { return };
    for offset in (0..len).step_by(WINDOW.get() as usize) {
        if fadvise(&file, offset, Some(WINDOW), Advice::WillNeed).is_err() {
            return;
        }
    }
}
