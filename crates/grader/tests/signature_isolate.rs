#![cfg(feature = "isolate")]
//! Signature problems in C++: the contestant's functions linked with the problem's grader, in the box.
mod common;

use common::{registry, sandbox};
use judge_grader::checkers::standard::Standard;
use judge_grader::checkers::{CheckInput, Checker};
use judge_grader::compile::{BuildOutcome, build, program_for};
use judge_grader::run_case::{CaseIo, CaseLimits, Runnable, run_case};
use judge_grader::signature::signature_job;
use judge_problem::data_version::{FsStamps, WaitPolicy};
use judge_problem::dir_cache::DirCache;
use judge_problem::resolve::Resolver;
use judge_sandbox::classify::RunVerdict;
use std::sync::Arc;

const HEADER: &str = "long long add(long long a, long long b);\n";
const GRADER: &str = "#include <cstdio>\n#include \"sol.h\"\nint main(){long long a,b;std::scanf(\"%lld %lld\",&a,&b);std::printf(\"%lld\\n\",add(a,b));}\n";

enum Outcome {
    CompileError(String),
    Run(RunVerdict, bool),
}

fn problem_dir(root: &std::path::Path, allow_main: bool) {
    let dir = root.join("sig");
    std::fs::create_dir_all(&dir).unwrap();
    let init = format!(
        "test_cases:\n- {{in: 1.in, out: 1.out, points: 1}}\nsignature_grader: {{entry: grader.cpp, header: sol.h, allow_main: {allow_main}}}\n"
    );
    for (name, text) in [
        ("init.yml", init.as_str()),
        ("sol.h", HEADER),
        ("grader.cpp", GRADER),
        ("1.in", "1 2\n"),
        ("1.out", "3\n"),
    ] {
        std::fs::write(dir.join(name), text).unwrap();
    }
}

fn judge(offset: u32, source: &str, allow_main: bool) -> Outcome {
    let dirs: Vec<_> = (0..4).map(|_| tempfile::tempdir().unwrap()).collect();
    problem_dir(dirs[0].path(), allow_main);
    let resolver = Resolver {
        root: dirs[0].path().into(),
        cache: Arc::new(DirCache::new(dirs[1].path().into(), 1 << 30)),
        wait: WaitPolicy { timeout: Default::default(), poll: Default::default() },
        zip: Default::default(),
        stamps: Arc::new(FsStamps),
    };
    let problem = resolver.resolve("sig", 0).unwrap();
    let registry = registry();
    let language = registry.get("CPP17").unwrap();
    let spec = problem.init.signature_grader.clone().unwrap();
    let job = signature_job(language, &problem, &spec, source.as_bytes()).unwrap().unwrap();
    let mut sandbox = sandbox(offset, dirs[2].path());
    let program_dir = dirs[3].path().join("program");
    if let BuildOutcome::CompileError { log } = build(&mut sandbox, &job, &program_dir).unwrap() {
        return Outcome::CompileError(log);
    }
    let program = program_for(language, &program_dir, source.as_bytes());
    let limits = CaseLimits { time_limit_s: 1.0, memory_kib: 65_536, jvm_reserve_kib: 0 };
    let input = problem.open_case_file("1.in").unwrap();
    let io = CaseIo { input: &input, output_cap: 1 << 20, file_io: None };
    let output = dirs[3].path().join("out");
    let run = run_case(&mut sandbox, &Runnable { program: &program, limits: &limits }, &io, &output).unwrap();
    let answer = dirs[1].path().join("ans");
    std::fs::write(&answer, "3\n").unwrap();
    let io = CheckInput { input: &answer, output: &output, answer: &answer };
    let passed = Standard { unicode: false }.check(&io).unwrap().passed;
    Outcome::Run(run.verdict, passed)
}

fn compile_log(outcome: Outcome) -> String {
    match outcome {
        Outcome::CompileError(log) => log,
        Outcome::Run(verdict, passed) => panic!("expected a compile error, ran: {verdict:?} passed={passed}"),
    }
}

const ADD: &str = "long long add(long long a, long long b) { return a + b; }\n";

#[test]
fn user_functions_link_with_the_grader_and_pass() {
    assert!(matches!(judge(12, ADD, false), Outcome::Run(RunVerdict::Ok, true)));
}

#[test]
fn a_wrong_function_is_a_wrong_answer() {
    let wrong = "long long add(long long a, long long b) { return a - b; }\n";
    assert!(matches!(judge(13, wrong, false), Outcome::Run(RunVerdict::Ok, false)));
}

#[test]
fn a_contestant_main_is_renamed_unless_allowed() {
    let with_main = format!("{ADD}int main() {{ return 1; }}\n");
    assert!(matches!(judge(14, &with_main, false), Outcome::Run(RunVerdict::Ok, true)));
    assert!(compile_log(judge(15, &with_main, true)).contains("main"), "two mains do not link");
}

#[test]
fn a_missing_function_is_a_compile_error() {
    assert!(compile_log(judge(16, "int unrelated;\n", false)).contains("add"));
}

#[test]
fn the_grader_sources_are_not_in_the_run_box() {
    let peek = "#include <cstdio>\nlong long add(long long a, long long b) {\n  std::FILE* f = std::fopen(\"grader.cpp\", \"r\"); std::FILE* h = std::fopen(\"sol.h\", \"r\");\n  return (f || h) ? 0 : a + b;\n}\n";
    assert!(matches!(judge(17, peek, false), Outcome::Run(RunVerdict::Ok, true)), "read the grader's files");
}

#[test]
fn contestant_code_cannot_pull_the_grader_into_its_binary() {
    let incbin = format!("{ADD}__asm__(\".incbin \\\"grader.cpp\\\"\");\n");
    assert!(compile_log(judge(18, &incbin, false)).contains("grader.cpp"), "grader.cpp was readable");
    let include = format!("#include \"grader.cpp\"\n{ADD}");
    assert!(compile_log(judge(19, &include, false)).contains("grader.cpp"));
}
