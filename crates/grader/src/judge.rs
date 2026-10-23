//! One submission: plan, build, choose the checker, grade (spec §2 "assignment lifetime").
//!
//! Refusals come first and never touch a box: an init.yml feature this judge does not grade is
//! `RETRY UNSUPPORTED`; a language key with no runtime here (SCRATCH, or a key the daemon removed at
//! start) is `RETRY RUNTIME_MISSING`, so the bridge moves the submission on instead of waiting for it.

use crate::artefacts::{Artefacts, HelperSource, read_data_source};
use crate::case_judges::{
    CaseContext, Check, InteractiveCases, OutputCases, ProgramCases, UnreadableOutputs,
};
use crate::checkers::custom::{CustomChecker, PartialPoints};
use crate::checkers::select::{CheckerChoice, CustomKind, choose};
use crate::checkers::themis::{self, ThemisNames};
use crate::compile::{BuildOutcome, CompileJob, build, program_for};
use crate::error::GradeError;
use crate::file_io::SolutionFileIo;
use crate::grade::grade_cases;
use crate::languages::{Language, LanguageKind};
use crate::output_only::OutputSource;
use crate::program::Program;
use crate::report::Reporter;
use crate::run_case::CaseLimits;
use crate::session::Boxes;
use crate::signature::{SIGNATURE_LANGUAGES, signature_job};
use judge_problem::data_version::{DataError, StampSource};
use judge_problem::init::{CheckerSpec, FilesSpec, InitYaml, InteractiveSpec};
use judge_problem::plan::{CasePlan, PlanOptions, plan_cases};
use judge_problem::resolve::ResolvedProblem;
use judge_sandbox::IsolateBox;
use std::path::PathBuf;
use std::sync::Arc;

/// Compile-error text for a TEXT or OUTPUT submission to an interactive problem.
pub const PROGRAM_REQUIRED: &str = "This problem accepts only programs.";
/// Compile-error text for a program submitted to an output-only problem.
pub const OUTPUT_REQUIRED: &str = "This problem accepts only output files.";

#[derive(Debug, Clone)]
pub struct GraderDefaults {
    pub default_output_limit: u64,
    pub default_output_prefix: u64,
    pub jvm_reserve_kib: u64,
}

/// One worker's grading resources. `stamps` is the resolver's stamp source: a loose-file problem's
/// stamp is read again when grading ends.
pub struct JudgeEnv {
    pub solution: IsolateBox,
    pub helper: IsolateBox,
    pub artefacts: Arc<Artefacts>,
    pub stamps: Arc<dyn StampSource>,
    pub scratch: PathBuf,
    pub defaults: GraderDefaults,
}

#[derive(Debug, Clone)]
pub enum SubmissionSource {
    Inline(Vec<u8>),
    File(PathBuf),
}

