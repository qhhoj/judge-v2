#![cfg(feature = "isolate")]
mod common;

use common::{language_or_skip, registry, sandbox};
use judge_grader::compile::{CompileJob, build, program_for};
use judge_grader::languages::Language;
use judge_grader::program::Program;
use judge_grader::run_case::{CaseIo, CaseLimits, CaseRun, Runnable, run_case};
use judge_problem::init::FileIo;
use judge_sandbox::IsolateBox;
use judge_sandbox::classify::RunVerdict;
use std::fs::File;
use std::path::PathBuf;

struct Fixture {
    _dirs: Vec<tempfile::TempDir>,
    sandbox: IsolateBox,
    program: Program,
    scratch: PathBuf,
}

fn fixture_with(offset: u32, language: &Language, source: &str) -> Fixture {
    let dirs: Vec<_> = (0..3).map(|_| tempfile::tempdir().unwrap()).collect();
    let mut sandbox = sandbox(offset, dirs[0].path());
    let job = CompileJob::for_language(language, source.as_bytes());
    build(&mut sandbox, &job, dirs[1].path()).unwrap();
    let program = program_for(language, dirs[1].path(), source.as_bytes());
    let scratch = dirs[2].path().to_path_buf();
    Fixture { _dirs: dirs, sandbox, program, scratch }
}

fn fixture(offset: u32, key: &str, source: &str) -> Option<Fixture> {
    let language = language_or_skip(&registry(), key)?;
    Some(fixture_with(offset, &language, source))
}

const LIMITS: CaseLimits = CaseLimits { time_limit_s: 1.0, memory_kib: 65_536, jvm_reserve_kib: 131_072 };

fn input(f: &Fixture, text: &str) -> File {
    let path = f.scratch.join("in");
    std::fs::write(&path, text).unwrap();
    File::open(path).unwrap()
}

fn go(f: &mut Fixture, io: &CaseIo, name: &str) -> (CaseRun, PathBuf) {
    let out = f.scratch.join(name);
    let runnable = Runnable { program: &f.program, limits: &LIMITS };
    (run_case(&mut f.sandbox, &runnable, io, &out).unwrap(), out)
}

fn plain(file: &File, cap: u64) -> CaseIo<'_> {
    CaseIo { input: file, output_cap: cap, file_io: None }
}

#[test]
fn stdin_to_stdout() {
    let Some(mut f) = fixture(0, "PY3", "a, b = map(int, input().split())\nprint(a + b)\n") else { return };
    let file = input(&f, "1 2\n");
    let (run, out) = go(&mut f, &plain(&file, 1 << 20), "out");
    assert_eq!(run.verdict, RunVerdict::Ok);
    assert_eq!(std::fs::read_to_string(&out).unwrap(), "3\n");
    assert_eq!(run.output_len, 2);
}

#[test]
fn file_io_reads_and_writes_named_files() {
    let src = "a, b = map(int, open('sum.inp').read().split())\nopen('sum.out', 'w').write(str(a + b))\n";
    let Some(mut f) = fixture(1, "PY3", src) else { return };
    let file_io = FileIo { input: "sum.inp".into(), output: "sum.out".into() };
    let file = input(&f, "4 5");
    let io = CaseIo { input: &file, output_cap: 1 << 20, file_io: Some(&file_io) };
    let (run, out) = go(&mut f, &io, "out");
    assert_eq!(run.verdict, RunVerdict::Ok);
    assert_eq!(std::fs::read_to_string(&out).unwrap(), "9");
}

#[test]
fn a_file_io_name_that_leaves_the_box_never_runs() {
    let Some(mut f) = fixture(2, "PY3", "print(1)\n") else { return };
    let file_io = FileIo { input: "../x".into(), output: "o".into() };
    let file = input(&f, "");
    let io = CaseIo { input: &file, output_cap: 100, file_io: Some(&file_io) };
    let runnable = Runnable { program: &f.program, limits: &LIMITS };
    assert!(run_case(&mut f.sandbox, &runnable, &io, &f.scratch.join("o")).is_err());
}

#[test]
fn a_program_that_makes_the_output_a_symlink_has_an_empty_output() {
    let src = "import os\nos.symlink('/etc/hostname', 'output')\n";
    let Some(mut f) = fixture(3, "PY3", src) else { return };
    let file = input(&f, "");
    let (run, out) = go(&mut f, &plain(&file, 1 << 20), "out");
    assert_eq!(run.output_len, 0);
    assert_eq!(std::fs::read(&out).unwrap(), b"");
}

#[test]
fn python_flood_is_ole() {
    let Some(mut f) = fixture(4, "PY3", "while True: print('x' * 1000)\n") else { return };
    let file = input(&f, "");
    let (run, _) = go(&mut f, &plain(&file, 8192), "out");
    assert_eq!(run.verdict, RunVerdict::Ole);
}

