//! Zip extraction under limits: no traversal, no symlinks, no lying sizes, and only archives the backend's reader
//! reads the same way (docs/grading.md#caching-and-data-version).

use crate::data_version::DataError;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone)]
pub struct ZipLimits {
    pub max_entries: usize,
    pub max_total_bytes: u64,
}

impl Default for ZipLimits {
    fn default() -> Self {
        ZipLimits { max_entries: 100_000, max_total_bytes: 8 << 30 }
    }
}

/// The most an archive the backend accepts extracts to (its `ZIP_LIMITS.maxTotalBytes`; the contract test compares
/// the two).
pub const BACKEND_ARCHIVE_MAX_BYTES: u64 = 2 << 30;
/// The smallest `cache_max_bytes` the daemon runs with: half of it must hold the largest archive the backend accepts.
pub const MIN_CACHE_MAX_BYTES: u64 = 2 * BACKEND_ARCHIVE_MAX_BYTES;

impl ZipLimits {
    /// One archive may fill at most half the cache, so one upload cannot evict everything else.
    pub fn for_cache(cache_max_bytes: u64) -> Self {
        let default = ZipLimits::default();
        ZipLimits { max_total_bytes: default.max_total_bytes.min(cache_max_bytes / 2), ..default }
    }
}

fn archive_error(message: impl std::fmt::Display) -> DataError {
    DataError::Archive(message.to_string())
}

/// Zips made on Windows store `tests\1.in`; `\` is a separator, never part of a name. The segments are
/// checked after the conversion. A control character never reaches a name the backend shows (it refuses them,
/// and shows CP437 control bytes as symbols), so a case could not name the file.
fn safe_relative(name: &str) -> Result<PathBuf, DataError> {
    let name = name.replace('\\', "/");
    let path = Path::new(&name);
    let normal = path.components().all(|c| matches!(c, Component::Normal(_)));
    let control = name.chars().any(|c| c < ' ' || c == '\u{7f}');
    if name.is_empty() || !normal || control {
        return Err(archive_error(format!("unsafe entry name {name:?}")));
    }
    Ok(path.to_path_buf())
}

const S_IFMT: u32 = 0o170000;
const S_IFLNK: u32 = 0o120000;
const UNICODE_PATH: u16 = 0x7075;
const UNICODE_PATH_VERSION: u8 = 1;

/// The data of each Info-ZIP Unicode Path field among an entry's extra fields.
fn unicode_paths(mut extra: &[u8]) -> Vec<&[u8]> {
    let mut fields = Vec::new();
    while let [id_low, id_high, len_low, len_high, rest @ ..] = extra {
        let len = usize::from(u16::from_le_bytes([*len_low, *len_high])).min(rest.len());
        if u16::from_le_bytes([*id_low, *id_high]) == UNICODE_PATH {
            fields.push(&rest[..len]);
        }
        extra = &rest[len..];
    }
    fields
}

/// The crate names an entry after its last Unicode Path field of any version, the backend after the first
/// version-1 one: only a single version-1 field names it the same on both.
fn unicode_path_agrees(extra: Option<&[u8]>) -> bool {
    match unicode_paths(extra.unwrap_or_default()).as_slice() {
        [] => true,
        [field] => field.first() == Some(&UNICODE_PATH_VERSION),
        _ => false,
    }
}

/// Where the entry goes, or why the judge refuses it.
fn entry_path(entry: &zip::read::ZipFile) -> Result<PathBuf, DataError> {
    if entry.unix_mode().is_some_and(|mode| mode & S_IFMT == S_IFLNK) {
        return Err(archive_error(format!("symlink entry {:?}", entry.name())));
    }
    if !unicode_path_agrees(entry.extra_data()) {
        return Err(archive_error(format!("entry {:?} has an ambiguous Unicode path", entry.name())));
    }
    safe_relative(entry.name())
}