#[derive(Debug, Clone)]
pub struct Submission {
    pub language_key: String,
    pub source: SubmissionSource,
    pub time_limit_s: f64,
    pub memory_kib: u64,
    pub short_circuit: bool,
    pub pretests_only: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryReason {
    DataUnavailable,
    RuntimeMissing,
    SandboxFailure,
    Unsupported,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    Graded,
    CompileError(String),
    InternalError(String),
    Aborted,
    Retry(RetryReason),
}

pub fn outcome_of(error: GradeError) -> Outcome {
    match error {
        GradeError::Sandbox(_) => Outcome::Retry(RetryReason::SandboxFailure),
        GradeError::RuntimeMissing(_) => Outcome::Retry(RetryReason::RuntimeMissing),
        GradeError::Unsupported(_) => Outcome::Retry(RetryReason::Unsupported),
        GradeError::Data(DataError::Unavailable { .. }) => Outcome::Retry(RetryReason::DataUnavailable),
        GradeError::Aborted => Outcome::Aborted,
        other => Outcome::InternalError(other.to_string()),
    }
}

pub fn judge_submission(
    env: &mut JudgeEnv,
    problem: &ResolvedProblem,
    submission: &Submission,
    reporter: &mut dyn Reporter,
) -> Outcome {
    let outcome = match try_judge(env, problem, submission, reporter) {
        Ok(None) => Outcome::Graded,
        Ok(Some(compile_log)) => Outcome::CompileError(compile_log),
        Err(error) => outcome_of(error),
    };
    settle(outcome, env.stamps.as_ref(), problem)
}

/// Grading reads from the data root after resolve: a loose-file problem its case files, any problem its
/// checker, interactor, signature and file I/O sources (an archive problem only its tests come from the
/// immutable cache entry). If the stamp moved (or an upload began) meanwhile, the result may mix two
/// versions: it becomes `RETRY DATA_UNAVAILABLE`.
fn settle(outcome: Outcome, stamps: &dyn StampSource, problem: &ResolvedProblem) -> Outcome {
    if !may_have_read_data_root(&outcome, problem) {
        return outcome;
    }
    let dir = &problem.data_dir;
    // The marker first: the uploader writes the stamp and then removes the marker, so an upload that ends
    // between these two reads still shows its new stamp. Read the other way, it could show neither.
    if stamps.upload_in_progress(dir) {
        return Outcome::Retry(RetryReason::DataUnavailable);
    }
    let stamp = stamps.read(dir).map(|found| found.unwrap_or(0));
    if stamp != Ok(problem.data_version_used) {
        return Outcome::Retry(RetryReason::DataUnavailable);
    }
    outcome
}

/// Whether the outcome may come from files read off the data root after resolve, archive or not. A compile
/// error does only for a signature problem (its entry and header); Aborted and Retry are kept as they are.
fn may_have_read_data_root(outcome: &Outcome, problem: &ResolvedProblem) -> bool {
    match outcome {
        Outcome::Aborted | Outcome::Retry(_) => false,
        Outcome::CompileError(_) => problem.init.signature_grader.is_some(),
        Outcome::Graded | Outcome::InternalError(_) => true,
    }
}

/// The submission being graded: its plan and where results go.
struct Run<'a> {
    plan: CasePlan,
    submission: &'a Submission,
    reporter: &'a mut dyn Reporter,
}

/// Ok(Some(log)) is a compile error.
fn try_judge(
    env: &mut JudgeEnv,
    problem: &ResolvedProblem,
    submission: &Submission,
    reporter: &mut dyn Reporter,
) -> Result<Option<String>, GradeError> {
    if let Some(feature) = unsupported_feature(&problem.init) {
        return Err(GradeError::Unsupported(feature));
    }
    let language = env.artefacts.registry().get(&submission.language_key).cloned();
    let language = language.ok_or_else(|| GradeError::RuntimeMissing(submission.language_key.clone()))?;
    if let Some(log) = refused_language(&language, &problem.init) {
        return Ok(Some(log.to_string()));
    }
    let plan = plan_of(problem, submission, &env.defaults)?;
    if problem.init.interactive.is_none() {
        single_checker(&plan)?; // refused before any contestant code is compiled
    }
    let source = read_source(&submission.source)?;
    let run = Run { plan, submission, reporter };
    if language.kind != LanguageKind::Program {
        grade_outputs(env, problem, (&language, source), run)?;
        return Ok(None);
    }
    let program = match build_solution(env, problem, &language, &source)? {
        Ok((program, log)) => {
            run.reporter.compiled(&log);
            program
        }
        Err(log) => return Ok(Some(log)),
    };
    let file_io = problem.init.file_io.as_ref().map(|io| SolutionFileIo::new(io, &source));
    grade_program(env, problem, (program, file_io), run)?;
    Ok(None)
}

/// init.yml's own refusals, plus an interactor protocol other than testlib's.
fn unsupported_feature(init: &InitYaml) -> Option<String> {
    let interactive = init.interactive.as_ref().filter(|i| i.kind != "testlib");
    init.unsupported_feature().or_else(|| interactive.map(|i| format!("interactive.{}", i.kind)))
}

/// A submission kind the problem cannot grade, as compile-error text: a program for an output-only
/// problem, or a TEXT/OUTPUT answer for a problem that links (signature) or talks to (interactive) a program.
fn refused_language(language: &Language, init: &InitYaml) -> Option<&'static str> {
    let program = language.kind == LanguageKind::Program;
    if program {
        return init.output_only.then_some(OUTPUT_REQUIRED);
    }
    if init.signature_grader.is_some() {
        return Some(SIGNATURE_LANGUAGES);
    }
    init.interactive.is_some().then_some(PROGRAM_REQUIRED)
}

