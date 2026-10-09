//! Finds a problem's init.yml and case files for one assignment (spec §2 "Caching and versioning").

use crate::archive::{ZipLimits, extract_zip};
use crate::code::problem_dir;
use crate::data_version::{
    DataError, StampSource, WaitPolicy, read_regular_to_string, regular_file, wait_for_stamp,
};
use crate::dir_cache::{CacheLease, DirCache};
use crate::init::{InitYaml, parse_init};
use std::fs::File;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Debug)]
pub struct ResolvedProblem {
    pub init: InitYaml,
    pub data_dir: PathBuf,
    pub files_dir: PathBuf,
    pub data_version_used: u64,
    pub lease: Option<CacheLease>,
}

/// A name inside a problem directory: `/`-separated segments, each non-empty, not starting with `.`
/// (so never `..` or a dotfile), no `\\` (the `archive.rs` rule; legacy zips store `tests/1.in`, preflight I17).
fn is_relative_name(name: &str) -> bool {
    !name.is_empty()
        && !name.contains('\\')
        && name.split('/').all(|seg| !seg.is_empty() && !seg.starts_with('.'))
}

/// Opens `base/name` once and judges the opened handle (preflight I11): the last segment is opened with
/// `O_NOFOLLOW`, the handle must be a regular file, and the kernel's own path for the handle must be exactly
/// `base/name`, so no directory on the way was a symlink. Nothing is checked by path and then reopened.
fn open_beneath(base: &Path, name: &str) -> Result<File, DataError> {
    if !is_relative_name(name) {
        return Err(DataError::Io(format!("bad file name {name:?}")));
    }
    let expected = base.canonicalize()?.join(name);
    let file = regular_file(&expected)?;
    let opened = std::fs::read_link(format!("/proc/self/fd/{}", file.as_raw_fd()))?;
    if opened != expected {
        return Err(DataError::Io(format!("{} resolves through a symlink", expected.display())));
    }
    Ok(file)
}

impl ResolvedProblem {
    /// A case file named by init.yml (relative, maybe in a subdirectory), never a dotfile or a symlink.
    /// The caller reads or copies from the returned handle, not from a path.
    pub fn open_case_file(&self, name: &str) -> Result<File, DataError> {
        open_beneath(&self.files_dir, name)
    }

    /// A file stored next to init.yml (checker, interactor and grader sources), same rules.
    pub fn open_data_file(&self, name: &str) -> Result<File, DataError> {
        open_beneath(&self.data_dir, name)
    }
}

pub struct Resolver {
    pub root: PathBuf,
    pub cache: Arc<DirCache>,
    pub wait: WaitPolicy,
    pub zip: ZipLimits,
    pub stamps: Arc<dyn StampSource>,
}

impl Resolver {
    /// Two passes: init.yml and the extracted archive are accepted only if the stamp is the same before
    /// and after (and no upload is running). A second disagreement is `Unavailable`.
    pub fn resolve(&self, code: &str, assigned: u64) -> Result<ResolvedProblem, DataError> {
        let dir = problem_dir(&self.root, code)?;
        if !std::fs::symlink_metadata(&dir)?.file_type().is_dir() {
            return Err(DataError::Io(format!("{} is not a real directory", dir.display())));
        }
        for _ in 0..2 {
            if let Some(problem) = self.attempt(code, &dir, assigned)? {
                return Ok(problem);
            }
        }
        Err(DataError::Unavailable { assigned, found: self.stamps.read(&dir)? })
    }

    /// None when the stamp moved while init.yml was read or the archive extracted; the work is discarded.
    fn attempt(&self, code: &str, dir: &Path, assigned: u64) -> Result<Option<ResolvedProblem>, DataError> {
        let version = wait_for_stamp(dir, assigned, &self.wait, self.stamps.as_ref())?;
        let init = parse_init(&read_regular_to_string(&dir.join("init.yml"))?)?;
        let problem = self.finish(code, dir, (version, init))?;
        // Marker, then stamp: an upload that began since the first read is seen by its marker, and one that
        // ends between the two checks has already written its new stamp (the uploader removes the marker last).
        let unchanged =
            !self.stamps.upload_in_progress(dir) && self.stamps.read(dir)?.unwrap_or(0) == version;
        if unchanged {
            return Ok(Some(problem));
        }
        if let Some(lease) = problem.lease {
            let key = lease.key().to_string();
            drop(lease);
            self.cache.invalidate(&key);
        }
        Ok(None)
    }

