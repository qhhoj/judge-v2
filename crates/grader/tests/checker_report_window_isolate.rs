#![cfg(feature = "isolate")]
//! Checkers and interactors that write more than the report window (64 KiB) to the stream their verdict is
//! on: each keeps the verdict it printed, read from the end of the stream where the protocol puts it.
//! The verdict never comes from free text the program printed (testlib's quit message goes to a result file
//! the judge creates; only a program that leaves it empty may end stderr with `points <number>`). Boxes:
//! offsets 0-8.
mod common;

use common::{artefacts, cpp_helper, registry, sandbox};
use judge_grader::checkers::custom::{CustomChecker, PartialPoints};
use judge_grader::checkers::select::CustomKind;
use judge_grader::checkers::themis::{ThemisNames, names_for};
use judge_grader::checkers::{CheckInput, CheckOutcome};
use judge_grader::compile::{CompileJob, build, program_for};
use judge_grader::error::GradeError;
use judge_grader::interactor::{Boxes, Interaction, Interactive, interact};
use judge_grader::run_case::{CaseLimits, Runnable};
use judge_sandbox::IsolateBox;
use std::path::Path;

/// Lines enough to take each stream well past the window.
const MANY: usize = 40_000;

struct Fixture {
    _dirs: Vec<tempfile::TempDir>,
    sandbox: IsolateBox,
    checker: CustomChecker,
    scratch: std::path::PathBuf,
}

fn fixture(offset: u32, kind: CustomKind, source: &str) -> Fixture {
    let dirs: Vec<_> = (0..3).map(|_| tempfile::tempdir().unwrap()).collect();
    let mut sandbox = sandbox(offset, dirs[0].path());
    let program = artefacts(dirs[1].path()).program(&mut sandbox, &cpp_helper(source)).unwrap();
    let scratch = dirs[2].path().to_path_buf();
    let themis_names =
        if kind == CustomKind::Themis { names_for(None, source.as_bytes()) } else { ThemisNames::default() };
    let checker =
        CustomChecker { program, kind, partial: PartialPoints::Absolute, themis_names, time_limit_s: 1.0 };
    Fixture { checker, _dirs: dirs, sandbox, scratch }
}

fn judge(f: &mut Fixture, answer: &str, output: &str) -> Result<CheckOutcome, GradeError> {
    let at = |name: &str| -> std::path::PathBuf { Path::new(&f.scratch).join(name) };
    for (name, text) in [("in", ""), ("ans", answer), ("out", output)] {
        std::fs::write(at(name), text).unwrap();
    }
    let io = CheckInput { input: &at("in"), output: &at("out"), answer: &at("ans"), points: 1.0 };
    f.checker.check(&mut f.sandbox, &io)
}

fn assert_wa(outcome: &CheckOutcome, what: &str) {
    assert!(!outcome.passed && outcome.fraction == 0.0, "{what}: {outcome:?}");
}

/// A themis checker that lists each wrong value it found, then prints the score 0.
const THEMIS_LISTS_WRONG_VALUES: &str = r#"#include <cstdio>
int main() {
    char test[512], work[512], path[1024];
    if (scanf("%511s %511s", test, work) != 2) return 3;
    snprintf(path, sizeof path, "%s/%s", test, "arr.inp");
    if (!fopen(path, "r")) return 3;
    snprintf(path, sizeof path, "%s/%s", test, "arr.out");
    FILE* ans = fopen(path, "r");
    snprintf(path, sizeof path, "%s/%s", work, "arr.out");
    FILE* out = fopen(path, "r");
    if (!ans || !out) return 3;
    long long want, got; int wrong = 0;
    while (fscanf(ans, "%lld", &want) == 1) {
        if (fscanf(out, "%lld", &got) != 1) { printf("Thieu output\n0\n"); return 0; }
        if (got != want) { if (!wrong) printf("Sai cac gia tri:\n"); printf("%lld\n", got); wrong++; }
    }
    if (wrong) printf("0\n"); else printf("Dung\n1\n");
    return 0;
}
"#;

#[test]
fn a_themis_score_after_a_long_stdout_is_read() {
    let mut f = fixture(0, CustomKind::Themis, THEMIS_LISTS_WRONG_VALUES);
    assert_wa(&judge(&mut f, "2\n2\n", "1\n1\n").unwrap(), "short");
    assert_wa(
        &judge(&mut f, &"2\n".repeat(MANY), &"1\n".repeat(MANY)).unwrap(),
        "the score 0 is printed last",
    );
    let right = judge(&mut f, &"2\n".repeat(MANY), &"2\n".repeat(MANY)).unwrap();
    assert!(right.passed && right.fraction == 1.0, "{right:?}");
}

