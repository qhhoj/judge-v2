//! Submissions that are outputs, not programs (spec §5 TEXT and OUTPUT).

use crate::error::GradeError;
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// An OUTPUT zip with more entries than this is refused (IE), preflight M10.
pub const MAX_OUTPUT_ENTRIES: usize = 10_000;

/// The zip, opened once per submission, with its file entries indexed: by full path (`sub/1.out`) and by
/// last segment (`1.out`, the first entry of that name). Entries with an absolute or `..` path are not
/// indexed. Nothing is extracted, so names are only map keys. `relayout` is how the zip's folders sit
/// against the case names (`ZipIndex::align`), `None` when the names are used as they are.
pub struct ZipIndex {
    archive: Mutex<zip::ZipArchive<File>>,
    pub by_path: HashMap<String, usize>,
    by_last: HashMap<String, usize>,
    relayout: Option<Relayout>,
}

/// How the zip's folders sit against the case names, for the whole submission: drop `drop` leading
/// segments of a case name, then put `root` in front (`B/Test01/B.out` → `Test01/B.out` with drop 1,
/// → `out/Test01/B.out` with root `out` as well).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct Relayout {
    drop: usize,
    root: String,
}

impl Relayout {
    fn apply(&self, name: &str) -> Option<String> {
        let segments: Vec<&str> = name.split('/').collect();
        let kept = segments.get(self.drop..).filter(|kept| !kept.is_empty())?.join("/");
        Some(if self.root.is_empty() { kept } else { format!("{}/{kept}", self.root) })
    }
}

pub enum OutputSource {
    Text(Vec<u8>),
    Zip(ZipIndex),
}

fn zip_error(error: impl std::fmt::Display) -> GradeError {
    GradeError::Io(error.to_string())
}

/// `./a//b` → `a/b`; None for an empty, absolute or `..` path.
fn clean_path(name: &str) -> Option<String> {
    let parts: Vec<&str> = name.split('/').filter(|p| !p.is_empty() && *p != ".").collect();
    let escapes = name.starts_with('/') || parts.contains(&"..") || name.contains('\\');
    (!parts.is_empty() && !escapes).then(|| parts.join("/"))
}

