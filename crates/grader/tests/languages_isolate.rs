//! Language findings LANG-1, LANG-3, LANG-4 (judging-bug hunt): compile/run behaviour in the real box.
#![cfg(feature = "isolate")]
mod common;

use common::{installed, language_or_skip, registry, sandbox, sandbox_on};
use judge_grader::compile::{BuildOutcome, CompileJob, build, program_for};
use judge_grader::error::GradeError;
use judge_grader::run_case::{CaseIo, CaseLimits, Runnable, run_case};
use judge_sandbox::classify::RunVerdict;

const LIMITS: CaseLimits = CaseLimits { time_limit_s: 2.0, memory_kib: 262_144, jvm_reserve_kib: 131_072 };

/// The common contest template: local runs read files, the online judge (ONLINE_JUDGE defined) uses stdio.
const ONLINE_JUDGE_TEMPLATE: &str = r#"#include <cstdio>
int main() {
#ifndef ONLINE_JUDGE
    freopen("input.txt", "r", stdin);
    freopen("output.txt", "w", stdout);
#endif
    int a, b;
    if (scanf("%d %d", &a, &b) != 2) return 0;
    printf("%d\n", a + b);
}
"#;

fn stdout_of(key: &str, offset: u32, source: &str, input: &str) -> Option<(RunVerdict, String)> {
    let language = language_or_skip(&registry(), key)?;
    let dirs: Vec<_> = (0..3).map(|_| tempfile::tempdir().unwrap()).collect();
    let mut sandbox = sandbox(offset, dirs[0].path());
    let job = CompileJob::for_language(&language, source.as_bytes());
    let outcome = build(&mut sandbox, &job, dirs[1].path()).unwrap();
    assert!(matches!(outcome, BuildOutcome::Built { .. }), "{key}: {outcome:?}");
    let program = program_for(&language, dirs[1].path(), source.as_bytes());
    let input_path = dirs[2].path().join("in");
    std::fs::write(&input_path, input).unwrap();
    let file = std::fs::File::open(&input_path).unwrap();
    let io = CaseIo { input: &file, output_cap: 1 << 20, file_io: None };
    let out = dirs[2].path().join("out");
    let runnable = Runnable { program: &program, limits: &LIMITS };
    let run = run_case(&mut sandbox, &runnable, &io, &out).unwrap();
    Some((run.verdict, std::fs::read_to_string(&out).unwrap_or_default()))
}

#[test]
fn c_and_cpp_are_compiled_with_online_judge_defined() {
    for (offset, key) in [(41, "CPP17"), (42, "CPP14"), (43, "CPP20"), (44, "CLANGX")] {
        let Some((verdict, stdout)) = stdout_of(key, offset, ONLINE_JUDGE_TEMPLATE, "1 2\n") else {
            continue;
        };
        assert_eq!(verdict, RunVerdict::Ok, "{key}");
        assert_eq!(stdout.trim(), "3", "{key}: ONLINE_JUDGE undefined, stdout went to output.txt");
    }
}

#[test]
fn a_pypy3_syntax_error_is_a_compile_error() {
    let mut language = registry().get("PYPY3").unwrap().clone();
    if !installed(&language) {
        // Without PyPy, python3 stands in: the shipped `-m py_compile` command is the same for both.
        language.compile.as_mut().expect("PYPY3 compiles")[0] = "/usr/bin/python3".into();
    }
    let (meta, out) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let job = CompileJob::for_language(&language, b"x = (\n");
    let outcome = build(&mut sandbox(45, meta.path()), &job, out.path()).unwrap();
    assert!(matches!(outcome, BuildOutcome::CompileError { .. }), "PYPY3 syntax error: {outcome:?}");
}

#[test]
fn a_compiler_missing_in_the_box_is_a_judge_error_not_a_compile_error() {
    let Some(mut language) = language_or_skip(&registry(), "CPP17") else { return };
    language.compile = Some(vec!["/usr/bin/no-such-compiler-in-box".into(), "main.cpp".into()]);
    let (meta, out) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let job = CompileJob::for_language(&language, b"int main(){}");
    let result = build(&mut sandbox(46, meta.path()), &job, out.path());
    assert!(
        matches!(result, Err(GradeError::RuntimeMissing(_))),
        "execve failure blamed on the user: {result:?}"
    );
}

const PRINT_PROCESSORS: &str = "public class Main { public static void main(String[] a) { \
    System.out.println(Runtime.getRuntime().availableProcessors()); } }\n";

#[test]
fn a_jvm_in_a_box_pinned_to_one_core_sees_one_processor_on_any_host() {
    // isolate's `time` sums every thread of the box, the JIT's too: the JVM must size them from the box's core.
    let language = registry().get("JAVA").unwrap().clone();
    let (snapshot, meta, io) =
        (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut program = program_for(&language, snapshot.path(), PRINT_PROCESSORS.as_bytes());
    if !std::path::Path::new(&program.run[0]).exists() {
        return eprintln!("skip: JAVA (runtime not installed on this host)");
    }
    // The source launcher stands in for javac (a JRE-only host): the shipped run flags, Main.java compiled in memory.
    *program.run.last_mut().unwrap() = "Main.java".into();
    std::fs::write(snapshot.path().join("Main.java"), PRINT_PROCESSORS).unwrap();
    let input = io.path().join("in");
    std::fs::write(&input, "").unwrap();
    let file = std::fs::File::open(&input).unwrap();
    let case_io = CaseIo { input: &file, output_cap: 1 << 20, file_io: None };
    let limits = CaseLimits { time_limit_s: 10.0, ..LIMITS };
    let out = io.path().join("out");
    let mut sandbox = sandbox_on(47, meta.path(), Some(0));
    let run =
        run_case(&mut sandbox, &Runnable { program: &program, limits: &limits }, &case_io, &out).unwrap();
    assert_eq!(run.verdict, RunVerdict::Ok);
    assert_eq!(std::fs::read_to_string(&out).unwrap().trim(), "1");
}