/// A testlib checker that writes each output token to stderr and scores with `quitp`.
const TESTLIB_POINTS_AFTER_TRACE: &str = r#"#include "testlib.h"
int main(int argc, char* argv[]) {
    registerTestlibCmd(argc, argv);
    int good = 0, total = 0;
    while (!ans.seekEof()) {
        long long want = ans.readLong();
        long long got = ouf.readLong();
        std::cerr << got << "\n";
        total++;
        if (got == want) good++;
    }
    quitp(good == total ? 1.0 : 0.1, "graded");
}
"#;

#[test]
fn testlib_points_after_a_long_stderr_are_the_points_printed() {
    let mut f = fixture(1, CustomKind::Testlib, TESTLIB_POINTS_AFTER_TRACE);
    let short = judge(&mut f, "7\n7\n", "100\n7\n").unwrap();
    assert!((short.fraction - 0.1).abs() < 1e-9, "short: {short:?}");
    let output = format!("100\n{}", "7\n".repeat(MANY - 1));
    let long = judge(&mut f, &"7\n".repeat(MANY), &output).unwrap();
    assert!((long.fraction - 0.1).abs() < 1e-9 && long.feedback == "graded", "long: {long:?}");
}

/// `_pc(0)` (no points, exit code 0) after a long stderr trace.
const TESTLIB_PC0_AFTER_TRACE: &str = r#"#include "testlib.h"
int main(int argc, char* argv[]) {
    registerTestlibCmd(argc, argv);
    while (!ouf.seekEof()) std::cerr << ouf.readToken() << "\n";
    quitf(_pc(0), "nothing earned");
}
"#;

#[test]
fn testlib_pc0_after_a_long_stderr_is_a_wrong_answer() {
    let mut f = fixture(2, CustomKind::Testlib, TESTLIB_PC0_AFTER_TRACE);
    assert_wa(&judge(&mut f, "x\n", "a\n").unwrap(), "short");
    assert_wa(&judge(&mut f, "x\n", &"a\n".repeat(MANY)).unwrap(), "long");
}

/// A default checker gives its share first on stdout (exit 2), then a long listing.
const DEFAULT_SHARE_THEN_LISTING: &str = r#"#include <cstdio>
int main(int argc, char* argv[]) {
    FILE* out = fopen(argv[2], "r");
    printf("0.5\n");
    char token[64];
    while (out && fscanf(out, "%63s", token) == 1) printf("%s\n", token);
    return 2;
}
"#;

#[test]
fn a_default_share_before_a_long_stdout_is_read() {
    let mut f = fixture(3, CustomKind::Default, DEFAULT_SHARE_THEN_LISTING);
    let long = judge(&mut f, "", &"1\n".repeat(MANY)).unwrap();
    assert!(long.passed && (long.fraction - 0.5).abs() < 1e-9, "{long:?}");
}

/// A testlib interactor that logs each query to stderr, on one line, and scores with `quitp`.
const INTERACTOR_POINTS_AFTER_LOG: &str = r#"#include "testlib.h"
int main(int argc, char* argv[]) {
    registerInteraction(argc, argv);
    int good = 0, total = 0;
    while (true) {
        std::string query = ouf.readToken();
        if (query == "end") break;
        std::cerr << query << ' ';
        total++;
        if (query == "7") good++;
    }
    quitp(good == total ? 1.0 : 0.1, "graded");
}
"#;

/// Interacts with a solution that asks `100`, then `sevens` times `7`, then `end`.
fn interact_once(offset: u32, sevens: usize) -> Result<Interactive, GradeError> {
    let dirs: Vec<_> = (0..4).map(|_| tempfile::tempdir().unwrap()).collect();
    let mut solution = sandbox(offset, dirs[0].path());
    let mut helper = sandbox(offset + 1, dirs[0].path());
    let interactor =
        artefacts(dirs[1].path()).program(&mut helper, &cpp_helper(INTERACTOR_POINTS_AFTER_LOG)).unwrap();
    let (input, answer) = (dirs[3].path().join("input"), dirs[3].path().join("answer"));
    std::fs::write(&input, "").unwrap();
    std::fs::write(&answer, "").unwrap();
    let source = format!(
        "#include <cstdio>\nint main(){{ puts(\"100\"); for (int i = 0; i < {sevens}; i++) puts(\"7\"); puts(\"end\"); }}\n"
    );
    let registry = registry();
    let language = registry.get("CPP17").unwrap();
    let dir = dirs[2].path().join("program");
    build(&mut solution, &CompileJob::for_language(language, source.as_bytes()), &dir).unwrap();
    let program = program_for(language, &dir, source.as_bytes());
    let limits = CaseLimits { time_limit_s: 2.0, memory_kib: 262_144, jvm_reserve_kib: 0 };
    let runnable = Runnable { program: &program, limits: &limits };
    let x = Interaction {
        solution: &runnable,
        interactor: &interactor,
        input: &input,
        answer: &answer,
        points: 1.0,
        partial: PartialPoints::Absolute,
    };
    interact(Boxes { solution: &mut solution, helper: &mut helper }, &x)
}

