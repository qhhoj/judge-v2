//! `<code>/.data-version`: written last by the backend (temp + rename), read here (docs/grading.md#caching-and-data-version).

use crate::init::InitError;
use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::time::{Duration, Instant};

pub const STAMP_NAME: &str = ".data-version";
/// An uploader creates this file before its first change and removes it after writing the stamp.
pub const WRITING_MARKER: &str = ".data-version.writing";

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DataError {
    #[error("bad problem code: {0:?}")]
    BadCode(String),
    #[error("data version {assigned} not available (found {found:?})")]
    Unavailable { assigned: u64, found: Option<u64> },
    #[error("problem data i/o: {0}")]
    Io(String),
    #[error("bad .data-version: {0:?}")]
    BadStamp(String),
    #[error(transparent)]
    Init(#[from] InitError),
    #[error("archive: {0}")]
    Archive(String),
}

impl From<std::io::Error> for DataError {
    fn from(error: std::io::Error) -> Self {
        DataError::Io(error.to_string())
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum StampCheck {
    Use(u64),
    Wait,
}

#[derive(Debug, Clone)]
pub struct WaitPolicy {
    pub timeout: Duration,
    pub poll: Duration,
}

/// Opens a problem file once, never through a symlink, and judges the opened handle:
/// a symlink planted on the data root must not copy the judge key or another box's files into a box.
/// `O_NONBLOCK` keeps a planted FIFO from hanging the open; the `fstat` of the handle then refuses it.
pub fn regular_file(path: &Path) -> Result<std::fs::File, DataError> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)?;
    if !file.metadata()?.is_file() {
        return Err(DataError::Io(format!("{} is not a regular file", path.display())));
    }
    Ok(file)
}

pub fn read_regular(path: &Path) -> Result<Vec<u8>, DataError> {
    let mut bytes = Vec::new();
    regular_file(path)?.read_to_end(&mut bytes)?;
    Ok(bytes)
}

pub fn read_regular_to_string(path: &Path) -> Result<String, DataError> {
    String::from_utf8(read_regular(path)?)
        .map_err(|_| DataError::Io(format!("{} is not UTF-8", path.display())))
}

pub fn read_stamp(dir: &Path) -> Result<Option<u64>, DataError> {
    let path = dir.join(STAMP_NAME);
    match std::fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
        Ok(_) => {}
    }
    let text = read_regular_to_string(&path)?;
    text.trim().parse().map(Some).map_err(|_| DataError::BadStamp(text))
}

/// Where the judge learns a problem's data version. The filesystem in production; scripted in tests,
/// so a stamp that moves between two reads can be reproduced exactly.
pub trait StampSource: Send + Sync {
    fn read(&self, dir: &Path) -> Result<Option<u64>, DataError>;

    /// True while an uploader is rewriting the problem's files: the stamp is written last, so it is
    /// still the old version during the whole upload.
    fn upload_in_progress(&self, dir: &Path) -> bool;
}

pub struct FsStamps;

impl StampSource for FsStamps {
    fn read(&self, dir: &Path) -> Result<Option<u64>, DataError> {
        read_stamp(dir)
    }

    fn upload_in_progress(&self, dir: &Path) -> bool {
        std::fs::symlink_metadata(dir.join(WRITING_MARKER)).is_ok()
    }
}

pub fn check_stamp(found: Option<u64>, assigned: u64) -> StampCheck {
    match found {
        None => StampCheck::Use(0),
        Some(version) if version >= assigned => StampCheck::Use(version),
        Some(_) => StampCheck::Wait,
    }
}