/// Opens the archive at the central directory its end records name, the one the backend read.
fn open_named_directory(file: &std::fs::File) -> Result<zip::ZipArchive<&std::fs::File>, DataError> {
    let named = crate::zip_directory::named_directory(file).map_err(archive_error)?;
    let archive = zip::ZipArchive::new(file).map_err(archive_error)?;
    if archive.offset() != 0 || archive.central_directory_start() != named.start {
        return Err(archive_error("the central directory read is not the one the end records name"));
    }
    Ok(archive)
}

pub fn extract_zip(zip: &Path, into: &Path, limits: &ZipLimits) -> Result<(), DataError> {
    let file = crate::data_version::regular_file(zip)?;
    let mut archive = open_named_directory(&file)?;
    if archive.len() > limits.max_entries {
        return Err(archive_error(format!("more than {} entries", limits.max_entries)));
    }
    let mut budget = limits.max_total_bytes;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(archive_error)?;
        let relative = entry_path(&entry)?;
        if entry.is_dir() {
            std::fs::create_dir_all(into.join(&relative))?;
            continue;
        }
        budget = copy_entry(&mut entry, &into.join(relative), budget)?;
    }
    Ok(())
}

/// Copies at most `budget` bytes; the real byte count is checked, not the declared size.
fn copy_entry(entry: &mut impl Read, target: &Path, budget: u64) -> Result<u64, DataError> {
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = std::fs::File::create(target)?;
    let written = std::io::copy(&mut entry.take(budget + 1), &mut file)?;
    if written > budget {
        return Err(archive_error("archive is larger than the extraction limit"));
    }
    Ok(budget - written)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    fn zip_with(entries: &[(&str, &[u8])]) -> tempfile::NamedTempFile {
        let file = tempfile::NamedTempFile::new().unwrap();
        let mut writer = zip::ZipWriter::new(file.reopen().unwrap());
        for (name, bytes) in entries {
            writer.start_file(*name, SimpleFileOptions::default()).unwrap();
            writer.write_all(bytes).unwrap();
        }
        writer.finish().unwrap();
        file
    }

    fn limits() -> ZipLimits {
        ZipLimits { max_entries: 10, max_total_bytes: 1000 }
    }

    #[test]
    fn traversal_and_absolute_names_are_refused() {
        for name in ["../evil", "/etc/evil", "a/../../evil"] {
            let zip = zip_with(&[(name, b"x")]);
            let out = tempfile::tempdir().unwrap();
            assert!(
                matches!(extract_zip(zip.path(), out.path(), &limits()), Err(DataError::Archive(_))),
                "{name}"
            );
        }
    }

    #[test]
    fn too_many_entries_or_bytes_are_refused() {
        let many: Vec<(String, Vec<u8>)> = (0..11).map(|i| (format!("{i}"), vec![b'x'])).collect();
        let refs: Vec<(&str, &[u8])> = many.iter().map(|(n, b)| (n.as_str(), b.as_slice())).collect();
        let out = tempfile::tempdir().unwrap();
        assert!(extract_zip(zip_with(&refs).path(), out.path(), &limits()).is_err());
        let big = vec![b'x'; 1001];
        let out = tempfile::tempdir().unwrap();
        assert!(extract_zip(zip_with(&[("big", &big)]).path(), out.path(), &limits()).is_err());
    }

    #[test]
    fn windows_separators_become_directories() {
        let zip = zip_with(&[("tests\\1.in", b"7\n")]);
        let out = tempfile::tempdir().unwrap();
        extract_zip(zip.path(), out.path(), &limits()).unwrap();
        assert_eq!(std::fs::read(out.path().join("tests/1.in")).unwrap(), b"7\n");
        let evil = zip_with(&[("..\\evil", b"x")]);
        assert!(extract_zip(evil.path(), out.path(), &limits()).is_err());
    }

    #[test]
    fn symlink_entries_are_refused() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let mut writer = zip::ZipWriter::new(file.reopen().unwrap());
        writer.add_symlink("link", "/etc/passwd", SimpleFileOptions::default()).unwrap();
        writer.finish().unwrap();
        let out = tempfile::tempdir().unwrap();
        assert!(matches!(extract_zip(file.path(), out.path(), &limits()), Err(DataError::Archive(_))));
    }
}
