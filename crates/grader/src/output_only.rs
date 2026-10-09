//! Submissions that are outputs, not programs (spec §5 TEXT and OUTPUT).

use crate::error::GradeError;
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};

/// An OUTPUT zip with more entries than this is refused (IE), preflight M10.
pub const MAX_OUTPUT_ENTRIES: usize = 10_000;

/// The zip's directory, read once per submission: last path segment → entry index.
pub struct ZipIndex {
    pub path: PathBuf,
    pub by_name: HashMap<String, usize>,
}

pub enum OutputSource {
    Text(Vec<u8>),
    Zip(ZipIndex),
}

fn zip_error(error: impl std::fmt::Display) -> GradeError {
    GradeError::Io(error.to_string())
}

impl OutputSource {
    /// Indexes the zip's file entries by their last path segment; the first entry of a name wins.
    pub fn zip(path: PathBuf) -> Result<OutputSource, GradeError> {
        let mut archive = zip::ZipArchive::new(std::fs::File::open(&path)?).map_err(zip_error)?;
        if archive.len() > MAX_OUTPUT_ENTRIES {
            return Err(GradeError::Io(format!("output zip has more than {MAX_OUTPUT_ENTRIES} entries")));
        }
        let mut by_name = HashMap::new();
        for index in 0..archive.len() {
            let entry = archive.by_index_raw(index).map_err(zip_error)?;
            if entry.is_file() {
                let last = entry.name().rsplit('/').next().unwrap_or("").to_string();
                by_name.entry(last).or_insert(index);
            }
        }
        Ok(OutputSource::Zip(ZipIndex { path, by_name }))
    }
}

/// At most `cap` decompressed bytes of the entry; no entry of that name is an empty output.
fn zip_entry(zip: &ZipIndex, wanted: &str, cap: u64) -> Result<Vec<u8>, GradeError> {
    let Some(&index) = zip.by_name.get(wanted) else { return Ok(Vec::new()) };
    let mut archive = zip::ZipArchive::new(std::fs::File::open(&zip.path)?).map_err(zip_error)?;
    let entry = archive.by_index(index).map_err(zip_error)?;
    let mut bytes = Vec::new();
    entry.take(cap).read_to_end(&mut bytes)?;
    Ok(bytes)
}

/// Writes the submission's answer for one case to `dest` (at most `cap` bytes) and returns its length.
/// `case_output_name` is the case's `out` file name (`1.out`); TEXT answers every case with its source.
pub fn output_for_case(
    source: &OutputSource,
    case_output_name: &str,
    dest: &Path,
    cap: u64,
) -> Result<u64, GradeError> {
    let bytes = match source {
        OutputSource::Text(text) => {
            text[..text.len().min(usize::try_from(cap).unwrap_or(usize::MAX))].to_vec()
        }
        OutputSource::Zip(zip) => zip_entry(zip, case_output_name, cap)?,
    };
    std::fs::write(dest, &bytes)?;
    Ok(bytes.len() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn zip_of(entries: &[(&str, &[u8])]) -> tempfile::NamedTempFile {
        let file = tempfile::NamedTempFile::new().unwrap();
        let mut w = zip::ZipWriter::new(file.reopen().unwrap());
        for (name, bytes) in entries {
            w.start_file(*name, zip::write::SimpleFileOptions::default()).unwrap();
            w.write_all(bytes).unwrap();
        }
        w.finish().unwrap();
        file
    }

    #[test]
    fn text_is_every_case_output() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("o");
        let text = OutputSource::Text(b"42\n".to_vec());
        assert_eq!(output_for_case(&text, "9.out", &dest, 100).unwrap(), 3);
        assert_eq!(std::fs::read(&dest).unwrap(), b"42\n");
        assert_eq!(output_for_case(&text, "1.out", &dest, 2).unwrap(), 2, "capped");
    }

    #[test]
    fn zip_entries_match_by_name_and_missing_is_empty() {
        let zip = zip_of(&[("1.out", b"a"), ("dir/2.out", b"bb")]);
        let source = OutputSource::zip(zip.path().into()).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("o");
        assert_eq!(output_for_case(&source, "1.out", &dest, 100).unwrap(), 1);
        assert_eq!(std::fs::read(&dest).unwrap(), b"a");
        assert_eq!(output_for_case(&source, "2.out", &dest, 100).unwrap(), 2);
        assert_eq!(output_for_case(&source, "3.out", &dest, 100).unwrap(), 0);
        assert_eq!(std::fs::read(&dest).unwrap(), b"", "a missing entry overwrites the previous answer");
    }

    #[test]
    fn the_first_entry_of_a_name_wins_and_directories_do_not_count() {
        let zip = zip_of(&[("a/1.out", b"first"), ("b/1.out", b"second"), ("sub/", b"")]);
        let source = OutputSource::zip(zip.path().into()).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("o");
        output_for_case(&source, "1.out", &dest, 100).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"first");
        assert_eq!(output_for_case(&source, "sub", &dest, 100).unwrap(), 0);
    }

    #[test]
    fn output_is_capped() {
        let zip = zip_of(&[("1.out", &[b'x'; 1000])]);
        let dir = tempfile::tempdir().unwrap();
        let source = OutputSource::zip(zip.path().into()).unwrap();
        assert_eq!(output_for_case(&source, "1.out", &dir.path().join("o"), 10).unwrap(), 10);
    }

    #[test]
    fn a_file_that_is_not_a_zip_is_an_error() {
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), b"not a zip").unwrap();
        assert!(matches!(OutputSource::zip(file.path().into()), Err(GradeError::Io(_))));
    }

    #[test]
    fn a_zip_with_too_many_entries_is_refused_and_one_index_serves_every_case() {
        let names: Vec<String> = (0..=MAX_OUTPUT_ENTRIES).map(|i| format!("{i}.out")).collect();
        let entries: Vec<(&str, &[u8])> = names.iter().map(|n| (n.as_str(), &b"x"[..])).collect();
        assert!(matches!(OutputSource::zip(zip_of(&entries).path().into()), Err(GradeError::Io(_))));
        let ok = zip_of(&entries[..MAX_OUTPUT_ENTRIES]);
        let source = OutputSource::zip(ok.path().into()).unwrap();
        let OutputSource::Zip(index) = &source else { panic!("zip expected") };
        assert_eq!(index.by_name.len(), MAX_OUTPUT_ENTRIES);
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(output_for_case(&source, "9999.out", &dir.path().join("o"), 10).unwrap(), 1);
    }
}