pub fn wait_for_stamp(
    dir: &Path,
    assigned: u64,
    policy: &WaitPolicy,
    stamps: &dyn StampSource,
) -> Result<u64, DataError> {
    let deadline = Instant::now() + policy.timeout;
    loop {
        let found = stamps.read(dir)?;
        if let (false, StampCheck::Use(version)) =
            (stamps.upload_in_progress(dir), check_stamp(found, assigned))
        {
            return Ok(version);
        }
        if Instant::now() >= deadline {
            return Err(DataError::Unavailable { assigned, found });
        }
        std::thread::sleep(policy.poll);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
    use std::time::{Duration, Instant};

    fn quick() -> WaitPolicy {
        WaitPolicy { timeout: Duration::from_millis(300), poll: Duration::from_millis(20) }
    }

    #[test]
    fn stamp_is_read_with_or_without_newline_missing_is_none_and_garbage_fails() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read_stamp(dir.path()).unwrap(), None);
        std::fs::write(dir.path().join(".data-version"), "7\n").unwrap();
        assert_eq!(read_stamp(dir.path()).unwrap(), Some(7));
        std::fs::write(dir.path().join(".data-version"), "12").unwrap();
        assert_eq!(read_stamp(dir.path()).unwrap(), Some(12));
        std::fs::write(dir.path().join(".data-version"), "seven").unwrap();
        assert!(matches!(read_stamp(dir.path()), Err(DataError::BadStamp(_))));
    }

    #[test]
    fn symlinked_files_are_refused_and_regular_files_read() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        std::fs::write(&real, "9\n").unwrap();
        assert_eq!(read_regular_to_string(&real).unwrap(), "9\n");
        std::os::unix::fs::symlink(&real, dir.path().join(".data-version")).unwrap();
        assert!(matches!(read_stamp(dir.path()), Err(DataError::Io(_))));
        std::os::unix::fs::symlink("/etc/hostname", dir.path().join("1.in")).unwrap();
        assert!(matches!(read_regular(&dir.path().join("1.in")), Err(DataError::Io(_))));
        assert!(
            matches!(regular_file(dir.path()), Err(DataError::Io(_))),
            "a directory is not a regular file"
        );
    }

    #[test]
    fn check_policy_follows_the_spec() {
        assert_eq!(check_stamp(None, 5), StampCheck::Use(0));
        assert_eq!(check_stamp(Some(5), 5), StampCheck::Use(5));
        assert_eq!(check_stamp(Some(9), 5), StampCheck::Use(9));
        assert_eq!(check_stamp(Some(4), 5), StampCheck::Wait);
    }

    #[test]
    fn a_lagging_stamp_times_out_as_unavailable() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".data-version"), "1").unwrap();
        let started = Instant::now();
        let error = wait_for_stamp(dir.path(), 2, &quick(), &FsStamps).unwrap_err();
        assert_eq!(error, DataError::Unavailable { assigned: 2, found: Some(1) });
        assert!(started.elapsed() >= Duration::from_millis(300));
    }

    /// Real stamps, plus a hook that runs before the n-th `read` and the n-th marker check (each counted
    /// from 1), so an uploader's step lands between two polls of the wait without a racing thread.
    #[derive(Default)]
    struct ScriptedStamps {
        reads: AtomicUsize,
        marker_checks: AtomicUsize,
        before_read: Option<Box<dyn Fn(usize) + Send + Sync>>,
        before_marker_check: Option<Box<dyn Fn(usize) + Send + Sync>>,
    }

    impl StampSource for ScriptedStamps {
        fn read(&self, dir: &Path) -> Result<Option<u64>, DataError> {
            let n = self.reads.fetch_add(1, SeqCst) + 1;
            self.before_read.as_ref().inspect(|hook| hook(n));
            FsStamps.read(dir)
        }

        fn upload_in_progress(&self, dir: &Path) -> bool {
            let n = self.marker_checks.fetch_add(1, SeqCst) + 1;
            self.before_marker_check.as_ref().inspect(|hook| hook(n));
            FsStamps.upload_in_progress(dir)
        }
    }

    /// The scripted sources decide when the stamp moves, so the deadline is only a backstop here.
    fn backstop() -> WaitPolicy {
        WaitPolicy { timeout: Duration::from_secs(60), poll: Duration::from_millis(1) }
    }

    #[test]
    fn a_stamp_that_catches_up_is_used() {
        let dir = tempfile::tempdir().unwrap();
        let stamp = dir.path().join(".data-version");
        std::fs::write(&stamp, "1").unwrap();
        let before_read = move |n: usize| {
            if n == 2 {
                let temp = stamp.with_extension("tmp");
                std::fs::write(&temp, "2\n").unwrap();
                std::fs::rename(&temp, &stamp).unwrap();
            }
        };
        let stamps = ScriptedStamps { before_read: Some(Box::new(before_read)), ..Default::default() };
        assert_eq!(wait_for_stamp(dir.path(), 2, &backstop(), &stamps).unwrap(), 2);
        assert_eq!(stamps.reads.load(SeqCst), 2, "waited on stamp 1, used stamp 2 on the next poll");
    }

    #[test]
    fn an_upload_marker_holds_the_wait_until_it_is_removed() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".data-version"), "2").unwrap();
        let marker = dir.path().join(WRITING_MARKER);
        std::fs::write(&marker, "").unwrap();
        let error = wait_for_stamp(dir.path(), 2, &quick(), &FsStamps).unwrap_err();
        assert_eq!(error, DataError::Unavailable { assigned: 2, found: Some(2) });
        let before_marker_check = move |n: usize| {
            if n == 2 {
                std::fs::remove_file(&marker).unwrap();
            }
        };
        let stamps =
            ScriptedStamps { before_marker_check: Some(Box::new(before_marker_check)), ..Default::default() };
        assert_eq!(wait_for_stamp(dir.path(), 2, &backstop(), &stamps).unwrap(), 2);
        assert_eq!(stamps.marker_checks.load(SeqCst), 2, "held while the marker was there");
    }

    #[test]
    fn a_fifo_is_refused_without_hanging() {
        let dir = tempfile::tempdir().unwrap();
        let fifo = dir.path().join("pipe");
        let status = std::process::Command::new("mkfifo").arg(&fifo).status().unwrap();
        assert!(status.success());
        assert!(matches!(regular_file(&fifo), Err(DataError::Io(_))));
    }
}
