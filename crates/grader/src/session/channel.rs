//! The split channel: one sealed 3 MiB memfd both boxes map, and two doorbell pipes. The daemon creates it,
//! never maps it (so the repo keeps zero `unsafe`), hands each side its three fds through
//! `IsolateBox::command_with_channel`, and drops every copy once both sides are spawned.
//!
//! Layout of the shared memory: the contestant-to-jury region at 0, the jury-to-contestant region at 1 MiB, the
//! void-call log at 2 MiB, each `REGION_BYTES` long.

use crate::error::GradeError;
use judge_sandbox::ChannelFds;
use rustix::fs::{MemfdFlags, Mode, OFlags, SealFlags};
use std::os::fd::OwnedFd;
use std::os::unix::fs::FileExt;

pub const RING_BYTES: u64 = 3 << 20;
pub const REGION_BYTES: u64 = 1 << 20;
/// The page the jury reports its state on, staged in the jury box (`files::stage_session_page`).
pub const SESSION_PAGE: &str = ".qhhoj-session";
pub const SESSION_PAGE_BYTES: usize = 4096;

/// The channel of one split session, created by the daemon. Dropped (every fd closed) once both sides spawned.
pub struct RingChannel {
    pub jury: ChannelFds,
    pub contestant: ChannelFds,
}

impl RingChannel {
    pub fn create() -> Result<RingChannel, GradeError> {
        let shared = sealed_memory()?;
        let (jury_wait, contestant_ring) = doorbell()?;
        let (contestant_wait, jury_ring) = doorbell()?;
        let contestant_shared = shared.try_clone()?;
        Ok(RingChannel {
            jury: ChannelFds { doorbell_wait: jury_wait, doorbell_ring: jury_ring, shared },
            contestant: ChannelFds {
                doorbell_wait: contestant_wait,
                doorbell_ring: contestant_ring,
                shared: contestant_shared,
            },
        })
    }
}

/// memfd, sized, every page written once (pre-touched, so no side takes a fault on first use and neither box is
/// charged for it), sealed against resizing and further seals, and owner-only so a box uid cannot reopen it by
/// `/proc/self/fd/5`.
fn sealed_memory() -> Result<OwnedFd, GradeError> {
    let fd = rustix::fs::memfd_create("qhhoj-ring", MemfdFlags::CLOEXEC | MemfdFlags::ALLOW_SEALING)
        .map_err(io)?;
    rustix::fs::ftruncate(&fd, RING_BYTES).map_err(io)?;
    let file = std::fs::File::from(fd);
    let zeros = vec![0u8; REGION_BYTES as usize];
    for region in 0..RING_BYTES / REGION_BYTES {
        file.write_all_at(&zeros, region * REGION_BYTES)?;
    }
    rustix::fs::fcntl_add_seals(&file, SealFlags::SHRINK | SealFlags::GROW | SealFlags::SEAL).map_err(io)?;
    rustix::fs::fchmod(&file, Mode::RUSR | Mode::WUSR).map_err(io)?;
    Ok(file.into())
}

/// One doorbell: (read end, write end), the write end non-blocking (a full pipe means "already rung").
fn doorbell() -> Result<(OwnedFd, OwnedFd), GradeError> {
    let (wait, ring) = std::io::pipe()?;
    let ring = OwnedFd::from(ring);
    let flags = rustix::fs::fcntl_getfl(&ring).map_err(io)?;
    rustix::fs::fcntl_setfl(&ring, flags | OFlags::NONBLOCK).map_err(io)?;
    Ok((wait.into(), ring))
}

fn io(errno: rustix::io::Errno) -> GradeError {
    GradeError::from(std::io::Error::from(errno))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustix::fs::{SealFlags, fcntl_get_seals, fstat};
    use std::io::{Read, Write};

    #[test]
    fn the_shared_memory_is_three_zeroed_mebibytes_sealed_against_resizing() {
        let ring = RingChannel::create().unwrap();
        let shared = std::fs::File::from(ring.jury.shared);
        let mut bytes = vec![1u8; RING_BYTES as usize];
        shared.read_exact_at(&mut bytes, 0).unwrap();
        assert!(bytes.iter().all(|b| *b == 0));
        let seals = fcntl_get_seals(&shared).unwrap();
        assert!(seals.contains(SealFlags::SHRINK | SealFlags::GROW | SealFlags::SEAL), "{seals:?}");
        assert!(!seals.contains(SealFlags::WRITE), "both sides still write it");
        assert_eq!(fstat(&shared).unwrap().st_mode & 0o777, 0o600);
        assert_eq!(rustix::fs::ftruncate(&shared, RING_BYTES * 2), Err(rustix::io::Errno::PERM));
    }

    #[test]
    fn both_sides_share_one_memory() {
        let ring = RingChannel::create().unwrap();
        let jury = std::fs::File::from(ring.jury.shared);
        let contestant = std::fs::File::from(ring.contestant.shared);
        jury.write_all_at(b"ping", REGION_BYTES).unwrap();
        let mut seen = [0u8; 4];
        contestant.read_exact_at(&mut seen, REGION_BYTES).unwrap();
        assert_eq!(&seen, b"ping");
    }

    #[test]
    fn each_doorbell_rings_the_other_side_and_only_ring_ends_are_non_blocking() {
        let RingChannel { jury, contestant } = RingChannel::create().unwrap();
        let nonblocking =
            |fd: &OwnedFd| rustix::fs::fcntl_getfl(fd).unwrap().contains(rustix::fs::OFlags::NONBLOCK);
        assert!(nonblocking(&jury.doorbell_ring) && nonblocking(&contestant.doorbell_ring));
        assert!(!nonblocking(&jury.doorbell_wait) && !nonblocking(&contestant.doorbell_wait));
        std::fs::File::from(contestant.doorbell_ring).write_all(b"x").unwrap();
        let mut wait = std::fs::File::from(jury.doorbell_wait);
        let mut byte = [0u8; 1];
        wait.read_exact(&mut byte).unwrap();
        assert_eq!(wait.read(&mut byte).unwrap(), 0, "the ringer's only write end is gone: EOF, a dead peer");
    }
}