fn plan_of(
    problem: &ResolvedProblem,
    submission: &Submission,
    defaults: &GraderDefaults,
) -> Result<CasePlan, GradeError> {
    let options = PlanOptions {
        pretests_only: submission.pretests_only,
        default_output_limit: defaults.default_output_limit,
        default_output_prefix: defaults.default_output_prefix,
    };
    plan_cases(&problem.init, &options).map_err(|e| GradeError::Data(DataError::Init(e)))
}

/// Per-case checker overrides are unused in legacy data: every case must resolve to the same checker,
/// otherwise the problem is `UNSUPPORTED` rather than graded with the wrong one.
pub fn single_checker(plan: &CasePlan) -> Result<CheckerSpec, GradeError> {
    let Some(first) = plan.cases.first() else { return Ok(CheckerSpec::Name("standard".into())) };
    let same =
        |spec: &CheckerSpec| spec.name() == first.checker.name() && spec.args() == first.checker.args();
    if !plan.cases.iter().all(|case| same(&case.checker)) {
        return Err(GradeError::Unsupported("checker.per_case".into()));
    }
    Ok(first.checker.clone())
}

fn read_source(source: &SubmissionSource) -> Result<Vec<u8>, GradeError> {
    match source {
        SubmissionSource::Inline(bytes) => Ok(bytes.clone()),
        SubmissionSource::File(path) => Ok(std::fs::read(path)?),
    }
}

/// Ok(Ok((program, log))) built, Ok(Err(log)) compile error.
fn build_solution(
    env: &mut JudgeEnv,
    problem: &ResolvedProblem,
    language: &Language,
    source: &[u8],
) -> Result<Result<(Program, String), String>, GradeError> {
    let job = match &problem.init.signature_grader {
        Some(signature) => match signature_job(language, problem, signature, source)? {
            Some(job) => job,
            None => return Ok(Err(SIGNATURE_LANGUAGES.to_string())),
        },
        None => CompileJob::for_language(language, source),
    };
    let dir = env.scratch.join("program");
    if dir.exists() {
        std::fs::remove_dir_all(&dir)?;
    }
    match build(&mut env.solution, &job, &dir)? {
        BuildOutcome::Built { log } => Ok(Ok((program_for(language, &dir, source), log))),
        BuildOutcome::CompileError { log } => Ok(Err(log)),
    }
}

/// A problem-author program (checker or interactor) from files next to init.yml, compiled once per hash.
fn helper_program(
    env: &mut JudgeEnv,
    problem: &ResolvedProblem,
    lang: &str,
    names: &[&str],
) -> Result<Program, GradeError> {
    let files = names.iter().map(|name| read_data_source(problem, name)).collect::<Result<Vec<_>, _>>()?;
    let source = HelperSource { lang: lang.to_string(), files };
    let artefacts: &Artefacts = &env.artefacts;
    artefacts.program(&mut env.helper, &source)
}

fn check_of(
    env: &mut JudgeEnv,
    problem: &ResolvedProblem,
    spec: &CheckerSpec,
    time_limit_s: f64,
) -> Result<Check, GradeError> {
    let unicode = problem.init.hints.iter().any(|hint| hint == "unicode");
    match choose(spec, unicode)? {
        CheckerChoice::Builtin(checker) => Ok(Check::Builtin(checker)),
        CheckerChoice::Custom(custom) => {
            let files: Vec<&str> = custom.files.iter().map(String::as_str).collect();
            let program = helper_program(env, problem, &custom.lang, &files)?;
            let themis_names = match custom.kind {
                CustomKind::Themis => themis_names(problem, files[0])?,
                _ => ThemisNames::default(),
            };
            let partial = custom.partial;
            Ok(Check::Custom(CustomChecker {
                program,
                kind: custom.kind,
                partial,
                themis_names,
                time_limit_s,
            }))
        }
    }
}

/// A themis checker opens the case files by name: the problem's file I/O names and its own source's literals
/// (and, per case, the case's own file names: `CustomChecker::check_case`).
fn themis_names(problem: &ResolvedProblem, source: &str) -> Result<ThemisNames, GradeError> {
    let bytes = read_data_source(problem, source)?.bytes;
    Ok(themis::names_for(problem.init.file_io.as_ref(), &bytes))
}

fn limits_of(submission: &Submission, defaults: &GraderDefaults) -> CaseLimits {
    CaseLimits {
        time_limit_s: submission.time_limit_s,
        memory_kib: submission.memory_kib,
        jvm_reserve_kib: defaults.jvm_reserve_kib,
    }
}