#[test]
fn output_of_exactly_the_cap_is_not_ole() {
    let Some(mut f) = fixture(5, "PY3", "print('x' * 99)\n") else { return };
    let file = input(&f, "");
    let (run, _) = go(&mut f, &plain(&file, 100), "out");
    assert_eq!((run.verdict, run.output_len), (RunVerdict::Ok, 100));
}

#[test]
fn second_case_after_mle_is_clean() {
    let src = "import sys\nif sys.stdin.read().strip() == 'big':\n    a = bytearray(200_000_000)\nprint(1)\n";
    let Some(mut f) = fixture(6, "PY3", src) else { return };
    let big = input(&f, "big");
    let (first, _) = go(&mut f, &plain(&big, 1 << 20), "o1");
    assert_eq!(first.verdict, RunVerdict::Mle);
    let small = input(&f, "small");
    let (second, _) = go(&mut f, &plain(&small, 1 << 20), "o2");
    assert_eq!(second.verdict, RunVerdict::Ok);
    assert!(second.memory_kib < first.memory_kib);
}

#[test]
fn a_busy_loop_is_tle() {
    let Some(mut f) = fixture(7, "PY3", "while True: pass\n") else { return };
    let file = input(&f, "");
    let (run, _) = go(&mut f, &plain(&file, 1 << 20), "out");
    assert_eq!(run.verdict, RunVerdict::Tle);
}

#[test]
fn an_uncaught_python_exception_is_rte_named_after_it() {
    let Some(mut f) = fixture(8, "PY3", "print(1 // 0)\n") else { return };
    let file = input(&f, "");
    let (run, _) = go(&mut f, &plain(&file, 1 << 20), "out");
    assert_eq!(run.verdict, RunVerdict::Rte { reason: "ZeroDivisionError".into() });
}

#[test]
fn a_plain_non_zero_exit_stays_ir() {
    let Some(mut f) = fixture(9, "PY3", "import sys\nsys.exit(3)\n") else { return };
    let file = input(&f, "");
    let (run, _) = go(&mut f, &plain(&file, 1 << 20), "out");
    assert_eq!(run.verdict, RunVerdict::Ir { exit_code: 3 });
}

#[test]
fn a_native_crash_is_rte_by_signal() {
    let Some(mut f) = fixture(10, "CPP17", "int main(){ int *p = 0; return *p; }") else { return };
    let file = input(&f, "");
    let (run, _) = go(&mut f, &plain(&file, 1 << 20), "out");
    assert_eq!(run.verdict, RunVerdict::Rte { reason: "SIGSEGV".into() });
}

/// JAVA with the JRE-only layout of this host: javac is reached through the compiler module.
fn java_language() -> Option<Language> {
    let java = "/usr/lib/jvm/default-java/bin/java";
    if !std::path::Path::new(java).exists() {
        eprintln!("skip: JAVA (no JVM on this host)");
        return None;
    }
    let mut language = registry().get("JAVA").unwrap().clone();
    let javac = [java, "-m", "jdk.compiler/com.sun.tools.javac.Main", "-encoding", "UTF-8", "{class}.java"];
    language.compile = Some(javac.map(String::from).to_vec());
    language.compile_processes = 128;
    Some(language)
}

#[test]
fn java_uncaught_exception_is_rte_and_out_of_memory_is_mle() {
    let Some(language) = java_language() else { return };
    let boom =
        "public class Main { public static void main(String[] a) { int x = 0; System.out.println(1 / x); } }";
    let mut f = fixture_with(11, &language, boom);
    let file = input(&f, "");
    let (run, _) = go(&mut f, &plain(&file, 1 << 20), "out");
    assert_eq!(run.verdict, RunVerdict::Rte { reason: "ArithmeticException".into() });
    let hog = "public class Main { public static void main(String[] a) { long[] x = new long[200_000_000]; System.out.println(x.length); } }";
    let mut g = fixture_with(12, &language, hog);
    let (run, _) = go(&mut g, &plain(&file, 1 << 20), "out");
    assert_eq!(run.verdict, RunVerdict::Mle);
}

#[test]
fn the_input_comes_from_the_opened_handle_not_from_its_path() {
    let Some(mut f) = fixture(13, "PY3", "print(input())\n") else { return };
    let file = input(&f, "original\n");
    let path = f.scratch.join("in");
    std::fs::remove_file(&path).unwrap();
    std::fs::write(&path, "swapped\n").unwrap();
    let (run, out) = go(&mut f, &plain(&file, 1 << 20), "out");
    assert_eq!(run.verdict, RunVerdict::Ok);
    assert_eq!(std::fs::read_to_string(&out).unwrap(), "original\n");
}
