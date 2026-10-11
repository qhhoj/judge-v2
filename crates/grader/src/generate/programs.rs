//! Compiling the plan's programs from the unpacked bundle.

use super::failure::{GenerationFailure, Kind, Stage};
use super::plan::{GenerationPlan, PlanProgram, is_safe_relative};
use crate::artefacts::{MAX_SOURCE_BYTES, for_helper};
use crate::compile::{BuildOutcome, CompileJob, build, program_for};
use crate::judge::JudgeEnv;
use crate::languages::Language;
use crate::program::{Program, SourceFile};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Files beside a source that go into its box; more are left out.
const MAX_SIBLINGS: usize = 256;

/// A file of the bundle: a plain relative path to a regular file inside it.
pub fn bundle_file(bundle: &Path, relative: &str) -> Option<PathBuf> {
    if !is_safe_relative(relative) {
        return None;
    }
    let path = bundle.join(relative);
    std::fs::symlink_metadata(&path).ok()?.is_file().then_some(path)
}

fn compile_failure(kind: Kind, program: &PlanProgram, detail: impl Into<String>) -> GenerationFailure {
    GenerationFailure::new(Stage::Compile, kind, detail).of_program(&program.name)
}

/// The language when this judge can run it as a program.
fn language_of(env: &JudgeEnv, program: &PlanProgram) -> Result<Language, GenerationFailure> {
    match env.artefacts.registry().get(&program.language_key) {
        Some(language) if language.kind == crate::languages::LanguageKind::Program => Ok(language.clone()),
        _ => Err(compile_failure(
            Kind::UnsupportedLanguage,
            program,
            format!("language {} is not available on this judge", program.language_key),
        )),
    }
}

fn read_source(bundle: &Path, program: &PlanProgram) -> Result<(PathBuf, Vec<u8>), GenerationFailure> {
    let missing = || {
        compile_failure(Kind::MissingFile, program, format!("{} is not in the bundle", program.source_path))
    };
    let path = bundle_file(bundle, &program.source_path).ok_or_else(missing)?;
    let bytes = std::fs::read(&path)?;
    if bytes.len() as u64 > MAX_SOURCE_BYTES {
        let detail = format!("{} is larger than {MAX_SOURCE_BYTES} bytes", program.source_path);
        return Err(compile_failure(Kind::CompileError, program, detail));
    }
    Ok((path, bytes))
}

/// Regular files of the source's directory other than the source itself, by name, small ones only:
/// the headers a program includes. Subdirectories are not copied (the box is flat).
fn siblings(source: &Path) -> Result<Vec<SourceFile>, GenerationFailure> {
    let Some(dir) = source.parent() else { return Ok(vec![]) };
    let mut found = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let small = entry.metadata().is_ok_and(|meta| meta.len() <= MAX_SOURCE_BYTES);
        let Some(name) = entry.file_name().to_str().map(String::from) else { continue };
        if entry.file_type()?.is_file() && small && entry.path() != source {
            found.push((name, entry.path()));
        }
    }
    found.sort();
    found.truncate(MAX_SIBLINGS);
    // read only what is kept
    found.into_iter().map(|(name, path)| Ok(SourceFile { name, bytes: std::fs::read(path)? })).collect()
}

/// The compile job: the source renamed as the language wants it, the package's own files beside it
/// (so `#include "testlib.h"` finds the package's copy), `/testlib` as the fallback.
fn compile_job(
    env: &JudgeEnv,
    language: &Language,
    source: &SourceFile,
    beside: Vec<SourceFile>,
) -> CompileJob {
    let mut job =
        for_helper(CompileJob::for_language(language, &source.bytes), language, env.artefacts.testlib_dir());
    let taken = language.source_name(&source.bytes);
    job.files.extend(beside.into_iter().filter(|file| file.name != taken));
    job
}

fn compile_program(
    env: &mut JudgeEnv,
    bundle: &Path,
    (program, into): (&PlanProgram, &Path),
) -> Result<Program, GenerationFailure> {
    let language = language_of(env, program)?;
    let (path, bytes) = read_source(bundle, program)?;
    let main = SourceFile { name: program.source_path.clone(), bytes };
    let job = compile_job(env, &language, &main, siblings(&path)?);
    match build(&mut env.helper, &job, into)? {
        BuildOutcome::Built { .. } => Ok(program_for(&language, into, &main.bytes)),
        BuildOutcome::CompileError { log } => Err(compile_failure(Kind::CompileError, program, log)),
    }
}

/// Every program of the plan, by name. `check` runs before each compile (deadline, cancel).
pub fn compile_all(
    env: &mut JudgeEnv,
    plan: &GenerationPlan,
    dirs: (&Path, &Path),
    check: &dyn Fn() -> Result<(), GenerationFailure>,
) -> Result<HashMap<String, Program>, GenerationFailure> {
    let (bundle, programs_dir) = dirs;
    let mut compiled = HashMap::new();
    for (position, program) in plan.programs.iter().enumerate() {
        check()?;
        let into = programs_dir.join(position.to_string());
        compiled.insert(program.name.clone(), compile_program(env, bundle, (program, &into))?);
    }
    Ok(compiled)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundle_files_must_be_plain_relative_regular_files() {
        let bundle = tempfile::tempdir().unwrap();
        std::fs::create_dir(bundle.path().join("files")).unwrap();
        std::fs::write(bundle.path().join("files/gen.cpp"), "x").unwrap();
        std::os::unix::fs::symlink("/etc/hostname", bundle.path().join("files/link")).unwrap();
        assert!(bundle_file(bundle.path(), "files/gen.cpp").is_some());
        assert!(bundle_file(bundle.path(), "files").is_none(), "a directory");
        assert!(bundle_file(bundle.path(), "files/nope.cpp").is_none());
        assert!(bundle_file(bundle.path(), "files/link").is_none(), "a symlink");
        assert!(bundle_file(bundle.path(), "../etc/hostname").is_none());
        assert!(bundle_file(bundle.path(), "/etc/hostname").is_none());
    }

    #[test]
    fn a_source_has_its_directorys_small_regular_files_beside_it_but_not_itself_or_subdirectories() {
        let dir = tempfile::tempdir().unwrap();
        let files = dir.path().join("files");
        std::fs::create_dir_all(files.join("sub")).unwrap();
        for name in ["gen.cpp", "testlib.h", "helper.hpp"] {
            std::fs::write(files.join(name), name).unwrap();
        }
        std::fs::write(files.join("big.bin"), vec![0u8; MAX_SOURCE_BYTES as usize + 1]).unwrap();
        std::fs::write(files.join("sub/inner.h"), "x").unwrap();
        let names: Vec<_> =
            siblings(&files.join("gen.cpp")).unwrap().into_iter().map(|file| file.name).collect();
        assert_eq!(names, ["helper.hpp", "testlib.h"]);
    }
}