    fn finish(
        &self,
        code: &str,
        dir: &Path,
        (version, init): (u64, InitYaml),
    ) -> Result<ResolvedProblem, DataError> {
        let data_dir = dir.to_path_buf();
        let Some(archive) = init.archive.clone() else {
            let files_dir = data_dir.clone();
            return Ok(ResolvedProblem {
                init,
                files_dir,
                data_dir,
                data_version_used: version,
                lease: None,
            });
        };
        if !is_relative_name(&archive) || archive.contains('/') {
            return Err(DataError::Archive(format!("bad archive name {archive:?}")));
        }
        let zip = dir.join(&archive);
        let key = archive_key(code, version, &zip)?;
        let lease = self.cache.get_or_build(&key, |into| extract_zip(&zip, into, &self.zip))?;
        let files_dir = lease.path().to_path_buf();
        Ok(ResolvedProblem { init, files_dir, data_dir, data_version_used: version, lease: Some(lease) })
    }
}

/// The cache key is `code/<version>-<archive identity>`. The identity (mtime, size, inode, ctime) keeps a
/// deleted-and-recreated code, which may restart at the same `dataVersion`, and an rsync that replaces the
/// archive from being served stale.
fn archive_key(code: &str, version: u64, zip: &Path) -> Result<String, DataError> {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::symlink_metadata(zip)?;
    Ok(format!("{code}/{version}-{}-{}-{}-{}", meta.mtime(), meta.size(), meta.ino(), meta.ctime()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data_version::{FsStamps, WRITING_MARKER};
    use std::io::Read;
    use std::time::Duration;

    fn file_name(path: &Path) -> String {
        path.file_name().unwrap().to_string_lossy().into_owned()
    }

    fn entries_of(cache: &Path, code: &str, prefix: &str) -> Vec<String> {
        let found = std::fs::read_dir(cache.join(code)).into_iter().flatten().flatten();
        found
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with(prefix))
            .collect()
    }

    fn read_all(mut file: File) -> Vec<u8> {
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).unwrap();
        bytes
    }

    fn resolver(root: &Path, cache: &Path) -> Resolver {
        Resolver {
            root: root.into(),
            cache: Arc::new(DirCache::new(cache.into(), 1 << 30)),
            wait: WaitPolicy { timeout: Duration::from_millis(200), poll: Duration::from_millis(20) },
            zip: ZipLimits::default(),
            stamps: Arc::new(FsStamps),
        }
    }

    fn write_problem(root: &Path, stamp: Option<&str>) {
        let dir = root.join("sum");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("init.yml"), "test_cases:\n- {in: 1.in, out: 1.out, points: 1}\n").unwrap();
        std::fs::write(dir.join("1.in"), "1 2\n").unwrap();
        if let Some(stamp) = stamp {
            std::fs::write(dir.join(".data-version"), stamp).unwrap();
        }
    }

    #[test]
    fn loose_files_resolve_in_the_data_dir_with_the_stamp_version() {
        let (root, cache) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        write_problem(root.path(), Some("3\n"));
        let problem = resolver(root.path(), cache.path()).resolve("sum", 3).unwrap();
        assert_eq!(problem.data_version_used, 3);
        assert_eq!(read_all(problem.open_case_file("1.in").unwrap()), b"1 2\n");
    }

    #[test]
    fn missing_stamp_grades_from_disk_as_version_zero() {
        let (root, cache) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        write_problem(root.path(), None);
        assert_eq!(resolver(root.path(), cache.path()).resolve("sum", 9).unwrap().data_version_used, 0);
    }

    #[test]
    fn lagging_stamp_is_unavailable() {
        let (root, cache) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        write_problem(root.path(), Some("1"));
        let error = resolver(root.path(), cache.path()).resolve("sum", 2).unwrap_err();
        assert!(matches!(error, DataError::Unavailable { .. }));
    }

    #[test]
    fn case_file_refuses_dotfiles_traversal_and_missing_files() {
        let (root, cache) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        write_problem(root.path(), None);
        let problem = resolver(root.path(), cache.path()).resolve("sum", 0).unwrap();
        for name in [
            ".data-version",
            "../x",
            "a/../1.in",
            "/etc/hostname",
            "a//b",
            "sub/.x",
            "a\\b",
            "",
            "missing.in",
        ] {
            assert!(problem.open_case_file(name).is_err(), "{name}");
        }
    }

    #[test]
    fn case_file_accepts_names_in_subdirectories() {
        let (root, cache) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        write_problem(root.path(), None);
        std::fs::create_dir_all(root.path().join("sum/sub")).unwrap();
        std::fs::write(root.path().join("sum/sub/1.in"), "4\n").unwrap();
        let problem = resolver(root.path(), cache.path()).resolve("sum", 0).unwrap();
        assert_eq!(read_all(problem.open_case_file("sub/1.in").unwrap()), b"4\n");
    }

    #[test]
    fn symlinked_case_files_and_directories_are_io_errors() {
        let (root, cache) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        write_problem(root.path(), None);
        let dir = root.path().join("sum");
        std::fs::remove_file(dir.join("1.in")).unwrap();
        std::os::unix::fs::symlink("/etc/hostname", dir.join("1.in")).unwrap();
        std::os::unix::fs::symlink("/etc", dir.join("sub")).unwrap();
        let problem = resolver(root.path(), cache.path()).resolve("sum", 0).unwrap();
        assert!(matches!(problem.open_case_file("1.in"), Err(DataError::Io(_))));
        assert!(matches!(problem.open_case_file("sub/hostname"), Err(DataError::Io(_))));
        assert!(matches!(problem.open_data_file("1.in"), Err(DataError::Io(_))));
    }

    #[test]
    fn symlinked_code_dirs_and_init_yml_are_io_errors() {
        let (root, cache) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        write_problem(root.path(), None);
        let dir = root.path().join("sum");
        let r = resolver(root.path(), cache.path());
        std::os::unix::fs::symlink(&dir, root.path().join("linked")).unwrap();
        assert!(matches!(r.resolve("linked", 0), Err(DataError::Io(_))));
        std::fs::remove_file(dir.join("init.yml")).unwrap();
        std::os::unix::fs::symlink(cache.path(), dir.join("init.yml")).unwrap();
        assert!(matches!(r.resolve("sum", 0), Err(DataError::Io(_))));
    }

    #[test]
    fn archive_names_that_leave_the_problem_dir_are_refused() {
        for name in ["../x.zip", "/etc/x.zip", ".hidden.zip", "sub/x.zip"] {
            let (root, cache) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
            let dir = root.path().join("bad");
            std::fs::create_dir_all(&dir).unwrap();
            let init = format!("archive: \"{name}\"\ntest_cases:\n- {{in: 1.in, out: 1.out, points: 1}}\n");
            std::fs::write(dir.join("init.yml"), init).unwrap();
            let error = resolver(root.path(), cache.path()).resolve("bad", 0).err();
            assert!(matches!(error, Some(DataError::Archive(_))), "{name}");
        }
    }

    #[test]
    fn zip_limits_follow_the_cache_size() {
        assert_eq!(ZipLimits::for_cache(20 << 30).max_total_bytes, 8 << 30);
        assert_eq!(ZipLimits::for_cache(4 << 30).max_total_bytes, 2 << 30);
    }

    #[test]
    fn archive_problems_extract_into_the_cache_keyed_by_version() {
        let (root, cache) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let dir = root.path().join("zipped");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("init.yml"),
            "archive: t.zip\ntest_cases:\n- {in: 1.in, out: 1.out, points: 1}\n",
        )
        .unwrap();
        let mut writer = zip::ZipWriter::new(std::fs::File::create(dir.join("t.zip")).unwrap());
        writer.start_file("1.in", zip::write::SimpleFileOptions::default()).unwrap();
        std::io::Write::write_all(&mut writer, b"5\n").unwrap();
        writer.finish().unwrap();
        std::fs::write(dir.join(".data-version"), "4").unwrap();
        let problem = resolver(root.path(), cache.path()).resolve("zipped", 4).unwrap();
        assert!(problem.files_dir.starts_with(cache.path()));
        assert!(file_name(&problem.files_dir).starts_with("4-"));
        assert_eq!(read_all(problem.open_case_file("1.in").unwrap()), b"5\n");
    }

    #[test]
    fn a_recreated_code_with_the_same_data_version_is_not_served_from_the_old_cache_entry() {
        let (root, cache) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let dir = root.path().join("zipped");
        std::fs::create_dir_all(&dir).unwrap();
        let init = "archive: t.zip\ntest_cases:\n- {in: 1.in, out: 1.out, points: 1}\n";
        std::fs::write(dir.join("init.yml"), init).unwrap();
        write_zip(&dir.join("t.zip"), b"first problem\n");
        set_stamp(&dir, 1);
        let r = resolver(root.path(), cache.path());
        let first = r.resolve("zipped", 1).unwrap();
        assert_eq!(read_all(first.open_case_file("1.in").unwrap()), b"first problem\n");
        drop(first);
        std::fs::remove_dir_all(&dir).unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("init.yml"), init).unwrap();
        write_zip(&dir.join("t.zip"), b"second problem, longer\n");
        set_stamp(&dir, 1);
        let second = r.resolve("zipped", 1).unwrap();
        assert_eq!(read_all(second.open_case_file("1.in").unwrap()), b"second problem, longer\n");
    }

    #[test]
    fn missing_init_yml_is_an_io_error_and_bad_code_is_refused() {
        let (root, cache) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let r = resolver(root.path(), cache.path());
        assert!(matches!(r.resolve("nothing", 0), Err(DataError::Io(_))));
        assert!(matches!(r.resolve("../x", 0), Err(DataError::BadCode(_))));
    }

    /// Real stamps, plus a hook that runs before the n-th `read` (1-based) and may rewrite files on disk.
    struct ScriptedStamps {
        reads: std::sync::atomic::AtomicUsize,
        before_read: Box<dyn Fn(usize) + Send + Sync>,
    }

    impl ScriptedStamps {
        fn new(before_read: impl Fn(usize) + Send + Sync + 'static) -> Arc<Self> {
            Arc::new(ScriptedStamps { reads: Default::default(), before_read: Box::new(before_read) })
        }

        fn reads(&self) -> usize {
            self.reads.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    impl StampSource for ScriptedStamps {
        fn read(&self, dir: &Path) -> Result<Option<u64>, DataError> {
            let n = self.reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
            (self.before_read)(n);
            FsStamps.read(dir)
        }

        fn upload_in_progress(&self, dir: &Path) -> bool {
            FsStamps.upload_in_progress(dir)
        }
    }

    fn init_with_cases(count: usize) -> String {
        let cases: String =
            (0..count).map(|i| format!("- {{in: {i}.in, out: {i}.out, points: 1}}\n")).collect();
        format!("test_cases:\n{cases}")
    }

    fn set_stamp(dir: &Path, version: u64) {
        std::fs::write(dir.join(".tmp-stamp"), version.to_string()).unwrap();
        std::fs::rename(dir.join(".tmp-stamp"), dir.join(".data-version")).unwrap();
    }

    fn scripted_problem(stamps: Arc<ScriptedStamps>, root: &Path, cache: &Path) -> Resolver {
        Resolver { stamps, ..resolver(root, cache) }
    }

    /// Real stamps; `hook(step, after)` runs before and after every question (stamp read or marker check),
    /// counted together from 1, so a test can place an uploader's steps between any two of them.
    struct Stepped {
        steps: std::sync::atomic::AtomicUsize,
        hook: Box<dyn Fn(usize, bool) + Send + Sync>,
    }

    impl Stepped {
        fn ask<T>(&self, answer: impl FnOnce() -> T) -> T {
            let step = self.steps.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
            (self.hook)(step, false);
            let value = answer();
            (self.hook)(step, true);
            value
        }
    }

    impl StampSource for Stepped {
        fn read(&self, dir: &Path) -> Result<Option<u64>, DataError> {
            self.ask(|| FsStamps.read(dir))
        }

        fn upload_in_progress(&self, dir: &Path) -> bool {
            self.ask(|| FsStamps.upload_in_progress(dir))
        }
    }

    #[test]
    fn an_upload_that_ends_during_the_second_check_is_not_accepted_as_the_old_version() {
        let (root, cache) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let dir = root.path().join("sum");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("init.yml"), init_with_cases(1)).unwrap();
        set_stamp(&dir, 4);
        let hook_dir = dir.clone();
        // Steps 1-2 are the first wait (stamp, marker). The upload starts right after them, so init.yml is read
        // half-new, and it ends (stamp 5, then marker removed) right after step 3, the first second-check question.
        let hook = move |step: usize, after: bool| match (step, after) {
            (2, true) => {
                std::fs::write(hook_dir.join(WRITING_MARKER), "").unwrap();
                std::fs::write(hook_dir.join("init.yml"), init_with_cases(2)).unwrap();
            }
            (3, true) => {
                set_stamp(&hook_dir, 5);
                std::fs::remove_file(hook_dir.join(WRITING_MARKER)).unwrap();
            }
            _ => {}
        };
        let stamps = Arc::new(Stepped { steps: Default::default(), hook: Box::new(hook) });
        let resolver = Resolver { stamps, ..resolver(root.path(), cache.path()) };
        let problem = resolver.resolve("sum", 4).unwrap();
        assert_eq!(
            (problem.data_version_used, problem.init.test_cases.len()),
            (5, 2),
            "never 2 cases as version 4"
        );
    }

    #[test]
    fn a_stamp_that_moves_during_the_init_read_is_reread() {
        let (root, cache) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let dir = root.path().join("sum");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("init.yml"), init_with_cases(1)).unwrap();
        set_stamp(&dir, 4);
        let hook_dir = dir.clone();
        let stamps = ScriptedStamps::new(move |n| {
            if n == 2 {
                std::fs::write(hook_dir.join("init.yml"), init_with_cases(2)).unwrap();
                set_stamp(&hook_dir, 5);
            }
        });
        let problem = scripted_problem(stamps.clone(), root.path(), cache.path()).resolve("sum", 4).unwrap();
        assert_eq!((problem.data_version_used, problem.init.test_cases.len()), (5, 2));
        assert_eq!(stamps.reads(), 4);
    }

    #[test]
    fn a_stamp_that_keeps_moving_is_unavailable() {
        let (root, cache) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        write_problem(root.path(), Some("4"));
        let dir = root.path().join("sum");
        let stamps = ScriptedStamps::new(move |n| {
            if n == 2 || n == 4 {
                set_stamp(&dir, 3 + n as u64 / 2 + 1);
            }
        });
        let error = scripted_problem(stamps, root.path(), cache.path()).resolve("sum", 4).unwrap_err();
        assert_eq!(error, DataError::Unavailable { assigned: 4, found: Some(6) });
    }

    #[test]
    fn a_steady_stamp_is_read_exactly_twice() {
        let (root, cache) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        write_problem(root.path(), Some("4"));
        let stamps = ScriptedStamps::new(|_| {});
        scripted_problem(stamps.clone(), root.path(), cache.path()).resolve("sum", 4).unwrap();
        assert_eq!(stamps.reads(), 2);
    }

    fn write_zip(path: &Path, content: &[u8]) {
        let mut writer = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
        writer.start_file("1.in", zip::write::SimpleFileOptions::default()).unwrap();
        std::io::Write::write_all(&mut writer, content).unwrap();
        writer.finish().unwrap();
    }

    #[test]
    fn an_archive_swapped_during_extraction_is_discarded_and_rebuilt_under_the_new_version() {
        let (root, cache) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let dir = root.path().join("zipped");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("init.yml"),
            "archive: t.zip\ntest_cases:\n- {in: 1.in, out: 1.out, points: 1}\n",
        )
        .unwrap();
        write_zip(&dir.join("t.zip"), b"old\n");
        set_stamp(&dir, 4);
        let hook_dir = dir.clone();
        let stamps = ScriptedStamps::new(move |n| {
            if n == 2 {
                write_zip(&hook_dir.join("t.zip"), b"new\n");
                set_stamp(&hook_dir, 5);
            }
        });
        let problem = scripted_problem(stamps, root.path(), cache.path()).resolve("zipped", 4).unwrap();
        assert_eq!(problem.data_version_used, 5);
        assert!(file_name(&problem.files_dir).starts_with("5-"));
        assert_eq!(read_all(problem.open_case_file("1.in").unwrap()), b"new\n");
        assert!(entries_of(cache.path(), "zipped", "4-").is_empty(), "the half-old entry is gone");
    }

    #[test]
    fn an_upload_marker_makes_the_resolve_wait_then_fail() {
        let (root, cache) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        write_problem(root.path(), Some("4"));
        std::fs::write(root.path().join("sum/.data-version.writing"), "").unwrap();
        let error = resolver(root.path(), cache.path()).resolve("sum", 4).unwrap_err();
        assert!(matches!(error, DataError::Unavailable { .. }));
    }

    #[test]
    fn an_opened_case_file_is_the_one_that_was_checked() {
        let (root, cache) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        write_problem(root.path(), None);
        let problem = resolver(root.path(), cache.path()).resolve("sum", 0).unwrap();
        let handle = problem.open_case_file("1.in").unwrap();
        let path = root.path().join("sum/1.in");
        std::fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink("/etc/hostname", &path).unwrap();
        assert_eq!(read_all(handle), b"1 2\n", "a swap after the open changes nothing");
    }

    #[test]
    fn a_directory_alias_inside_the_problem_is_refused_too() {
        let (root, cache) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        write_problem(root.path(), None);
        let dir = root.path().join("sum");
        std::fs::create_dir_all(dir.join("real")).unwrap();
        std::fs::write(dir.join("real/2.in"), "x").unwrap();
        std::os::unix::fs::symlink(dir.join("real"), dir.join("alias")).unwrap();
        let problem = resolver(root.path(), cache.path()).resolve("sum", 0).unwrap();
        assert!(problem.open_case_file("real/2.in").is_ok());
        assert!(matches!(problem.open_case_file("alias/2.in"), Err(DataError::Io(_))));
    }

    #[test]
    fn a_replaced_unstamped_archive_of_the_same_size_is_not_served_stale() {
        let (root, cache) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let dir = root.path().join("zipped");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("init.yml"),
            "archive: t.zip\ntest_cases:\n- {in: 1.in, out: 1.out, points: 1}\n",
        )
        .unwrap();
        write_zip(&dir.join("t.zip"), b"old\n");
        let r = resolver(root.path(), cache.path());
        assert_eq!(read_all(r.resolve("zipped", 0).unwrap().open_case_file("1.in").unwrap()), b"old\n");
        let replacement = dir.join("t.new");
        write_zip(&replacement, b"new\n");
        let times = std::fs::metadata(dir.join("t.zip")).unwrap().modified().unwrap();
        std::fs::File::options().write(true).open(&replacement).unwrap().set_modified(times).unwrap();
        std::fs::rename(&replacement, dir.join("t.zip")).unwrap();
        assert_eq!(read_all(r.resolve("zipped", 0).unwrap().open_case_file("1.in").unwrap()), b"new\n");
    }

    #[test]
    fn an_upload_that_starts_after_the_first_stamp_read_is_not_accepted() {
        let (root, cache) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        write_problem(root.path(), Some("4"));
        let marker = root.path().join("sum").join(WRITING_MARKER);
        // Steps 1-2 are the first wait (stamp, marker); the upload starts right after and never ends.
        let hook = move |step: usize, after: bool| {
            if (step, after) == (2, true) {
                std::fs::write(&marker, "").unwrap();
            }
        };
        let stamps = Arc::new(Stepped { steps: Default::default(), hook: Box::new(hook) });
        let resolver = Resolver { stamps, ..resolver(root.path(), cache.path()) };
        let error = resolver.resolve("sum", 4).unwrap_err();
        assert!(matches!(error, DataError::Unavailable { .. }), "{error:?}");
    }
}
