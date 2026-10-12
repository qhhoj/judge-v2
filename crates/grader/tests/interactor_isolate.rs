#![cfg(feature = "isolate")]
//! A guessing game: the interactor holds a number from `input`; the solution finds it by binary search.
mod common;

use common::{artefacts, cpp_helper, registry, sandbox};
use judge_grader::compile::{CompileJob, build, program_for};
use judge_grader::error::GradeError;
use judge_grader::interactor::{Boxes, Interaction, Interactive, interact};
use judge_grader::program::Program;
use judge_grader::run_case::{CaseLimits, Runnable};
use judge_sandbox::IsolateBox;
use judge_sandbox::classify::RunVerdict;
use std::time::{Duration, Instant};

const INTERACTOR: &str = r#"#include "testlib.h"
int main(int argc, char* argv[]) {
    registerInteraction(argc, argv);
    int secret = inf.readInt(), asked = 0;
    while (true) {
        int guess = ouf.readInt();
        if (++asked > 40) quitf(_wa, "too many guesses");
        if (guess == secret) { std::cout << "=" << std::endl; quitf(_ok, "found in %d", asked); }
        std::cout << (guess < secret ? "<" : ">") << std::endl;
    }
}
"#;

const GOOD: &str = r#"#include <iostream>
#include <string>
int main() {
    int lo = 1, hi = 1000;
    while (true) {
        int m = (lo + hi) / 2;
        std::cout << m << std::endl;
        std::string r; std::cin >> r;
        if (r == "=") return 0;
        if (r == "<") lo = m + 1; else hi = m - 1;
    }
}
"#;

struct Game {
    dirs: Vec<tempfile::TempDir>,
    solution: IsolateBox,
    helper: IsolateBox,
    interactor: Program,
}

fn game(offset: u32, interactor_source: &str) -> Game {
    let dirs: Vec<_> = (0..4).map(|_| tempfile::tempdir().unwrap()).collect();
    let solution = sandbox(offset, dirs[0].path());
    let mut helper = sandbox(offset + 1, dirs[0].path());
    let interactor = artefacts(dirs[1].path()).program(&mut helper, &cpp_helper(interactor_source)).unwrap();
    std::fs::write(dirs[3].path().join("input"), "377\n").unwrap();
    std::fs::write(dirs[3].path().join("answer"), "").unwrap();
    Game { dirs, solution, helper, interactor }
}

fn play(g: &mut Game, source: &str) -> Result<Interactive, GradeError> {
    play_in(g, "CPP17", source)
}

fn play_in(g: &mut Game, key: &str, source: &str) -> Result<Interactive, GradeError> {
    let registry = registry();
    let language = registry.get(key).unwrap();
    let dir = g.dirs[2].path().join("program");
    let _ = std::fs::remove_dir_all(&dir);
    build(&mut g.solution, &CompileJob::for_language(language, source.as_bytes()), &dir).unwrap();
    let program = program_for(language, &dir, source.as_bytes());
    let limits = CaseLimits { time_limit_s: 1.0, memory_kib: 262_144, jvm_reserve_kib: 0 };
    let runnable = Runnable { program: &program, limits: &limits };
    let scratch = g.dirs[3].path();
    let x = Interaction {
        solution: &runnable,
        interactor: &g.interactor,
        input: &scratch.join("input"),
        answer: &scratch.join("answer"),
        points: 1.0,
    };
    interact(Boxes { solution: &mut g.solution, helper: &mut g.helper }, &x)
}

#[test]
fn binary_search_is_accepted_and_reports_time_and_memory() {
    let mut g = game(6, INTERACTOR);
    let result = play(&mut g, GOOD).unwrap();
    assert_eq!(result.verdict, RunVerdict::Ok);
    let check = result.check.unwrap();
    assert!(check.passed, "{check:?}");
    assert!(result.memory_kib > 0);
}

#[test]
fn wrong_guessing_is_wa() {
    let mut g = game(8, INTERACTOR);
    let always_one = "#include <iostream>\n#include <string>\nint main(){ std::string r; while (true) { std::cout << 1 << std::endl; if (!(std::cin >> r)) return 0; } }\n";
    let check = play(&mut g, always_one).unwrap().check.unwrap();
    assert!(!check.passed);
    assert_eq!(check.feedback, "too many guesses");
}

#[test]
fn a_silent_solution_is_tle_and_the_interactor_is_not_blamed() {
    let mut g = game(10, INTERACTOR);
    let started = Instant::now();
    let result = play(&mut g, "#include <unistd.h>\nint main(){ sleep(30); }\n").unwrap();
    assert_eq!(result.verdict, RunVerdict::Tle);
    assert!(result.check.is_none());
    assert!(started.elapsed() < Duration::from_secs(15), "the pair deadlocked");
}

#[test]
fn both_sides_waiting_for_each_other_end_by_the_wall_limit() {
    let mut g = game(12, INTERACTOR);
    let started = Instant::now();
    let reads_first = "#include <string>\n#include <iostream>\nint main(){ std::string r; std::cin >> r; std::cout << 1 << std::endl; }\n";
    let result = play(&mut g, reads_first).unwrap();
    assert_eq!(result.verdict, RunVerdict::Tle);
    assert!(started.elapsed() < Duration::from_secs(15));
}

#[test]
fn a_crashing_solution_is_rte_whatever_the_interactor_saw() {
    let mut g = game(14, INTERACTOR);
    let result = play(&mut g, "int main(){ *(volatile int*)0 = 1; }\n").unwrap();
    assert!(matches!(result.verdict, RunVerdict::Rte { ref reason } if reason == "SIGSEGV"), "{result:?}");
    assert!(result.check.is_none());
}

