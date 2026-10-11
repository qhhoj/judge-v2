#![cfg(feature = "isolate")]
//! Test generation end to end in real boxes: a fixture bundle with a generator that includes a header
//! beside it, a testlib validator and a main solution.
mod common;

use common::{artefacts, sandbox};
use judge_grader::generate::{
    Control, GenerationFailure, GenerationJob, GenerationPlan, Kind, PlanLimits, PlanProgram, PlanTest,
    ProgramLimits, Role, Stage, generate_tests,
};
use judge_grader::judge::{GraderDefaults, JudgeEnv};
use judge_problem::data_version::FsStamps;
use std::io::{Read, Write};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/generate");
const MEBIBYTE: u64 = 1 << 20;

struct Bench {
    dirs: tempfile::TempDir,
    env: JudgeEnv,
}

fn bench(offset: u32) -> Bench {
    let dirs = tempfile::tempdir().unwrap();
    let make = |name: &str| {
        let path = dirs.path().join(name);
        std::fs::create_dir_all(&path).unwrap();
        path
    };
    let env = JudgeEnv {
        solution: sandbox(offset, &make("meta")),
        helper: sandbox(offset + 1, &make("meta")),
        artefacts: Arc::new(artefacts(&make("cache"))),
        stamps: Arc::new(FsStamps),
        scratch: make("scratch"),
        defaults: GraderDefaults {
            default_output_limit: MEBIBYTE,
            default_output_prefix: 64,
            jvm_reserve_kib: 131_072,
        },
    };
    Bench { dirs, env }
}

/// The fixture directory as a bundle zip.
fn bundle_zip(into: &Path) -> std::path::PathBuf {
    fn add(writer: &mut zip::ZipWriter<std::fs::File>, root: &Path, dir: &Path) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                add(writer, root, &path);
                continue;
            }
            let name = path.strip_prefix(root).unwrap().to_str().unwrap();
            writer.start_file(name, zip::write::SimpleFileOptions::default()).unwrap();
            writer.write_all(&std::fs::read(&path).unwrap()).unwrap();
        }
    }
    let path = into.join("bundle.zip");
    let mut writer = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
    add(&mut writer, Path::new(FIXTURE), Path::new(FIXTURE));
    writer.finish().unwrap();
    path
}

fn program(name: &str, role: Role, source: &str) -> PlanProgram {
    PlanProgram { name: name.into(), role, language_key: "CPP17".into(), source_path: source.into() }
}

fn test(index: u32, source: (&str, &[&str])) -> PlanTest {
    let (generator, args) = source;
    let manual = generator.starts_with("tests/");
    PlanTest {
        index,
        input_name: format!("{index:02}.inp"),
        answer_name: format!("{index:02}.out"),
        group: if index == 1 { String::new() } else { "1".into() },
        manual_path: if manual { generator.into() } else { String::new() },
        generator: if manual { String::new() } else { generator.into() },
        args: args.iter().map(|arg| arg.to_string()).collect(),
    }
}

fn plan(tests: Vec<PlanTest>) -> GenerationPlan {
    let limits = ProgramLimits { time_s: 1.0, wall_s: 3.0, memory_kib: 256 * 1024 };
    GenerationPlan {
        id: "fixture".into(),
        bundle_size: 1,
        deadline_s: 120,
        limits: PlanLimits {
            generator: limits,
            validator: limits,
            solution: limits,
            max_file_bytes: MEBIBYTE,
            max_total_bytes: 4 * MEBIBYTE,
            max_upload_bytes: 4 * MEBIBYTE,
        },
        programs: vec![
            program("gen", Role::Generator, "files/gen.cpp"),
            program("validator", Role::Validator, "files/validator.cpp"),
            program("sol", Role::Solution, "solutions/sol.cpp"),
        ],
        tests,
        file_io: None,
    }
}

fn run(bench: &mut Bench, plan: GenerationPlan) -> Result<Vec<(String, String)>, GenerationFailure> {
    run_with(bench, plan, &AtomicBool::new(false))
}

/// The result zip's entries as (name, text).
fn run_with(
    bench: &mut Bench,
    plan: GenerationPlan,
    abort: &AtomicBool,
) -> Result<Vec<(String, String)>, GenerationFailure> {
    let job = GenerationJob {
        plan,
        bundle_zip: bundle_zip(bench.dirs.path()),
        result_zip: bench.dirs.path().join("result.zip"),
    };
    let control = Control { deadline: Instant::now() + Duration::from_secs(120), abort };
    generate_tests(&mut bench.env, &job, &control)?;
    let mut archive = zip::ZipArchive::new(std::fs::File::open(&job.result_zip).unwrap()).unwrap();
    let mut entries = Vec::new();
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).unwrap();
        let mut text = String::new();
        entry.read_to_string(&mut text).unwrap();
        entries.push((entry.name().to_string(), text));
    }
    Ok(entries)
}

