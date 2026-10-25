//! The jury program of a split problem, built in the jury box once per language key (a CPP03 submission's jury
//! is compiled as C++03, as the one-process link compiled the entry) and kept in the artefact cache.
//!
//! 1. Compile the probe, the baseline and the entry (`-c`, the key's `signature` flags) and list their symbols.
//! 2. Roles: defined in the entry = callback, referenced = contestant function (`judge_interface::classify`).
//! 3. Compile the jury glue and the runtime and link them with `entry.o` into `main`.
//!
//! The cache entry holds `main` and `roles`, and only a build that succeeded is kept: a failure is never
//! cached. A missing tool or a sandbox failure is RETRY (`GradeError::RuntimeMissing`, `Sandbox`); a grader
//! that does not compile or link, or an interface the judge cannot split, is a problem fault (IE) for this
//! submission, and the next submission builds again.

use crate::artefacts::Artefacts;
use crate::compile::{BuildOutcome, CompileJob, build, clip_log, program_for};
use crate::error::GradeError;
use crate::languages::Language;
use crate::program::{Program, SourceFile};
use crate::split::compiler_command;
use crate::split::sources::SplitSources;
use crate::split::symbols::{entry_symbols, names, probe_symbols};
use judge_interface::glue::{contestant::contestant_glue, jury::jury_glue, probe};
use judge_interface::{GENERATOR_VERSION, Role, classify, map_declarations, runtime};
use judge_sandbox::IsolateBox;
use std::path::Path;
use std::sync::Arc;

const ROLES_FILE: &str = "roles";
/// Most CPU a jury build step may take (a grader can be as heavy as a checker with testlib).
const JURY_COMPILE_TIME_S: f64 = 30.0;
const ENTRY_OBJECT: &str = "qhhoj_entry.o";
const LISTINGS: [&str; 5] =
    ["qhhoj_probe.nm", "qhhoj_probe.demangled", "qhhoj_baseline.nm", "qhhoj_entry.defined", "qhhoj_entry.nm"];
/// The symbol tools the build runs; the box sees the host's `/usr/bin`.
const TOOLS: [&str; 2] = ["/usr/bin/nm", "/usr/bin/c++filt"];

pub struct SplitJury {
    pub program: Program,
    /// One role per header function, in declaration order (the contestant glue needs them).
    pub roles: Vec<Role>,
}

/// The cached jury for `language`, built in `helper` on first use.
pub fn build_jury(
    artefacts: &Artefacts,
    helper: &mut IsolateBox,
    sources: &SplitSources,
    language: &Language,
) -> Result<SplitJury, GradeError> {
    if let Some(missing) = TOOLS.iter().find(|tool| !Path::new(tool).exists()) {
        return Err(GradeError::RuntimeMissing(missing.to_string()));
    }
    let identity = cache_identity(sources);
    let lease = artefacts.cached("split-jury", (&identity, &language.key), |into| {
        let roles = build_steps(helper, sources, language, into)?;
        std::fs::write(into.join(ROLES_FILE), roles_text(&roles))?;
        Ok(())
    })?;
    let dir = lease.path().to_path_buf();
    let roles = parse_roles(&std::fs::read_to_string(dir.join(ROLES_FILE))?)?;
    let mut program = program_for(language, &dir, b"");
    program.lease = Some(Arc::new(lease));
    Ok(SplitJury { program, roles })
}

/// Everything the build depends on besides the key and the compiler: the entry, the header, the runtime, the
/// generator version, and what the generator writes for this header (so a generator change without a version
/// bump still rebuilds): the probe, the baseline, and both sides' glue with every function on each side.
fn cache_identity(sources: &SplitSources) -> Vec<SourceFile> {
    let (interface, header) = (&sources.interface, sources.header.name.as_str());
    let all = |role: Role| vec![role; interface.functions.len()];
    let generated = [
        probe::probe(interface, header),
        probe::baseline(header),
        jury_glue(interface, &all(Role::Callback), header),
        jury_glue(interface, &all(Role::Contestant), header),
        contestant_glue(interface, &all(Role::Callback), header),
        contestant_glue(interface, &all(Role::Contestant), header),
    ];
    let mut files = vec![
        sources.entry.clone(),
        sources.header.clone(),
        text_file(runtime::HEADER_NAME, runtime::HEADER),
        text_file(runtime::SOURCE_NAME, runtime::SOURCE),
        text_file("generator", &GENERATOR_VERSION.to_string()),
    ];
    files.extend(generated.iter().enumerate().map(|(i, text)| text_file(&format!("generated{i}"), text)));
    files
}

/// The problem fault for a grader that does not build as `key`. The log is not quoted (it is the grader's).
fn does_not_compile(key: &str) -> GradeError {
    GradeError::Checker(format!("signature grader: the entry does not compile as {key}"))
}

fn roles_text(roles: &[Role]) -> String {
    let letter = |role: &Role| match role {
        Role::Callback => 'c',
        Role::Contestant => 'k',
        Role::Unwired => 'u',
    };
    roles.iter().map(letter).collect()
}

fn parse_roles(text: &str) -> Result<Vec<Role>, GradeError> {
    let role = |letter: char| match letter {
        'c' => Ok(Role::Callback),
        'k' => Ok(Role::Contestant),
        'u' => Ok(Role::Unwired),
        other => Err(GradeError::Io(format!("bad role {other:?} in a cached jury build"))),
    };
    text.chars().map(role).collect()
}

