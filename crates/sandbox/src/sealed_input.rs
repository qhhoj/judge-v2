//! A case input as a sealed memfd the judge gives a run as its stdin (`IsolateBox::run_drained`).
//!
//! A memfd's pages are shmem charged to the judge that wrote them, and page-cache reclaim never drops them, so
//! the run is never charged for its input however hard the host's memory is pressed; a file's page cache
//! evicted before the program reads it is charged to the box. The copy is sealed against writing and resizing,
//! so the box cannot keep memory of its own in it; reading, seeking, `fstat` and mapping work as on a file.
//! The write seal is `F_SEAL_FUTURE_WRITE` (Linux 5.1): before Linux 6.7, `F_SEAL_WRITE` also refuses a
//! read-only shared mapping, a common fast-input idiom.

use crate::error::SandboxError;
use rustix::fs::{MemfdFlags, SealFlags};
use std::fs::File;
use std::io::Seek;
use std::path::Path;

/// A sealed memfd holding the file at `source`, read from its start, its offset at 0.
pub fn sealed_copy(source: &Path) -> Result<File, SandboxError> {
    let flags = MemfdFlags::CLOEXEC | MemfdFlags::ALLOW_SEALING;
    let mut copy = File::from(rustix::fs::memfd_create("qhhoj-input", flags).map_err(std::io::Error::from)?);
    std::io::copy(&mut File::open(source)?, &mut copy)?;
    let seals = SealFlags::FUTURE_WRITE | SealFlags::SHRINK | SealFlags::GROW | SealFlags::SEAL;
    rustix::fs::fcntl_add_seals(&copy, seals).map_err(std::io::Error::from)?;
    copy.rewind()?;
    Ok(copy)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::FileExt;

    #[test]
    fn the_copy_reads_as_the_file_from_offset_0_and_takes_no_write() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("in");
        let bytes: Vec<u8> = (0..300_000u32).map(|i| (i % 253) as u8).collect();
        std::fs::write(&path, &bytes).unwrap();
        let mut copy = sealed_copy(&path).unwrap();
        assert_eq!(copy.metadata().unwrap().len(), bytes.len() as u64);
        let mut read = Vec::new();
        copy.read_to_end(&mut read).unwrap();
        assert_eq!(read, bytes);
        assert!(copy.write_all(b"x").is_err() && copy.write_all_at(b"x", 0).is_err());
        assert!(copy.set_len(1).is_err() && copy.set_len(1 << 30).is_err());
        let by_path = format!("/proc/self/fd/{}", copy.as_raw_fd());
        if let Ok(reopened) = std::fs::OpenOptions::new().write(true).open(by_path) {
            assert!(reopened.write_all_at(b"x", 0).is_err(), "reopened by path");
        }
    }
}