#[test]
fn manual_and_generated_tests_get_their_inputs_and_answers_under_their_entry_names() {
    let mut bench = bench(40);
    let tests = vec![
        test(1, ("tests/01", &[])),
        test(2, ("gen", &["4", "5"])),
        test(3, ("gen", &["4", "5"])), // the same command again: the same input
        test(4, ("gen", &["1", "1"])),
    ];
    let entries = run(&mut bench, plan(tests)).unwrap();
    let expected = [
        ("01.inp", "1 2\n"),
        ("01.out", "3\n"),
        ("02.inp", "8 5\n"),
        ("02.out", "13\n"),
        ("03.inp", "8 5\n"),
        ("03.out", "13\n"),
        ("04.inp", "2 1\n"),
        ("04.out", "3\n"),
    ];
    let seen: Vec<_> = entries.iter().map(|(name, text)| (name.as_str(), text.as_str())).collect();
    assert_eq!(seen, expected);
}

#[test]
fn a_test_the_validator_rejects_stops_the_job_with_its_index_and_message() {
    let mut bench = bench(42);
    let tests = vec![test(1, ("gen", &["2", "3"])), test(2, ("gen", &["60", "3"]))]; // 120 > 100
    let failure = run(&mut bench, plan(tests)).unwrap_err();
    assert_eq!((failure.stage, failure.kind), (Stage::Validate, Kind::Rejected));
    assert_eq!((failure.program.as_str(), failure.test_index), ("validator", 2));
    assert!(failure.detail.contains("120"), "{failure:?}");
}

#[test]
fn a_generator_that_never_ends_is_a_time_limit_and_one_that_fails_is_a_crash_with_its_message() {
    let mut bench = bench(44);
    let looping = run(&mut bench, plan(vec![test(1, ("gen", &["loop"]))])).unwrap_err();
    assert_eq!((looping.stage, looping.kind), (Stage::Generate, Kind::TimeLimit));
    assert_eq!((looping.program.as_str(), looping.test_index), ("gen", 1));
    let failing =
        run(&mut bench, plan(vec![test(1, ("gen", &["2", "3"])), test(2, ("gen", &["fail"]))])).unwrap_err();
    assert_eq!((failing.kind, failing.test_index), (Kind::Crashed, 2));
    assert_eq!(failing.detail, "exit code 3: FAIL bad arguments");
}

#[test]
fn a_crashing_solution_fails_at_its_test() {
    let mut bench = bench(46);
    let failure =
        run(&mut bench, plan(vec![test(1, ("gen", &["2", "3"])), test(2, ("tests/02", &[]))])).unwrap_err();
    assert_eq!((failure.stage, failure.kind), (Stage::Solve, Kind::Crashed));
    assert_eq!((failure.program.as_str(), failure.test_index), ("sol", 2));
    assert_eq!(failure.detail, "signal SIGSEGV");
}

#[test]
fn a_source_that_does_not_compile_reports_the_compiler_log() {
    let mut bench = bench(48);
    let mut broken = plan(vec![test(1, ("gen", &["2", "3"]))]);
    broken.programs[0].source_path = "tests/01".into(); // "1 2\n" is not C++
    let failure = run(&mut bench, broken).unwrap_err();
    assert_eq!(
        (failure.stage, failure.kind, failure.program.as_str()),
        (Stage::Compile, Kind::CompileError, "gen")
    );
    assert!(!failure.detail.is_empty());
    let mut unknown = plan(vec![test(1, ("gen", &["2", "3"]))]);
    unknown.programs[0].language_key = "NOPE".into();
    assert_eq!(run(&mut bench, unknown).unwrap_err().kind, Kind::UnsupportedLanguage);
    let mut missing = plan(vec![test(1, ("gen", &["2", "3"]))]);
    missing.programs[0].source_path = "files/none.cpp".into();
    assert_eq!(run(&mut bench, missing).unwrap_err().kind, Kind::MissingFile);
}

#[test]
fn the_total_cap_and_a_cancel_stop_the_job() {
    let mut bench = bench(50);
    let mut small = plan(vec![test(1, ("gen", &["2", "3"])), test(2, ("gen", &["4", "3"]))]);
    small.limits.max_total_bytes = 9; // the two inputs take 8 bytes, the first answer crosses 9
    let failure = run(&mut bench, small).unwrap_err();
    assert_eq!((failure.stage, failure.kind), (Stage::Job, Kind::OutputLimit));
    let aborted = AtomicBool::new(true);
    let cancelled = run_with(&mut bench, plan(vec![test(1, ("gen", &["2", "3"]))]), &aborted).unwrap_err();
    assert!(cancelled.is_cancelled());
}

fn compile_check(programs: Vec<PlanProgram>) -> GenerationPlan {
    GenerationPlan { programs, ..plan(vec![]) }
}

#[test]
fn a_compile_check_of_a_good_testlib_checker_succeeds_with_an_empty_result() {
    let mut bench = bench(54);
    let good = compile_check(vec![program("checker", Role::Checker, "files/checker.cpp")]);
    assert!(run(&mut bench, good).unwrap().is_empty());
}

#[test]
fn a_compile_check_names_the_program_that_does_not_compile_and_its_log() {
    let mut bench = bench(56);
    let broken = compile_check(vec![
        program("checker", Role::Checker, "files/checker.cpp"),
        program("bad", Role::Interactor, "files/broken_checker.cpp"),
    ]);
    let failure = run(&mut bench, broken).unwrap_err();
    assert_eq!(
        (failure.stage, failure.kind, failure.program.as_str()),
        (Stage::Compile, Kind::CompileError, "bad")
    );
    assert!(failure.detail.contains("undeclared_name_in_checker"), "{}", failure.detail);
}