/// Steps 1-3 of the module doc; `into` receives `main`.
fn build_steps(
    helper: &mut IsolateBox,
    sources: &SplitSources,
    language: &Language,
    into: &Path,
) -> Result<Vec<Role>, GradeError> {
    let objects = into.join(".objects");
    if let BuildOutcome::CompileError { .. } = build(helper, &objects_job(sources, language), &objects)? {
        return Err(does_not_compile(&language.key));
    }
    let roles = roles_from(sources, &objects)?
        .map_err(|text| GradeError::Checker(format!("signature header: {text}")))?;
    let entry_object = std::fs::read(objects.join(ENTRY_OBJECT))?;
    std::fs::remove_dir_all(&objects)?;
    let link = link_job(sources, language, &roles, entry_object);
    match build(helper, &link, into)? {
        BuildOutcome::Built { .. } => Ok(roles),
        BuildOutcome::CompileError { log } => Err(GradeError::Checker(link_failure(&log))),
    }
}

/// The entry needs something the jury side cannot provide: most often a symbol the contestant was to define
/// that the header does not declare (a variable, or an undeclared function).
fn link_failure(log: &str) -> String {
    let mut text = clip_log(log.as_bytes());
    text.truncate(text.char_indices().nth(400).map_or(text.len(), |(at, _)| at));
    format!("signature grader: the entry does not link without the contestant's code: {text}")
}

/// Roles from the listings of step 1, or the refusal text.
fn roles_from(sources: &SplitSources, objects: &Path) -> Result<Result<Vec<Role>, String>, GradeError> {
    let read = |name: &str| std::fs::read_to_string(objects.join(name));
    let probe = probe_symbols(&read(LISTINGS[0])?, &read(LISTINGS[1])?);
    let baseline = names(&read(LISTINGS[2])?).into_iter().collect();
    let entry = entry_symbols(&read(LISTINGS[3])?, &read(LISTINGS[4])?);
    let (interface, header) = (&sources.interface, sources.header.name.as_str());
    let classified = map_declarations(interface, &probe, &baseline, header)
        .and_then(|symbols| classify(interface, symbols, &entry, header));
    Ok(classified.map(|classification| classification.roles).map_err(|refusal| refusal.to_string()))
}

fn entry_source_name(sources: &SplitSources) -> String {
    let extension = sources.entry.name.rsplit_once('.').map_or("cpp", |(_, extension)| extension);
    format!("qhhoj_entry.{extension}")
}

/// Step 1: the three objects, the entry object kept, and the `nm` listings.
fn objects_job(sources: &SplitSources, language: &Language) -> CompileJob {
    let (interface, header) = (&sources.interface, sources.header.name.as_str());
    let entry = SourceFile { name: entry_source_name(sources), bytes: sources.entry.bytes.clone() };
    let files = vec![
        entry.clone(),
        sources.header.clone(),
        text_file("qhhoj_probe.cpp", &probe::probe(interface, header)),
        text_file("qhhoj_baseline.cpp", &probe::baseline(header)),
    ];
    let cc = compiler_command(language.signature.as_deref().unwrap_or_default());
    let script = format!(
        "{cc} -c qhhoj_probe.cpp -o qhhoj_probe.o && {cc} -c qhhoj_baseline.cpp -o qhhoj_baseline.o && \
         {cc} -c {entry} -o {ENTRY_OBJECT} && \
         /usr/bin/nm -u -p qhhoj_probe.o > qhhoj_probe.nm && /usr/bin/c++filt < qhhoj_probe.nm > qhhoj_probe.demangled && \
         /usr/bin/nm -u -p qhhoj_baseline.o > qhhoj_baseline.nm && \
         /usr/bin/nm --defined-only -p {ENTRY_OBJECT} > qhhoj_entry.defined && /usr/bin/nm -u -p {ENTRY_OBJECT} > qhhoj_entry.nm",
        entry = entry.name,
    );
    let mut keep: Vec<&str> = LISTINGS.to_vec();
    keep.push(ENTRY_OBJECT);
    jury_job(language, files, script, &keep)
}

/// Step 3: the jury glue and the runtime linked with the entry object into `main`.
fn link_job(
    sources: &SplitSources,
    language: &Language,
    roles: &[Role],
    entry_object: Vec<u8>,
) -> CompileJob {
    let glue = jury_glue(&sources.interface, roles, &sources.header.name);
    let files = vec![
        SourceFile { name: ENTRY_OBJECT.into(), bytes: entry_object },
        sources.header.clone(),
        text_file("qhhoj_jury.cpp", &glue),
        text_file(runtime::HEADER_NAME, runtime::HEADER),
        text_file(runtime::SOURCE_NAME, runtime::SOURCE),
    ];
    let cc = compiler_command(language.signature.as_deref().unwrap_or_default());
    let script = format!(
        "{cc} -c qhhoj_jury.cpp -o qhhoj_jury.o && {cc} -c {rpc} -o qhhoj_rpc.o && \
         {cc} {ENTRY_OBJECT} qhhoj_jury.o qhhoj_rpc.o -o main -lm",
        rpc = runtime::SOURCE_NAME,
    );
    jury_job(language, files, script, &["main"])
}

fn text_file(name: &str, text: &str) -> SourceFile {
    SourceFile { name: name.into(), bytes: text.as_bytes().to_vec() }
}

/// A compile step of the jury: the language's compile limits, at least `JURY_COMPILE_TIME_S` of CPU.
fn jury_job(language: &Language, files: Vec<SourceFile>, script: String, keep: &[&str]) -> CompileJob {
    let mut job = CompileJob::for_language(language, b"");
    job.argv = Some(vec!["/bin/sh".into(), "-c".into(), script]);
    job.files = files;
    job.keep = Some(keep.iter().map(|name| name.to_string()).collect());
    job.refusal = None;
    job.limits.time_s = job.limits.time_s.max(JURY_COMPILE_TIME_S);
    job.limits.wall_s = job.limits.wall_s.max(2.0 * JURY_COMPILE_TIME_S);
    job
}