#[test]
fn an_interactor_that_fails_or_crashes_is_an_error_not_a_verdict() {
    let failing = "#include \"testlib.h\"\nint main(int c, char** v){ registerInteraction(c, v); quitf(_fail, \"jury bug\"); }\n";
    let mut g = game(16, failing);
    let result = play(&mut g, GOOD);
    assert!(matches!(result, Err(GradeError::Checker(ref m)) if m.starts_with("interactor:")), "{result:?}");
    let crashing = "#include \"testlib.h\"\nint main(int c, char** v){ registerInteraction(c, v); *(volatile int*)0 = 1; }\n";
    let mut g = game(18, crashing);
    assert!(matches!(play(&mut g, GOOD), Err(GradeError::Checker(m)) if m.contains("crashed")));
}

#[test]
fn a_dead_interactor_is_an_error_even_when_the_solution_then_fails_on_eof() {
    // The parity fixture: an interactor that exits 3 at once; the solution exits non-zero on the EOF.
    let gives_up_on_eof = "#include <iostream>\n#include <string>\nint main(){ std::cout << 500 << std::endl; std::string r; if (!(std::cin >> r)) return 1; }\n";
    let mut g = game(20, "int main() { return 3; }\n");
    let result = play(&mut g, gives_up_on_eof);
    assert!(matches!(result, Err(GradeError::Checker(ref m)) if m.starts_with("interactor:")), "{result:?}");
}

/// Quits with WA on the first wrong guess, as most real interactors do.
const STRICT: &str = r#"#include "testlib.h"
int main(int argc, char* argv[]) {
    registerInteraction(argc, argv);
    int secret = inf.readInt();
    int guess = ouf.readInt();
    if (guess != secret) quitf(_wa, "wrong guess %d", guess);
    std::cout << "=" << std::endl;
    quitf(_ok, "found");
}
"#;

fn assert_interactor_wa(result: Interactive) {
    let check = result
        .check
        .as_ref()
        .unwrap_or_else(|| panic!("solution verdict won over the interactor's WA: {result:?}"));
    assert!(!check.passed, "{result:?}");
    assert_eq!(check.feedback, "wrong guess 1");
}

#[test]
fn cpp_writing_after_the_interactor_quit_wa_is_wa() {
    // Control: C++ dies by SIGPIPE, which is exempt, so the interactor's WA is the verdict.
    let mut g = game(40, STRICT);
    let src = "#include <iostream>\n#include <unistd.h>\nint main(){ std::cout << 1 << std::endl; usleep(300000); for (int i = 0; i < 100000; i++) std::cout << 1 << std::endl; }\n";
    assert_interactor_wa(play(&mut g, src).unwrap());
}

#[test]
fn python_reading_after_the_interactor_quit_wa_is_wa_not_eoferror() {
    let mut g = game(42, STRICT);
    let src = "print(1, flush=True)\nr = input()\n";
    assert_interactor_wa(play_in(&mut g, "PY3", src).unwrap());
}

#[test]
fn python_writing_after_the_interactor_quit_wa_is_wa_not_brokenpipe() {
    let mut g = game(44, STRICT);
    let src = "import time\nprint(1, flush=True)\ntime.sleep(0.3)\nfor _ in range(100000):\n    print(1, flush=True)\n";
    assert_interactor_wa(play_in(&mut g, "PY3", src).unwrap());
}

/// Two rounds: after the first answer it takes a while before writing round two.
const TWO_ROUNDS: &str = r#"#include "testlib.h"
#include <unistd.h>
int main(int argc, char* argv[]) {
    registerInteraction(argc, argv);
    int secret = inf.readInt();
    for (int round = 0; round < 2; round++) {
        int guess = ouf.readInt();
        if (guess != secret) quitf(_wa, "wrong guess %d", guess);
        usleep(300000);
        std::cout << "=" << std::endl;
    }
    quitf(_ok, "both rounds");
}
"#;

#[test]
fn an_interactor_hit_by_sigpipe_after_the_solution_quit_early_is_wa_not_ie() {
    let mut g = game(46, TWO_ROUNDS);
    let one_round_only = "#include <iostream>\nint main(){ std::cout << 377 << std::endl; }\n";
    let result = play(&mut g, one_round_only);
    let result = result.unwrap_or_else(|e| panic!("internal error instead of WA: {e:?}"));
    assert_eq!(result.verdict, RunVerdict::Ok);
    assert!(!result.check.unwrap().passed);
}

#[test]
fn cpp_returning_1_on_eof_after_the_interactor_quit_wa_is_wa() {
    let mut g = game(48, STRICT);
    let src = "#include <iostream>\nint main(){ std::cout << 1 << std::endl; int r; if (!(std::cin >> r)) return 1; }\n";
    assert_interactor_wa(play(&mut g, src).unwrap());
}

#[test]
fn a_python_solution_that_crashes_before_answering_is_rte_not_the_interactors_eof_pe() {
    // Stock testlib exits 2 (PE) when it reads EOF, so that exit code must not beat the solution's crash.
    let mut g = game(50, STRICT);
    let result = play_in(&mut g, "PY3", "x = 1 // 0\n").unwrap();
    assert_eq!(result.verdict, RunVerdict::Rte { reason: "ZeroDivisionError".into() });
    assert!(result.check.is_none(), "{result:?}");
}
