//! Finds a problem's init.yml and case files for one assignment (spec §2 "Caching and versioning").

use crate::archive::{ZipLimits, extract_zip};
use crate::code::problem_dir;
use crate::data_version::{
    DataError, WaitPolicy, read_regular_to_string, read_stamp, regular_file, wait_for_stamp,
};
use crate::dir_cache::{CacheLease, DirCache};
use crate::init::{InitYaml, parse_init};
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

/// `base/name` when every directory on the way is a real directory and the file is a regular file (preflight I11).
fn regular_under(base: &Path, name: &str) -> Result<PathBuf, DataError> {
    if !is_relative_name(name) {
        return Err(DataError::Io(format!("bad file name {name:?}")));
    }
    let mut path = base.to_path_buf();
    let segments: Vec<&str> = name.split('/').collect();
    for dir in &segments[..segments.len() - 1] {
        path.push(dir);
        if !std::fs::symlink_metadata(&path)?.file_type().is_dir() {
            return Err(DataError::Io(format!("{} is not a directory", path.display())));
        }
    }
    path.push(segments[segments.len() - 1]);
    regular_file(&path)?;
    Ok(path)
}

impl ResolvedProblem {
    /// A case file named by init.yml (relative, maybe in a subdirectory), never a dotfile or a symlink.
    pub fn case_file(&self, name: &str) -> Result<PathBuf, DataError> {
        regular_under(&self.files_dir, name)
    }

    /// A file stored next to init.yml (checker, interactor and grader sources), same rules.
    pub fn data_file(&self, name: &str) -> Result<PathBuf, DataError> {
        regular_under(&self.data_dir, name)
    }
}

pub struct Resolver {
    pub root: PathBuf,
    pub cache: Arc<DirCache>,
    pub wait: WaitPolicy,
    pub zip: ZipLimits,
}

impl Resolver {
    pub fn resolve(&self, code: &str, assigned: u64) -> Result<ResolvedProblem, DataError> {
        let dir = problem_dir(&self.root, code)?;
        if !std::fs::symlink_metadata(&dir)?.file_type().is_dir() {
            return Err(DataError::Io(format!("{} is not a real directory", dir.display())));
        }
        match self.read_consistent(&dir, assigned)? {
            Some(found) => self.finish(code, dir, found),
            None => match self.read_consistent(&dir, assigned)? {
                Some(found) => self.finish(code, dir, found),
                None => Err(DataError::Unavailable { assigned, found: read_stamp(&dir)? }),
            },
        }
    }

    /// init.yml read between two stamp reads; None when the stamp moved meanwhile.
    fn read_consistent(&self, dir: &Path, assigned: u64) -> Result<Option<(u64, InitYaml)>, DataError> {
        let before = wait_for_stamp(dir, assigned, &self.wait)?;
        let init = parse_init(&read_regular_to_string(&dir.join("init.yml"))?)?;
        let after = read_stamp(dir)?.unwrap_or(0);
        Ok((before == after).then_some((before, init)))
    }

    fn finish(
        &self,
        code: &str,
        dir: PathBuf,
        (version, init): (u64, InitYaml),
    ) -> Result<ResolvedProblem, DataError> {
        let Some(archive) = init.archive.clone() else {
            return Ok(ResolvedProblem {
                init,
                files_dir: dir.clone(),
                data_dir: dir,
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
        Ok(ResolvedProblem {
            init,
            files_dir: lease.path().to_path_buf(),
            data_dir: dir,
            data_version_used: version,
            lease: Some(lease),
        })
    }
}

fn archive_key(code: &str, version: u64, zip: &Path) -> Result<String, DataError> {
    if version > 0 {
        return Ok(format!("{code}/{version}"));
    }
    let meta = std::fs::symlink_metadata(zip)?;
    let mtime = meta.modified()?.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    Ok(format!("{code}/0-{mtime}-{}", meta.len()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn resolver(root: &Path, cache: &Path) -> Resolver {
        Resolver {
            root: root.into(),
            cache: Arc::new(DirCache::new(cache.into(), 1 << 30)),
            wait: WaitPolicy { timeout: Duration::from_millis(200), poll: Duration::from_millis(20) },
            zip: ZipLimits::default(),
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
        assert_eq!(problem.case_file("1.in").unwrap(), root.path().join("sum/1.in"));
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
            assert!(problem.case_file(name).is_err(), "{name}");
        }
    }

    #[test]
    fn case_file_accepts_names_in_subdirectories() {
        let (root, cache) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        write_problem(root.path(), None);
        std::fs::create_dir_all(root.path().join("sum/sub")).unwrap();
        std::fs::write(root.path().join("sum/sub/1.in"), "4\n").unwrap();
        let problem = resolver(root.path(), cache.path()).resolve("sum", 0).unwrap();
        assert_eq!(problem.case_file("sub/1.in").unwrap(), root.path().join("sum/sub/1.in"));
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
        assert!(matches!(problem.case_file("1.in"), Err(DataError::Io(_))));
        assert!(matches!(problem.case_file("sub/hostname"), Err(DataError::Io(_))));
        assert!(matches!(problem.data_file("1.in"), Err(DataError::Io(_))));
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
        assert!(problem.files_dir.ends_with("zipped/4"));
        assert_eq!(std::fs::read(problem.case_file("1.in").unwrap()).unwrap(), b"5\n");
    }

    #[test]
    fn missing_init_yml_is_an_io_error_and_bad_code_is_refused() {
        let (root, cache) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let r = resolver(root.path(), cache.path());
        assert!(matches!(r.resolve("nothing", 0), Err(DataError::Io(_))));
        assert!(matches!(r.resolve("../x", 0), Err(DataError::BadCode(_))));
    }
}
