//! A `file_io` case input held in memory by the judge for the length of a run.
//!
//! A page of a file is charged to the cgroup that brings it into the page cache, and not again when it is read.
//! The judge wrote the input, so its pages are the judge's; one evicted before the program reads it would be
//! charged to the box. The judge maps the whole file, faults it in (charged to the judge) and locks it, so the
//! box reads resident pages it is not charged for. Locking needs `LimitMEMLOCK` (packaging/qhhoj-judge.service);
//! without it the mapping still faults the pages in, but the host may evict them during the run.

use std::fs::File;
use std::os::fd::AsRawFd;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

/// One warning per process when the lock is refused.
static LOCK_REFUSED: AtomicBool = AtomicBool::new(false);

pub struct PinnedInput {
    addr: *mut libc::c_void,
    len: usize,
}

impl PinnedInput {
    /// Maps, faults in and locks the file at `path`; None for an empty file or one that cannot be mapped.
    pub fn new(path: &Path) -> Option<PinnedInput> {
        let file = File::open(path).ok()?;
        let len = usize::try_from(file.metadata().ok()?.len()).ok().filter(|len| *len > 0)?;
        let (prot, flags) = (libc::PROT_READ, libc::MAP_SHARED | libc::MAP_POPULATE);
        // SAFETY: a new read-only shared mapping of a whole regular file; nothing in Rust refers to it.
        let addr = unsafe { libc::mmap(std::ptr::null_mut(), len, prot, flags, file.as_raw_fd(), 0) };
        if addr == libc::MAP_FAILED {
            return None;
        }
        // SAFETY: the mapping above, `len` bytes long.
        if unsafe { libc::mlock(addr, len) } != 0 && !LOCK_REFUSED.swap(true, Ordering::Relaxed) {
            let error = std::io::Error::last_os_error();
            tracing::warn!("file_io input of {len} bytes not locked in memory ({error}); raise LimitMEMLOCK");
        }
        Some(PinnedInput { addr, len })
    }
}

impl Drop for PinnedInput {
    fn drop(&mut self) {
        // SAFETY: the mapping made in `new`; munmap also drops its lock.
        unsafe { libc::munmap(self.addr, self.len) };
    }
}