fn last_segment(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Roots tried for a relayout: none, and the first folders of the entries (a few, `__MACOSX` aside).
const MAX_RELAYOUT_ROOTS: usize = 8;

impl OutputSource {
    /// The OUTPUT zip at `path`, lined up with the problem's case `out` names (`ZipIndex::align`).
    pub fn zip(path: PathBuf, case_names: &[&str]) -> Result<OutputSource, GradeError> {
        let mut archive = zip::ZipArchive::new(File::open(&path)?).map_err(zip_error)?;
        if archive.len() > MAX_OUTPUT_ENTRIES {
            return Err(GradeError::Io(format!("output zip has more than {MAX_OUTPUT_ENTRIES} entries")));
        }
        let (mut by_path, mut by_last) = (HashMap::new(), HashMap::new());
        for index in 0..archive.len() {
            let entry = archive.by_index_raw(index).map_err(zip_error)?;
            let Some(clean) = clean_path(entry.name()).filter(|_| entry.is_file()) else { continue };
            by_last.entry(last_segment(&clean).to_string()).or_insert(index);
            by_path.entry(clean).or_insert(index);
        }
        let mut zip = ZipIndex { archive: Mutex::new(archive), by_path, by_last, relayout: None };
        zip.align(case_names);
        Ok(OutputSource::Zip(zip))
    }
}

impl ZipIndex {
    /// The entry for a case's `out` name: the same relative path, or under the zip's relayout (when there is
    /// one) that path moved instead; and for a flat name (`1.out`) the first entry of that name anywhere in
    /// the zip. A nested name never falls back to another directory's file.
    fn find(&self, wanted: &str) -> Option<usize> {
        let clean = clean_path(wanted)?;
        let found = match &self.relayout {
            Some(relayout) => relayout.apply(&clean).and_then(|name| self.by_path.get(&name)),
            None => self.by_path.get(&clean),
        };
        // `by_last` is keyed by bare names, so a nested `clean` can only match through `by_path`.
        found.or_else(|| self.by_last.get(&clean)).copied()
    }

    /// Lines the zip up with the case names once, for every case: outer folders the contestant dropped,
    /// added or renamed. The relayout taken is the one that finds the most (distinct) case names, more than
    /// the names as they are and more than any other relayout; one that would give two case names the same
    /// entry is never taken, so no case reads another's output.
    fn align(&mut self, case_names: &[&str]) {
        let names: Vec<String> = case_names
            .iter()
            .filter_map(|name| clean_path(name))
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        let Some(depth) = names.iter().map(|name| name.split('/').count()).min() else { return };
        let (mut best, mut tied) = (self.hits(&names, &Relayout::default()).unwrap_or(0), false);
        for relayout in self.relayouts(depth) {
            match self.hits(&names, &relayout) {
                Some(hits) if hits > best => (best, tied, self.relayout) = (hits, false, Some(relayout)),
                Some(hits) if hits == best && self.relayout.is_some() => tied = true,
                _ => {}
            }
        }
        if tied {
            self.relayout = None;
        }
    }

    /// Every relayout but the identity: drop 0..depth leading segments, with no root or one of the roots.
    fn relayouts(&self, depth: usize) -> Vec<Relayout> {
        let mut roots = vec![String::new()];
        roots.extend(self.roots());
        let all = roots
            .into_iter()
            .flat_map(|root| (0..depth).map(move |drop| Relayout { drop, root: root.clone() }));
        all.filter(|relayout| *relayout != Relayout::default()).collect()
    }

    /// How many case names the relayout finds; None when two of them land on the same name.
    fn hits(&self, names: &[String], relayout: &Relayout) -> Option<usize> {
        let moved: Vec<String> = names.iter().map(|name| relayout.apply(name)).collect::<Option<_>>()?;
        let distinct: HashSet<&String> = moved.iter().collect();
        (distinct.len() == moved.len())
            .then(|| moved.iter().filter(|name| self.by_path.contains_key(*name)).count())
    }

    /// The first folders of the entries (`mine` for `mine/a/1.out`), at most `MAX_RELAYOUT_ROOTS`.
    fn roots(&self) -> Vec<String> {
        let mut roots: Vec<&str> =
            self.by_path.keys().filter_map(|path| path.split_once('/')).map(|(first, _)| first).collect();
        roots.sort_unstable();
        roots.dedup();
        roots
            .into_iter()
            .filter(|root| *root != "__MACOSX")
            .take(MAX_RELAYOUT_ROOTS)
            .map(String::from)
            .collect()
    }
}

fn unreadable(error: impl std::fmt::Display) -> GradeError {
    GradeError::UnreadableSubmission(error.to_string())
}

/// At most `limit` decompressed bytes of the entry; no entry for the name is an empty output. Any failure to
/// open or inflate the entry (unsupported method, encryption, bad data or checksum) is `UnreadableSubmission`:
/// the zip was written to scratch whole before grading, so these come from its bytes, not from the host.
fn zip_entry(zip: &ZipIndex, wanted: &str, limit: u64) -> Result<Vec<u8>, GradeError> {
    let Some(index) = zip.find(wanted) else { return Ok(Vec::new()) };
    let mut archive = zip.archive.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let entry = archive.by_index(index).map_err(unreadable)?;
    let mut bytes = Vec::new();
    entry.take(limit).read_to_end(&mut bytes).map_err(unreadable)?;
    Ok(bytes)
}

/// Writes the submission's answer for one case to `dest` and returns its length. At most `cap + 1` bytes are
/// written, so a length above `cap` means the answer is too long (OLE) while exactly `cap` is allowed.
/// `case_output_name` is the case's `out` file name (`1.out`, maybe `sub/1.out`); TEXT answers every case
/// with its source. A failed write to `dest` (the host's scratch) is `GradeError::Io`.
pub fn output_for_case(
    source: &OutputSource,
    case_output_name: &str,
    dest: &Path,
    cap: u64,
) -> Result<u64, GradeError> {
    let limit = cap.saturating_add(1);
    let bytes = match source {
        OutputSource::Text(text) => {
            text[..text.len().min(usize::try_from(limit).unwrap_or(usize::MAX))].to_vec()
        }
        OutputSource::Zip(zip) => zip_entry(zip, case_output_name, limit)?,
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
        assert_eq!(output_for_case(&text, "1.out", &dest, 2).unwrap(), 3, "cap + 1 bytes: over the cap");
        assert_eq!(output_for_case(&text, "1.out", &dest, 3).unwrap(), 3, "exactly the cap is fine");
    }

    #[test]
    fn zip_entries_match_by_name_and_missing_is_empty() {
        let zip = zip_of(&[("1.out", b"a"), ("dir/2.out", b"bb")]);
        let source = OutputSource::zip(zip.path().into(), &[]).unwrap();
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
        let source = OutputSource::zip(zip.path().into(), &[]).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("o");
        output_for_case(&source, "1.out", &dest, 100).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"first");
        assert_eq!(output_for_case(&source, "sub", &dest, 100).unwrap(), 0);
    }

    #[test]
    fn nested_case_names_match_their_own_path_only() {
        let zip = zip_of(&[("a/1.out", b"A"), ("b/1.out", b"B"), ("flat.out", b"F"), ("../evil.out", b"E")]);
        let source = OutputSource::zip(zip.path().into(), &[]).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("o");
        let read = |name: &str| {
            output_for_case(&source, name, &dest, 100).unwrap();
            std::fs::read(&dest).unwrap()
        };
        assert_eq!(read("a/1.out"), b"A");
        assert_eq!(read("b/1.out"), b"B");
        assert_eq!(read("c/1.out"), b"", "no fallback into another directory");
        assert_eq!(read("./flat.out"), b"F");
        assert_eq!(read("1.out"), b"A", "a flat name takes the first entry of that name");
        assert_eq!(read("evil.out"), b"", "a traversal entry is never indexed");
        assert_eq!(read("../evil.out"), b"");
    }

    fn aligned_read(entries: &[(&str, &[u8])], cases: &[&str]) -> Vec<Vec<u8>> {
        let zip = zip_of(entries);
        let source = OutputSource::zip(zip.path().into(), cases).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("o");
        let read = |name: &&str| {
            output_for_case(&source, name, &dest, 100).unwrap();
            std::fs::read(&dest).unwrap()
        };
        cases.iter().map(read).collect()
    }

    #[test]
    fn a_zip_with_outer_folders_dropped_added_or_renamed_is_lined_up_with_the_cases() {
        let cases = ["B/Test00/B.out", "B/Test01/B.out"];
        let dropped: &[(&str, &[u8])] = &[("Test00/B.out", b"0"), ("Test01/B.out", b"1")];
        assert_eq!(aligned_read(dropped, &cases), [b"0", b"1"]);
        let added: &[(&str, &[u8])] = &[("mine/B/Test00/B.out", b"0"), ("mine/B/Test01/B.out", b"1")];
        assert_eq!(aligned_read(added, &cases), [b"0", b"1"]);
        let renamed: &[(&str, &[u8])] = &[("out/Test00/B.out", b"0"), ("out/Test01/B.out", b"1")];
        assert_eq!(aligned_read(renamed, &cases), [b"0", b"1"]);
        let flat: &[(&str, &[u8])] = &[("01.a", b"x"), ("02.a", b"y")];
        assert_eq!(aligned_read(flat, &["tests/01.a", "tests/02.a"]), [b"x", b"y"]);
    }

    #[test]
    fn a_relayout_never_gives_a_case_another_case_s_output() {
        let one_file: &[(&str, &[u8])] = &[("B.out", b"only")];
        let empty: [&[u8]; 2] = [b"", b""];
        assert_eq!(
            aligned_read(one_file, &["B/Test00/B.out", "B/Test01/B.out"]),
            empty,
            "two cases, one file"
        );
        let part: &[(&str, &[u8])] = &[("sub1/1/out", b"11"), ("sub1/2/out", b"12")];
        let got = aligned_read(part, &["sub1/1/out", "sub1/2/out", "sub2/1/out"]);
        assert_eq!(got, [&b"11"[..], b"12", b""], "a missing subtask stays empty");
        let lone: &[(&str, &[u8])] = &[("a/1.out", b"A")];
        assert_eq!(aligned_read(lone, &["a/1.out", "c/1.out"]), [&b"A"[..], b""]);
        let mixed: &[(&str, &[u8])] = &[("a/1.out", b"A"), ("1.out", b"B")];
        assert_eq!(
            aligned_read(mixed, &["x/a/1.out", "a/1.out"]),
            [b"A", b"B"],
            "one relayout for all, one entry each"
        );
    }

    #[test]
    fn a_pretest_reusing_a_test_file_and_stray_folders_do_not_stop_the_relayout() {
        let cases = ["B/Test00/B.out", "B/Test01/B.out", "B/Test00/B.out"];
        let dropped: &[(&str, &[u8])] = &[("Test00/B.out", b"0"), ("Test01/B.out", b"1")];
        assert_eq!(aligned_read(dropped, &cases), [b"0", b"1", b"0"]);
        let finder: &[(&str, &[u8])] =
            &[("mine/B/Test00/B.out", b"0"), ("mine/B/Test01/B.out", b"1"), ("__MACOSX/mine/._B.out", b"m")];
        assert_eq!(aligned_read(finder, &cases[..2]), [b"0", b"1"]);
        let readme: &[(&str, &[u8])] =
            &[("README.txt", b"r"), ("out/Test00/B.out", b"0"), ("out/Test01/B.out", b"1")];
        assert_eq!(aligned_read(readme, &cases[..2]), [b"0", b"1"]);
    }

    /// One stored (uncompressed) entry `1.out`, so its bytes can be patched in place.
    fn stored_zip(data: &[u8]) -> Vec<u8> {
        let mut bytes = Vec::new();
        let mut w = zip::ZipWriter::new(std::io::Cursor::new(&mut bytes));
        let options =
            zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
        w.start_file("1.out", options).unwrap();
        w.write_all(data).unwrap();
        w.finish().unwrap();
        bytes
    }

    fn answer_of(bytes: &[u8], dest: &Path) -> Result<u64, GradeError> {
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), bytes).unwrap();
        output_for_case(&OutputSource::zip(file.path().into(), &[]).unwrap(), "1.out", dest, 100)
    }

    fn position(haystack: &[u8], needle: &[u8]) -> usize {
        haystack.windows(needle.len()).position(|w| w == needle).unwrap()
    }

    #[test]
    fn entries_the_zip_cannot_inflate_are_the_submission_s_fault() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("o");
        let good = stored_zip(b"hello world\n");
        assert_eq!(answer_of(&good, &dest).unwrap(), 12);
        let mut bad_checksum = good.clone();
        let at = position(&bad_checksum, b"hello");
        bad_checksum[at] = b'j';
        assert!(matches!(answer_of(&bad_checksum, &dest), Err(GradeError::UnreadableSubmission(_))));
        let mut bzip2 = good.clone();
        let central = position(&bzip2, b"PK\x01\x02");
        for at in [8, central + 10] {
            bzip2[at..at + 2].copy_from_slice(&12u16.to_le_bytes()); // compression method 12: bzip2, not built in
        }
        assert!(matches!(answer_of(&bzip2, &dest), Err(GradeError::UnreadableSubmission(_))));
        let mut encrypted = good;
        let central = position(&encrypted, b"PK\x01\x02");
        for at in [6, central + 8] {
            encrypted[at] |= 1; // general purpose flag bit 0: encrypted
        }
        assert!(matches!(answer_of(&encrypted, &dest), Err(GradeError::UnreadableSubmission(_))));
    }

    #[test]
    fn a_zip_with_too_many_entries_is_refused_and_one_index_serves_every_case() {
        let names: Vec<String> = (0..=MAX_OUTPUT_ENTRIES).map(|i| format!("{i}.out")).collect();
        let entries: Vec<(&str, &[u8])> = names.iter().map(|n| (n.as_str(), &b"x"[..])).collect();
        assert!(matches!(OutputSource::zip(zip_of(&entries).path().into(), &[]), Err(GradeError::Io(_))));
        let ok = zip_of(&entries[..MAX_OUTPUT_ENTRIES]);
        let source = OutputSource::zip(ok.path().into(), &[]).unwrap();
        let OutputSource::Zip(index) = &source else { panic!("zip expected") };
        assert_eq!(index.by_path.len(), MAX_OUTPUT_ENTRIES);
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(output_for_case(&source, "9999.out", &dir.path().join("o"), 10).unwrap(), 1);
    }
}