/// An interactive problem compiles only the checker its `interactive.checker` names: the top-level and
/// per-case checkers of a legacy interactive problem are never compiled or run (preflight M13).
fn grade_program(
    env: &mut JudgeEnv,
    problem: &ResolvedProblem,
    (program, file_io): (Program, Option<SolutionFileIo>),
    run: Run,
) -> Result<(), GradeError> {
    if let Some(spec) = &problem.init.interactive {
        return grade_interactive(env, problem, (program, spec), run);
    }
    let check = check_of(env, problem, &single_checker(&run.plan)?, run.submission.time_limit_s)?;
    let context = CaseContext { problem, check, scratch: env.scratch.clone() };
    let limits = limits_of(run.submission, &env.defaults);
    let boxes = Boxes { solution: &mut env.solution, helper: &mut env.helper };
    let mut judge = ProgramCases { context, boxes, program, limits, file_io };
    grade_cases(&run.plan, &mut judge, run.submission.short_circuit, run.reporter)
}

fn interactor_files(files: &FilesSpec) -> Vec<&str> {
    match files {
        FilesSpec::One(name) => vec![name.as_str()],
        FilesSpec::Many(names) => names.iter().map(String::as_str).collect(),
    }
}

/// The interactor plays every case; `interactive.checker`, when named, is compiled before the first case.
fn grade_interactive(
    env: &mut JudgeEnv,
    problem: &ResolvedProblem,
    (program, spec): (Program, &InteractiveSpec),
    run: Run,
) -> Result<(), GradeError> {
    let interactor = helper_program(env, problem, &spec.lang, &interactor_files(&spec.files))?;
    let time_limit_s = run.submission.time_limit_s;
    let check =
        spec.checker.as_ref().map(|checker| check_of(env, problem, checker, time_limit_s)).transpose()?;
    let checker = check.map(|check| CaseContext { problem, check, scratch: env.scratch.clone() });
    let partial = if spec.treat_checker_points_as_percentage {
        PartialPoints::Percentage
    } else {
        PartialPoints::Absolute
    };
    let limits = limits_of(run.submission, &env.defaults);
    let boxes = Boxes { solution: &mut env.solution, helper: &mut env.helper };
    let mut judge = InteractiveCases { problem, boxes, program, interactor, limits, checker, partial };
    grade_cases(&run.plan, &mut judge, run.submission.short_circuit, run.reporter)
}

/// TEXT answers every case with the source. An OUTPUT zip that cannot be opened is graded as
/// `UnreadableOutputs` (every case WA, the contestant's mistake); one over `MAX_OUTPUT_ENTRIES` stays IE
/// (preflight M10).
fn grade_outputs(
    env: &mut JudgeEnv,
    problem: &ResolvedProblem,
    (language, source): (&Language, Vec<u8>),
    run: Run,
) -> Result<(), GradeError> {
    let (plan, short_circuit) = (&run.plan, run.submission.short_circuit);
    if language.kind == LanguageKind::OutputZip && !is_readable_zip(&source) {
        return grade_cases(plan, &mut UnreadableOutputs, short_circuit, run.reporter);
    }
    let outputs = match language.kind {
        LanguageKind::OutputZip => {
            let zip = env.scratch.join("submission.zip");
            std::fs::write(&zip, &source)?;
            drop(source); // the archive is read from disk; do not hold the upload for the whole run
            let case_names: Vec<&str> = plan.cases.iter().map(|case| case.output.as_str()).collect();
            OutputSource::zip(zip, &case_names)?
        }
        _ => OutputSource::Text(source),
    };
    let check = check_of(env, problem, &single_checker(plan)?, run.submission.time_limit_s)?;
    let context = CaseContext { problem, check, scratch: env.scratch.clone() };
    let mut judge = OutputCases { context, helper: &mut env.helper, source: outputs };
    grade_cases(plan, &mut judge, short_circuit, run.reporter)
}