fn interactor_fraction(result: &Result<Interactive, GradeError>) -> Option<f64> {
    result.as_ref().ok()?.check.as_ref().map(|check| check.fraction)
}

#[test]
fn interactor_points_after_a_long_stderr_are_the_points_printed() {
    let short = interact_once(4, 1);
    assert!(interactor_fraction(&short).is_some_and(|f| (f - 0.1).abs() < 1e-9), "short: {short:?}");
    let long = interact_once(4, MANY);
    assert!(interactor_fraction(&long).is_some_and(|f| (f - 0.1).abs() < 1e-9), "long: {long:?}");
}

/// Traces the output without a newline before quitting, or quits with a message that holds an output line
/// after a newline; the answer file picks which and the verdict.
const TESTLIB_TRACE_AND_MESSAGE: &str = r#"#include "testlib.h"
int main(int argc, char* argv[]) {
    registerTestlibCmd(argc, argv);
    std::string mode = ans.readToken();
    if (mode == "trace-pc0" || mode == "trace-p01") {
        while (!ouf.seekEof()) std::cerr << ouf.readToken() << ' ';
        if (mode == "trace-pc0") quitf(_pc(0), "nothing earned");
        quitp(0.1, "graded");
    }
    std::string line = ouf.readLine();
    if (mode == "message-pc0") quitf(_pc(0), "got:\n%s", line.c_str());
    quitp(0.1, "got:\n%s", line.c_str());
}
"#;

#[test]
fn a_trace_on_the_verdicts_line_does_not_change_the_verdict() {
    let mut f = fixture(6, CustomKind::Testlib, TESTLIB_TRACE_AND_MESSAGE);
    assert_wa(&judge(&mut f, "trace-pc0", "a b c\n").unwrap(), "_pc(0) after a trace");
    for output in ["100 7 7\n", "points 1 7\n"] {
        let points = judge(&mut f, "trace-p01", output).unwrap();
        assert!((points.fraction - 0.1).abs() < 1e-9, "{output:?}: {points:?}");
    }
}

#[test]
fn an_output_line_in_the_quit_message_does_not_change_the_verdict() {
    let mut f = fixture(6, CustomKind::Testlib, TESTLIB_TRACE_AND_MESSAGE);
    let points = judge(&mut f, "message-p01", "points 1 x\n").unwrap();
    assert!((points.fraction - 0.1).abs() < 1e-9, "{points:?}");
    assert_wa(&judge(&mut f, "message-pc0", "ok x\n").unwrap(), "_pc(0) with an output line");
}

#[test]
fn a_result_file_among_the_checkers_own_files_is_refused() {
    let mut f = fixture(7, CustomKind::Testlib, TESTLIB_PC0_AFTER_TRACE);
    let forged = f.scratch.join("forged");
    std::fs::create_dir(&forged).unwrap();
    for entry in std::fs::read_dir(&f.checker.program.dir).unwrap() {
        let entry = entry.unwrap();
        std::fs::copy(entry.path(), forged.join(entry.file_name())).unwrap();
    }
    std::fs::write(forged.join(judge_grader::checkers::testlib::RESULT_FILE), "").unwrap();
    f.checker.program = judge_grader::program::Program { dir: forged, ..f.checker.program.clone() };
    let refused = judge(&mut f, "x\n", "a\n");
    assert!(matches!(&refused, Err(GradeError::Checker(m)) if m.contains("result file")), "{refused:?}");
}

/// A hand-written checker declared testlib (no testlib.h): it never writes the result file and gives
/// partial points as its last stderr line, then exits 7.
const HAND_WRITTEN_POINTS: &str = r#"#include <iostream>
int main() {
    std::cerr << "partial" << std::endl;
    std::cerr << "points 0.55" << std::endl;
    return 7;
}
"#;

#[test]
fn a_checker_without_testlib_gives_exit_7_points_on_its_last_stderr_line() {
    let mut f = fixture(8, CustomKind::Testlib, HAND_WRITTEN_POINTS);
    let points = judge(&mut f, "x\n", "a\n").unwrap();
    assert!(points.passed && (points.fraction - 0.55).abs() < 1e-9, "{points:?}");
}
