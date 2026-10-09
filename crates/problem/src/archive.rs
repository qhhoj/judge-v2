//! Zip extraction under limits: no traversal, no symlinks, no lying sizes (spec §2).

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

impl ZipLimits {
    /// One archive may fill at most half the cache (preflight M15).
    pub fn for_cache(cache_max_bytes: u64) -> Self {
        let default = ZipLimits::default();
        ZipLimits { max_total_bytes: default.max_total_bytes.min(cache_max_bytes / 2), ..default }
    }
}

fn archive_error(message: impl std::fmt::Display) -> DataError {
    DataError::Archive(message.to_string())
}

/// Zips made on Windows store `tests\1.in`; `\` is a separator, never part of a name. The segments are
/// checked after the conversion.
fn safe_relative(name: &str) -> Result<PathBuf, DataError> {
    let name = name.replace('\\', "/");
    let path = Path::new(&name);
    let normal = path.components().all(|c| matches!(c, Component::Normal(_)));
    if name.is_empty() || !normal {
        return Err(archive_error(format!("unsafe entry name {name:?}")));
    }
    Ok(path.to_path_buf())
}

const S_IFMT: u32 = 0o170000;
const S_IFLNK: u32 = 0o120000;

pub fn extract_zip(zip: &Path, into: &Path, limits: &ZipLimits) -> Result<(), DataError> {
    let mut archive = zip::ZipArchive::new(crate::data_version::regular_file(zip)?).map_err(archive_error)?;
    if archive.len() > limits.max_entries {
        return Err(archive_error(format!("more than {} entries", limits.max_entries)));
    }
    let mut budget = limits.max_total_bytes;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(archive_error)?;
        if entry.unix_mode().is_some_and(|mode| mode & S_IFMT == S_IFLNK) {
            return Err(archive_error(format!("symlink entry {:?}", entry.name())));
        }
        let relative = safe_relative(entry.name())?;
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
    fn extracts_flat_and_nested_entries() {
        let zip = zip_with(&[("1.in", b"1 2\n"), ("sub/1.out", b"3\n")]);
        let out = tempfile::tempdir().unwrap();
        extract_zip(zip.path(), out.path(), &limits()).unwrap();
        assert_eq!(std::fs::read(out.path().join("1.in")).unwrap(), b"1 2\n");
        assert_eq!(std::fs::read(out.path().join("sub/1.out")).unwrap(), b"3\n");
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
