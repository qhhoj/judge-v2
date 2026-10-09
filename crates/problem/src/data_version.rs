//! `<code>/.data-version`: written last by the backend (temp + rename), read here (spec §2).

use crate::init::InitError;
use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::time::{Duration, Instant};

pub const STAMP_NAME: &str = ".data-version";

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

/// Opens a problem file only if it is a regular file, never through a symlink (preflight I11):
/// a symlink planted on the data root must not copy the judge key or another box's files into a box.
pub fn regular_file(path: &Path) -> Result<std::fs::File, DataError> {
    if !std::fs::symlink_metadata(path)?.file_type().is_file() {
        return Err(DataError::Io(format!("{} is not a regular file", path.display())));
    }
    let file = std::fs::OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW).open(path)?;
    if !file.metadata()?.is_file() {
        return Err(DataError::Io(format!("{} changed while opening", path.display())));
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

pub fn check_stamp(found: Option<u64>, assigned: u64) -> StampCheck {
    match found {
        None => StampCheck::Use(0),
        Some(version) if version >= assigned => StampCheck::Use(version),
        Some(_) => StampCheck::Wait,
    }
}

pub fn wait_for_stamp(dir: &Path, assigned: u64, policy: &WaitPolicy) -> Result<u64, DataError> {
    let deadline = Instant::now() + policy.timeout;
    loop {
        let found = read_stamp(dir)?;
        if let StampCheck::Use(version) = check_stamp(found, assigned) {
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
    use std::time::{Duration, Instant};

    fn quick() -> WaitPolicy {
        WaitPolicy { timeout: Duration::from_millis(300), poll: Duration::from_millis(20) }
    }

    #[test]
    fn stamp_is_read_with_or_without_newline_and_missing_is_none() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read_stamp(dir.path()).unwrap(), None);
        std::fs::write(dir.path().join(".data-version"), "7\n").unwrap();
        assert_eq!(read_stamp(dir.path()).unwrap(), Some(7));
        std::fs::write(dir.path().join(".data-version"), "12").unwrap();
        assert_eq!(read_stamp(dir.path()).unwrap(), Some(12));
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
    fn garbage_stamp_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".data-version"), "seven").unwrap();
        assert!(matches!(read_stamp(dir.path()), Err(DataError::BadStamp(_))));
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
        let error = wait_for_stamp(dir.path(), 2, &quick()).unwrap_err();
        assert_eq!(error, DataError::Unavailable { assigned: 2, found: Some(1) });
        assert!(started.elapsed() >= Duration::from_millis(300));
    }

    #[test]
    fn a_stamp_that_catches_up_is_used() {
        let dir = tempfile::tempdir().unwrap();
        let stamp = dir.path().join(".data-version");
        std::fs::write(&stamp, "1").unwrap();
        let writer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(60));
            let temp = stamp.with_extension("tmp");
            std::fs::write(&temp, "2\n").unwrap();
            std::fs::rename(temp, stamp).unwrap();
        });
        assert_eq!(wait_for_stamp(dir.path(), 2, &quick()).unwrap(), 2);
        writer.join().unwrap();
    }
}