/// The zip's central directory parses. In memory, so a failure is the bytes' fault, never the host's.
fn is_readable_zip(bytes: &[u8]) -> bool {
    zip::ZipArchive::new(std::io::Cursor::new(bytes)).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use judge_problem::init::parse_init;
    use judge_sandbox::error::SandboxError;

    #[test]
    fn errors_map_to_outcomes() {
        let cases = [
            (
                GradeError::Sandbox(SandboxError::Isolate("x".into())),
                Outcome::Retry(RetryReason::SandboxFailure),
            ),
            (GradeError::RuntimeMissing("KOTLIN".into()), Outcome::Retry(RetryReason::RuntimeMissing)),
            (GradeError::Unsupported("generator".into()), Outcome::Retry(RetryReason::Unsupported)),
            (
                GradeError::Data(DataError::Unavailable { assigned: 2, found: Some(1) }),
                Outcome::Retry(RetryReason::DataUnavailable),
            ),
            (GradeError::Aborted, Outcome::Aborted),
            (GradeError::Checker("crash".into()), Outcome::InternalError("checker: crash".into())),
            (
                GradeError::Data(DataError::Io("1.in missing".into())),
                Outcome::InternalError("problem data i/o: 1.in missing".into()),
            ),
        ];
        for (error, outcome) in cases {
            assert_eq!(outcome_of(error), outcome);
        }
    }

    fn plan(yaml: &str) -> CasePlan {
        let options = PlanOptions { pretests_only: false, default_output_limit: 1, default_output_prefix: 0 };
        plan_cases(&parse_init(yaml).unwrap(), &options).unwrap()
    }

    #[test]
    fn per_case_checkers_are_unsupported() {
        let mixed = plan(
            "checker: standard\ntest_cases:\n- {in: a, out: b, points: 1, checker: identical}\n- {in: c, out: d, points: 1}\n",
        );
        assert_eq!(single_checker(&mixed).unwrap_err(), GradeError::Unsupported("checker.per_case".into()));
        let args = plan(
            "test_cases:\n- {in: a, out: b, points: 1, checker: {name: floats, args: {precision: 3}}}\n- {in: c, out: d, points: 1, checker: {name: floats, args: {precision: 4}}}\n",
        );
        assert!(single_checker(&args).is_err(), "same name, different args");
        let same = plan(
            "checker: linecount\ntest_cases:\n- {in: a, out: b, points: 1}\n- {in: c, out: d, points: 1, checker: linecount}\n",
        );
        assert_eq!(single_checker(&same).unwrap().name(), "linecount");
        assert_eq!(single_checker(&plan("test_cases: []\n")).unwrap().name(), "standard");
    }

    #[test]
    fn features_this_judge_does_not_grade_are_reported() {
        let split =
            parse_init("test_cases: []\nsignature_grader: {entry: g.cpp, header: s.h, split: true}\n")
                .unwrap();
        assert_eq!(unsupported_feature(&split).as_deref(), Some("signature.split"));
        let paired = parse_init(
            "test_cases: []\nsignature_grader: {entry: g.cpp, header: s.h, split: true, cores: 2}\n",
        )
        .unwrap();
        assert_eq!(unsupported_feature(&paired).as_deref(), Some("signature.split.paired"));
        let themis =
            parse_init("test_cases: []\ninteractive: {files: i.cpp, type: themis, lang: CPP17}\n").unwrap();
        assert_eq!(unsupported_feature(&themis).as_deref(), Some("interactive.themis"));
        let testlib =
            parse_init("test_cases: []\ninteractive: {files: i.cpp, type: testlib, lang: CPP17}\n").unwrap();
        assert_eq!(unsupported_feature(&testlib), None);
        let generator = parse_init("generator: g.cpp\ntest_cases: []\n").unwrap();
        assert_eq!(unsupported_feature(&generator).as_deref(), Some("generator"));
    }

    /// A stamp source answering from fixed values, as the files would at the end of grading.
    struct Stamps {
        read: Result<Option<u64>, DataError>,
        uploading: bool,
    }

    impl StampSource for Stamps {
        fn read(&self, _dir: &std::path::Path) -> Result<Option<u64>, DataError> {
            self.read.clone()
        }
        fn upload_in_progress(&self, _dir: &std::path::Path) -> bool {
            self.uploading
        }
    }

    fn loose_problem(used: u64) -> ResolvedProblem {
        loose_problem_with("test_cases: []\n", used)
    }

    fn loose_problem_with(init: &str, used: u64) -> ResolvedProblem {
        ResolvedProblem {
            init: parse_init(init).unwrap(),
            data_dir: "/nonexistent".into(),
            files_dir: "/nonexistent".into(),
            data_version_used: used,
            lease: None,
        }
    }

    fn settled(
        outcome: Outcome,
        read: Result<Option<u64>, DataError>,
        uploading: bool,
        used: u64,
    ) -> Outcome {
        settle(outcome, &Stamps { read, uploading }, &loose_problem(used))
    }

    #[test]
    fn a_signature_compile_error_is_retried_when_the_stamp_moved() {
        let signature =
            loose_problem_with("test_cases: []\nsignature_grader: {entry: g.cpp, header: s.h}\n", 3);
        let moved = Stamps { read: Ok(Some(4)), uploading: false };
        let outcome = settle(Outcome::CompileError("log".into()), &moved, &signature);
        assert_eq!(
            outcome,
            Outcome::Retry(RetryReason::DataUnavailable),
            "the entry or header may have changed"
        );
    }

    /// An upload that is running when `settle` starts and ends (stamp 4 written, then the marker removed)
    /// right after the first question `settle` asks, whichever question that is.
    struct UploadEndingNow {
        state: std::sync::Mutex<(Option<u64>, bool)>,
    }

    impl UploadEndingNow {
        fn answer<T>(&self, ask: impl FnOnce(&(Option<u64>, bool)) -> T) -> T {
            let mut state = self.state.lock().unwrap();
            let answer = ask(&state);
            *state = (Some(4), false);
            answer
        }
    }

    impl StampSource for UploadEndingNow {
        fn read(&self, _dir: &std::path::Path) -> Result<Option<u64>, DataError> {
            Ok(self.answer(|state| state.0))
        }
        fn upload_in_progress(&self, _dir: &std::path::Path) -> bool {
            self.answer(|state| state.1)
        }
    }

    #[test]
    fn an_upload_ending_during_the_recheck_is_still_seen() {
        let stamps = UploadEndingNow { state: std::sync::Mutex::new((Some(3), true)) };
        let outcome = settle(Outcome::Graded, &stamps, &loose_problem(3));
        assert_eq!(outcome, Outcome::Retry(RetryReason::DataUnavailable));
    }

    #[test]
    fn a_loose_problem_whose_stamp_moved_is_retried() {
        let retry = Outcome::Retry(RetryReason::DataUnavailable);
        let gone = || Err(DataError::Io("gone".into()));
        let compile_error = Outcome::CompileError("log".into());
        let ie = Outcome::InternalError("1.in missing".into());
        let unsupported = Outcome::Retry(RetryReason::Unsupported);
        // (outcome, stamp read at the end, upload marker, version used) -> settled outcome
        let cases = [
            (Outcome::Graded, Ok(Some(3)), false, 3, Outcome::Graded),
            (Outcome::Graded, Ok(None), false, 0, Outcome::Graded), // unstamped stays unstamped
            (Outcome::Graded, Ok(Some(4)), false, 3, retry.clone()),
            (Outcome::Graded, Ok(Some(1)), false, 0, retry.clone()), // a stamp appeared
            (Outcome::Graded, Ok(Some(3)), true, 3, retry.clone()),  // an upload began
            (Outcome::Graded, gone(), false, 3, retry.clone()),
            (compile_error.clone(), Ok(Some(4)), false, 3, compile_error), // no data in a plain compile
            (ie, Ok(Some(4)), false, 3, retry.clone()),                    // a half-written upload is not IE
            (Outcome::Aborted, Ok(Some(4)), false, 3, Outcome::Aborted),
            (unsupported.clone(), Ok(Some(4)), false, 3, unsupported),
        ];
        for (outcome, read, uploading, used, want) in cases {
            assert_eq!(settled(outcome.clone(), read.clone(), uploading, used), want, "{outcome:?} {read:?}");
        }
    }

    /// An archive problem at `version` in `root/code`: its tests in t.zip, `extra` appended to init.yml.
    fn archive_problem(root: &std::path::Path, cache: &std::path::Path, extra: &str) -> ResolvedProblem {
        use judge_problem::{
            archive::ZipLimits,
            data_version::{FsStamps, WaitPolicy},
            dir_cache::DirCache,
        };
        let dir = root.join("arch");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("init.yml"),
            format!("archive: t.zip\ntest_cases:\n- {{in: 1.in, out: 1.out, points: 1}}\n{extra}"),
        )
        .unwrap();
        let mut writer = zip::ZipWriter::new(std::fs::File::create(dir.join("t.zip")).unwrap());
        writer.start_file("1.in", zip::write::SimpleFileOptions::default()).unwrap();
        writer.start_file("1.out", zip::write::SimpleFileOptions::default()).unwrap();
        writer.finish().unwrap();
        std::fs::write(dir.join("checker.cpp"), "// version 5\n").unwrap();
        std::fs::write(dir.join(".data-version"), "5\n").unwrap();
        let resolver = judge_problem::resolve::Resolver {
            root: root.into(),
            cache: std::sync::Arc::new(DirCache::new(cache.into(), 1 << 30)),
            wait: WaitPolicy {
                timeout: std::time::Duration::from_millis(200),
                poll: std::time::Duration::from_millis(20),
            },
            zip: ZipLimits::default(),
            stamps: std::sync::Arc::new(FsStamps),
        };
        let problem = resolver.resolve("arch", 5).unwrap();
        assert!(problem.lease.is_some(), "graded from the archive cache entry");
        problem
    }

    /// The uploader's version 6: a new checker source next to init.yml, then the stamp (both written atomically).
    fn upload_version_6(problem: &ResolvedProblem) {
        std::fs::write(problem.data_dir.join("checker.cpp"), "// version 6\n").unwrap();
        std::fs::write(problem.data_dir.join(".data-version"), "6\n").unwrap();
    }

    #[test]
    fn an_archive_problem_whose_helper_source_changed_during_grading_is_retried() {
        use judge_problem::data_version::FsStamps;
        let (root, cache) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let bridged = "checker: {name: bridged, args: {files: checker.cpp, lang: CPP17, type: testlib}}\n";
        let problem = archive_problem(root.path(), cache.path(), bridged);
        // Resolve took version 5; the solution compiles; meanwhile version 6 lands. check_of then reads
        // the checker source from data_dir (helper_program -> read_data_source), after the upload:
        upload_version_6(&problem);
        let checker = crate::artefacts::read_data_source(&problem, "checker.cpp").unwrap();
        assert_eq!(checker.bytes, b"// version 6\n", "version 5 tests are checked by the version 6 checker");
        // ... and the result is still stored as data version 5.
        assert_eq!(
            settle(Outcome::Graded, &FsStamps, &problem),
            Outcome::Retry(RetryReason::DataUnavailable),
            "a result graded with a checker read after the stamp moved must not be kept as version 5"
        );
        let missing = Outcome::InternalError("problem data i/o: checker.cpp missing".into());
        assert_eq!(
            settle(missing, &FsStamps, &problem),
            Outcome::Retry(RetryReason::DataUnavailable),
            "a checker removed by the upload is not an IE of version 5"
        );
    }

    #[test]
    fn submission_kinds_a_problem_cannot_grade_are_refused() {
        let registry =
            crate::languages::LanguageRegistry::from_toml(include_str!("../../../languages.toml")).unwrap();
        let refused =
            |key: &str, yaml: &str| refused_language(registry.get(key).unwrap(), &parse_init(yaml).unwrap());
        let output_only = "output_only: true\ntest_cases: []\n";
        let interactive = "test_cases: []\ninteractive: {files: i.cpp, type: testlib, lang: CPP17}\n";
        let signature = "test_cases: []\nsignature_grader: {entry: g.cpp, header: s.h}\n";
        assert_eq!(refused("CPP17", output_only), Some(OUTPUT_REQUIRED));
        assert_eq!(refused("OUTPUT", output_only), None);
        assert_eq!(refused("TEXT", output_only), None);
        assert_eq!(refused("TEXT", interactive), Some(PROGRAM_REQUIRED));
        assert_eq!(refused("OUTPUT", signature), Some(SIGNATURE_LANGUAGES));
        assert_eq!(refused("CPP17", interactive), None);
        assert_eq!(refused("TEXT", "test_cases: []\n"), None, "TEXT answers a plain problem");
    }

    #[test]
    fn readable_zips_are_told_from_garbage() {
        let mut bytes = Vec::new();
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(&mut bytes));
        writer.start_file("1.out", zip::write::SimpleFileOptions::default()).unwrap();
        writer.finish().unwrap();
        assert!(is_readable_zip(&bytes));
        assert!(!is_readable_zip(b"not a zip"));
        assert!(!is_readable_zip(&bytes[..bytes.len() - 4]), "a truncated directory");
    }
}
