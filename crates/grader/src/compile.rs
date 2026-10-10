//! Compiles inside the solution box with compile limits (spec §5) and snapshots the result.

use crate::error::GradeError;
use crate::languages::{Language, Placeholders, expand, java_main_class};
use crate::program::{Program, SourceFile};
use judge_sandbox::IsolateBox;
use judge_sandbox::classify::{OutputUse, RunVerdict, classify};
use judge_sandbox::error::SandboxError;
use judge_sandbox::spec::{Limits, RunSpec, Stdio};
use std::fs::{File, OpenOptions};
use std::io::Read;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

pub const MAX_LOG_BYTES: usize = 65536;
/// A compiler may not leave more than this in the box; more is a compile error (a disk-filling source).
pub const MAX_SNAPSHOT_BYTES: u64 = 256 << 20;
const LOG: &str = "compile.log";
const ERR: &str = "compile.err";

#[derive(Debug, Clone)]
pub struct CompileJob {
    pub argv: Option<Vec<String>>,
    pub files: Vec<SourceFile>,
    pub limits: Limits,
    pub dirs: Vec<String>,
    /// Only these box files go into the program snapshot (None: everything but the logs).
    pub keep: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum BuildOutcome {
    Built { log: String },
    CompileError { log: String },
}

pub fn clip_log(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let mut end = text.len().min(MAX_LOG_BYTES);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_string()
}

impl CompileJob {
    pub fn for_language(language: &Language, source: &[u8]) -> CompileJob {
        let mut limits = Limits::compile();
        limits.processes = language.compile_processes;
        if let Some(o) = &language.compile_limits {
            (limits.time_s, limits.wall_s, limits.memory_kib) = (o.time_s, o.wall_s, o.memory_kib);
        }
        let values = Placeholders { memory_kib: 0, class: java_main_class(source) };
        CompileJob {
            argv: language.compile.as_ref().map(|argv| expand(argv, &values)),
            files: vec![SourceFile { name: language.source_name(source), bytes: source.to_vec() }],
            limits,
            dirs: language.compile_dirs.clone(),
            keep: None,
        }
    }
}

/// The run command with `{class}` expanded; `{memory_*}` is expanded per run.
pub fn program_for(language: &Language, dir: &Path, source: &[u8]) -> Program {
    let class = java_main_class(source).unwrap_or_else(|| "Main".into());
    Program {
        dir: dir.to_path_buf(),
        run: language.run.iter().map(|a| a.replace("{class}", &class)).collect(),
        processes: language.processes,
        jvm: language.jvm,
        dirs: language.dirs.clone(),
        env: language.env.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
        exception_marker: language.exception_marker.clone(),
        exception_style: language.exception_style,
        lease: None,
    }
}

pub fn build(sandbox: &mut IsolateBox, job: &CompileJob, into: &Path) -> Result<BuildOutcome, GradeError> {
    sandbox.reset()?;
    for file in &job.files {
        sandbox.put_bytes(&file.name, &file.bytes)?;
    }
    let log = match &job.argv {
        Some(argv) => match run_compiler(sandbox, job, argv)? {
            Ok(log) => log,
            Err(log) => return Ok(BuildOutcome::CompileError { log }),
        },
        None => String::new(),
    };
    if snapshot(sandbox.dir(), into, job.keep.as_deref())? {
        Ok(BuildOutcome::Built { log })
    } else {
        Ok(BuildOutcome::CompileError { log: format!("{log}\ncompiled output is too large") })
    }
}

fn read_log(sandbox: &IsolateBox, name: &str) -> Vec<u8> {
    sandbox.read_output(name, MAX_LOG_BYTES as u64).unwrap_or_default()
}

/// Ok(log) when the compiler succeeded, Err(log) for a compile error.
fn run_compiler(
    sandbox: &mut IsolateBox,
    job: &CompileJob,
    argv: &[String],
) -> Result<Result<String, String>, GradeError> {
    let spec = RunSpec {
        argv: argv.to_vec(),
        limits: job.limits.clone(),
        stdio: Stdio { stdin: Some("/dev/null".into()), stdout: Some(LOG.into()), stderr: Some(ERR.into()) },
        env: vec![],
        dirs: job.dirs.clone(),
    };
    let meta = sandbox.run(&spec)?;
    let mut log = read_log(sandbox, LOG);
    log.extend(read_log(sandbox, ERR));
    let mut log = clip_log(&log);
    match classify(&meta, OutputUse { written: 0, cap: u64::MAX }) {
        RunVerdict::Ok => Ok(Ok(log)),
        RunVerdict::Internal(message) => Err(SandboxError::Isolate(message).into()),
        RunVerdict::Tle => Ok(Err(with_note(&mut log, "compilation timed out"))),
        RunVerdict::Mle => Ok(Err(with_note(&mut log, "compiler ran out of memory"))),
        _ => Ok(Err(log)),
    }
}

fn with_note(log: &mut String, note: &str) -> String {
    log.push('\n');
    log.push_str(note);
    std::mem::take(log)
}

/// Copies the box contents (minus the logs, and only `keep` when given) into `into`. Only regular files
/// and directories are copied, never followed through symlinks: the compiler's output is not trusted.
/// Returns false when the output exceeds `MAX_SNAPSHOT_BYTES`.
fn snapshot(box_dir: &Path, into: &Path, keep: Option<&[String]>) -> Result<bool, GradeError> {
    std::fs::create_dir_all(into)?;
    let mut budget = MAX_SNAPSHOT_BYTES;
    for entry in std::fs::read_dir(box_dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let wanted = keep.is_none_or(|names| names.iter().any(|k| name == k.as_str()));
        if name == LOG || name == ERR || !wanted {
            continue;
        }
        if !copy_entry(&entry, &into.join(&name), &mut budget, 0)? {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Charged to the budget per directory and per file, so a flood of empty entries is bounded too.
const ENTRY_COST: u64 = 4096;
/// A compiler output nested deeper than this is a compile error.
const MAX_DEPTH: u32 = 16;

fn copy_entry(
    entry: &std::fs::DirEntry,
    to: &Path,
    budget: &mut u64,
    depth: u32,
) -> Result<bool, GradeError> {
    let kind = entry.file_type()?;
    if !(kind.is_dir() || kind.is_file()) {
        return Ok(true); // symlinks, sockets and fifos are dropped
    }
    if depth > MAX_DEPTH || *budget < ENTRY_COST {
        return Ok(false);
    }
    *budget -= ENTRY_COST;
    if kind.is_file() {
        return copy_file(&entry.path(), to, budget);
    }
    std::fs::create_dir_all(to)?;
    for child in std::fs::read_dir(entry.path())? {
        let child = child?;
        if !copy_entry(&child, &to.join(child.file_name()), budget, depth + 1)? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn copy_file(from: &Path, to: &Path, budget: &mut u64) -> Result<bool, GradeError> {
    // O_NOFOLLOW refuses a symlink swapped in after the directory listing, O_NONBLOCK keeps a fifo from hanging.
    let flags = libc::O_NOFOLLOW | libc::O_NONBLOCK;
    let source = OpenOptions::new().read(true).custom_flags(flags).open(from)?;
    let meta = source.metadata()?;
    if !meta.is_file() {
        return Ok(true);
    }
    let mode = meta.permissions().mode() & 0o777;
    let mut target = File::create(to)?;
    let copied = std::io::copy(&mut source.take(*budget + 1), &mut target)?;
    target.set_permissions(std::fs::Permissions::from_mode(mode | 0o400))?;
    if copied > *budget {
        return Ok(false);
    }
    *budget -= copied;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::languages::LanguageRegistry;

    fn registry() -> LanguageRegistry {
        LanguageRegistry::from_toml(include_str!("../../../languages.toml")).unwrap()
    }

    #[test]
    fn etc_symlinked_runtimes_bind_only_the_etc_dirs_they_need() {
        use judge_sandbox::spec::isolate_args;
        let r = registry();
        let flags = |dirs: Vec<String>| {
            let spec = RunSpec {
                argv: vec!["x".into()],
                limits: Limits::compile(),
                stdio: Stdio::default(),
                env: vec![],
                dirs,
            };
            isolate_args(0, &spec, Path::new("/m"))
                .into_iter()
                .filter(|a| a.starts_with("--dir="))
                .collect::<Vec<_>>()
        };
        let pas = CompileJob::for_language(r.get("PAS").unwrap(), b"begin end.");
        assert_eq!(flags(pas.dirs), ["--dir=/etc/alternatives"]);
        for key in ["JAVA", "JAVA8", "KOTLIN"] {
            let language = r.get(key).unwrap();
            let compile = CompileJob::for_language(language, b"class A {}");
            let run = program_for(language, Path::new("/p"), b"class A {}");
            assert_eq!(flags(compile.dirs), ["--dir=/etc/java-21-openjdk"], "{key} compile");
            assert_eq!(flags(run.dirs), ["--dir=/etc/java-21-openjdk"], "{key} run");
        }
        assert!(CompileJob::for_language(r.get("CPP17").unwrap(), b"").dirs.is_empty());
    }

    #[test]
    fn job_names_the_source_and_uses_language_limits() {
        let r = registry();
        let job = CompileJob::for_language(r.get("JAVA").unwrap(), b"public class Sol {}");
        assert_eq!(job.files[0].name, "Sol.java");
        assert_eq!(job.argv.as_ref().unwrap().last().unwrap(), "Sol.java");
        let kotlin = CompileJob::for_language(r.get("KOTLIN").unwrap(), b"fun main(){}");
        let limits = (kotlin.limits.time_s, kotlin.limits.memory_kib, kotlin.limits.processes);
        assert_eq!(limits, (30.0, 2_097_152, 128));
        let py = CompileJob::for_language(r.get("PYPY3").unwrap(), b"print(1)");
        assert!(py.argv.is_none());
    }

    #[test]
    fn program_expands_the_class_but_not_memory() {
        let r = registry();
        let p = program_for(r.get("JAVA").unwrap(), Path::new("/p"), b"public class Sol {}");
        assert!(p.run.contains(&"Sol".to_string()));
        assert!(p.run.iter().any(|a| a.contains("{memory_mib}")));
        assert!(p.jvm);
        assert_eq!(p.processes, 64);
    }

    #[test]
    fn logs_are_clipped_to_64_kib_on_a_char_boundary() {
        let long = "é".repeat(40_000); // 80,000 bytes
        let clipped = clip_log(long.as_bytes());
        assert!(clipped.len() <= MAX_LOG_BYTES);
        assert!(clipped.chars().all(|c| c == 'é'));
    }

    #[test]
    fn the_snapshot_keeps_files_and_modes_but_drops_logs_symlinks_and_unlisted_names() {
        let (from, into) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        std::fs::write(from.path().join("main"), b"bin").unwrap();
        std::fs::set_permissions(from.path().join("main"), std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::create_dir(from.path().join("pkg")).unwrap();
        std::fs::write(from.path().join("pkg/A.class"), b"cls").unwrap();
        std::fs::write(from.path().join(LOG), b"log").unwrap();
        std::os::unix::fs::symlink("/etc/hostname", from.path().join("link")).unwrap();
        std::os::unix::fs::symlink("/etc", from.path().join("pkg/etc")).unwrap();
        assert!(snapshot(from.path(), into.path(), None).unwrap());
        let mut names: Vec<_> = walk(into.path());
        names.sort();
        assert_eq!(names, ["main", "pkg/A.class"]);
        let mode = std::fs::metadata(into.path().join("main")).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o755);
        let only = tempfile::tempdir().unwrap();
        assert!(snapshot(from.path(), only.path(), Some(&["main".to_string()])).unwrap());
        assert_eq!(walk(only.path()), ["main"]);
    }

    #[test]
    fn deeply_nested_output_is_refused_instead_of_failing_the_grader() {
        let (from, into) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let deep = (0..40).fold(from.path().to_path_buf(), |p, i| p.join(format!("d{i}")));
        std::fs::create_dir_all(&deep).unwrap();
        assert!(!snapshot(from.path(), into.path(), None).unwrap());
    }

    #[test]
    fn a_flood_of_empty_files_is_charged_to_the_budget() {
        let (from, into) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        for i in 0..20 {
            std::fs::write(from.path().join(format!("f{i}")), b"").unwrap();
        }
        let mut budget = ENTRY_COST * 5;
        let all = std::fs::read_dir(from.path()).unwrap().all(|e| {
            let e = e.unwrap();
            copy_entry(&e, &into.path().join(e.file_name()), &mut budget, 0).unwrap()
        });
        assert!(!all);
    }

    fn walk(root: &Path) -> Vec<String> {
        fn go(dir: &Path, prefix: &str, out: &mut Vec<String>) {
            for e in std::fs::read_dir(dir).unwrap() {
                let e = e.unwrap();
                let name = format!("{prefix}{}", e.file_name().to_string_lossy());
                if e.file_type().unwrap().is_dir() {
                    go(&e.path(), &format!("{name}/"), out);
                } else {
                    out.push(name);
                }
            }
        }
        let mut out = vec![];
        go(root, "", &mut out);
        out
    }

    #[test]
    fn a_snapshot_over_the_byte_budget_is_refused() {
        let (from, into) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        std::fs::write(from.path().join("big"), vec![0u8; 1000]).unwrap();
        let entry = std::fs::read_dir(from.path()).unwrap().next().unwrap().unwrap();
        let mut budget = ENTRY_COST + 999;
        assert!(!copy_entry(&entry, &into.path().join("big"), &mut budget, 0).unwrap());
        let mut enough = ENTRY_COST + 1000;
        assert!(copy_entry(&entry, &into.path().join("big"), &mut enough, 0).unwrap());
        assert_eq!(enough, 0);
    }
}
