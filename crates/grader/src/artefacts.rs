//! Checkers and interactors compiled once per content hash (spec §2 "Compiled-artefact cache").
//! The sources are problem-author code: they are compiled inside a box and only ever run in one.

use crate::compile::{BuildOutcome, CompileJob, build, program_for};
use crate::error::GradeError;
use crate::languages::LanguageRegistry;
use crate::program::{Program, SourceFile};
use judge_problem::dir_cache::DirCache;
use judge_problem::resolve::ResolvedProblem;
use judge_sandbox::IsolateBox;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::Read;
use std::path::PathBuf;
use std::sync::Arc;

/// A source, header or grader file read from a problem directory may not be larger than this.
pub const MAX_SOURCE_BYTES: u64 = 1 << 20;

/// The first file is the main source; later ones are headers it includes.
#[derive(Debug, Clone)]
pub struct HelperSource {
    pub lang: String,
    pub files: Vec<SourceFile>,
}

pub struct ArtefactSetup {
    pub cache: Arc<DirCache>,
    pub registry: Arc<LanguageRegistry>,
    /// Language key → output of its `version` command; part of the cache key.
    pub build_ids: HashMap<String, String>,
    /// Host directory mounted read-only at `/testlib` while a helper compiles.
    pub testlib_dir: PathBuf,
}

pub struct Artefacts {
    setup: ArtefactSetup,
}

/// Hex sha256 over each file's name length, name, byte length and bytes, then the language and build id.
pub fn artefact_key(files: &[SourceFile], lang: &str, build_id: &str) -> String {
    let mut hash = Sha256::new();
    for file in files {
        hash.update((file.name.len() as u64).to_le_bytes());
        hash.update(file.name.as_bytes());
        hash.update((file.bytes.len() as u64).to_le_bytes());
        hash.update(&file.bytes);
    }
    hash.update((lang.len() as u64).to_le_bytes());
    hash.update(lang.as_bytes());
    hash.update(build_id.as_bytes());
    hex::encode(hash.finalize())
}

/// The flag goes right after the compiler, before the language's own options.
pub fn with_testlib(mut argv: Vec<String>, flag: Option<&str>) -> Vec<String> {
    if let (Some(flag), false) = (flag, argv.is_empty()) {
        argv.insert(1, flag.to_string());
    }
    argv
}

/// A file stored next to init.yml, read through the resolver's checked handle and capped.
pub fn read_data_source(problem: &ResolvedProblem, name: &str) -> Result<SourceFile, GradeError> {
    let file = problem.open_data_file(name)?;
    let mut bytes = Vec::new();
    file.take(MAX_SOURCE_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_SOURCE_BYTES {
        return Err(GradeError::Checker(format!("{name} is larger than {MAX_SOURCE_BYTES} bytes")));
    }
    Ok(SourceFile { name: name.to_string(), bytes })
}

impl Artefacts {
    pub fn new(setup: ArtefactSetup) -> Self {
        Artefacts { setup }
    }

    pub fn registry(&self) -> &LanguageRegistry {
        &self.setup.registry
    }

    /// Host directory mounted read-only at `/testlib` while a program compiles.
    pub fn testlib_dir(&self) -> &std::path::Path {
        &self.setup.testlib_dir
    }

    /// Compiles `source` once per key. The returned program's lease keeps the cache entry alive.
    pub fn program(&self, sandbox: &mut IsolateBox, source: &HelperSource) -> Result<Program, GradeError> {
        let language = self
            .setup
            .registry
            .get(&source.lang)
            .ok_or_else(|| GradeError::RuntimeMissing(source.lang.clone()))?;
        let main = &source.files.first().ok_or_else(|| GradeError::Checker("helper has no source".into()))?;
        let mut job = CompileJob::for_language(language, &main.bytes);
        job.argv = job.argv.map(|argv| with_testlib(argv, language.testlib_flag.as_deref()));
        job.dirs.push(format!("/testlib={}", self.setup.testlib_dir.display()));
        job.files.extend(source.files[1..].iter().cloned());
        let build_id = self.setup.build_ids.get(&source.lang).map_or("", String::as_str);
        let key = format!("artefacts/{}", artefact_key(&source.files, &source.lang, build_id));
        let lease = self.setup.cache.get_or_build(&key, |into| compile_helper(sandbox, &job, into))?;
        let mut program = program_for(language, lease.path(), &main.bytes);
        program.lease = Some(Arc::new(lease));
        Ok(program)
    }
}

fn compile_helper(
    sandbox: &mut IsolateBox,
    job: &CompileJob,
    into: &std::path::Path,
) -> Result<(), GradeError> {
    match build(sandbox, job, into)? {
        BuildOutcome::Built { .. } => Ok(()),
        BuildOutcome::CompileError { log } => {
            Err(GradeError::Checker(format!("checker failed to compile: {log}")))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(name: &str, bytes: &[u8]) -> SourceFile {
        SourceFile { name: name.into(), bytes: bytes.to_vec() }
    }

    #[test]
    fn the_key_is_stable_and_changes_with_any_input_or_boundary() {
        let key = |files: &[SourceFile], lang: &str, version: &str| artefact_key(files, lang, version);
        let base = key(&[file("c.cpp", b"x")], "CPP17", "14.2.0");
        assert_eq!(base, key(&[file("c.cpp", b"x")], "CPP17", "14.2.0"));
        for other in [
            key(&[file("c.cpp", b"y")], "CPP17", "14.2.0"),
            key(&[file("c.cpp", b"x")], "CPP20", "14.2.0"),
            key(&[file("c.cpp", b"x")], "CPP17", "15.2.0"),
            key(&[file("d.cpp", b"x")], "CPP17", "14.2.0"),
        ] {
            assert_ne!(base, other);
        }
        // name/content and language/version boundaries cannot collide
        assert_ne!(key(&[file("ab", b"c")], "L", "1"), key(&[file("a", b"bc")], "L", "1"));
        assert_ne!(key(&[file("a", b"")], "L", "1"), key(&[file("a", b""), file("", b"")], "L", "1"));
        assert_ne!(key(&[], "AB", "C"), key(&[], "A", "BC"));
    }
}
