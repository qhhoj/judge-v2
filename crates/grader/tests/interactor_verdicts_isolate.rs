#![cfg(feature = "isolate")]
//! Interactive verdict edge cases (bug hunt INT-1..3): who is blamed when one side of the pair ends first.
mod common;

use common::{artefacts, cpp_helper, installed, registry, sandbox};
use judge_grader::compile::{CompileJob, build, program_for};
use judge_grader::error::GradeError;
use judge_grader::interactor::{Boxes, Interaction, Interactive, interact};
use judge_grader::program::Program;
use judge_grader::run_case::{CaseLimits, Runnable};
use judge_sandbox::IsolateBox;
use judge_sandbox::classify::RunVerdict;

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

fn python_or_skip() -> bool {
    let ok = installed(registry().get("PY3").unwrap());
    if !ok {
        eprintln!("skip: PY3");
    }
    ok
}

fn assert_interactor_wa(result: &Result<Interactive, GradeError>) {
    let check = result.as_ref().ok().and_then(|r| r.check.as_ref());
    assert!(
        matches!(check, Some(c) if !c.passed && c.feedback == "too many guesses"),
        "the interactor quit first with WA, so its verdict should stand: {result:?}"
    );
}

/// INT-1: the interactor writes a final ack after a slow check; the correct solution has already exited.
#[test]
fn int1_a_final_ack_after_the_solution_exited_is_not_an_interactor_crash() {
    let slow_ack = r#"#include "testlib.h"
#include <unistd.h>
int main(int argc, char* argv[]) {
    registerInteraction(argc, argv);
    int secret = inf.readInt();
    std::cout << secret << std::endl;
    int guess = ouf.readInt();
    usleep(500000);
    std::cout << (guess == secret ? "correct" : "wrong") << std::endl;
    if (guess != secret) quitf(_wa, "wrong");
    quitf(_ok, "ok");
}
"#;
    let echo =
        "#include <iostream>\nint main(){ int x; std::cin >> x; std::cout << x << std::endl; return 0; }\n";
    let mut g = game(70, slow_ack);
    let result = play(&mut g, echo);
    assert!(
        matches!(&result, Ok(r) if r.verdict == RunVerdict::Ok && r.check.as_ref().is_some_and(|c| c.passed)),
        "expected AC: {result:?}"
    );
}

/// INT-2: a Python solution that keeps guessing; the interactor quits with WA and Python dies on EOF.
#[test]
fn int2_python_eof_after_the_interactor_quit_is_the_interactors_wa() {
    if !python_or_skip() {
        return;
    }
    let mut g = game(72, INTERACTOR);
    let result = play_in(&mut g, "PY3", "while True:\n    print(1, flush=True)\n    r = input()\n");
    assert_interactor_wa(&result);
}

/// INT-2: as above, but the Python solution dies writing to the closed pipe (BrokenPipeError).
#[test]
fn int2_python_broken_pipe_after_the_interactor_quit_is_the_interactors_wa() {
    if !python_or_skip() {
        return;
    }
    let mut g = game(74, INTERACTOR);
    let result = play_in(&mut g, "PY3", "import sys\nwhile True:\n    print(1, flush=True)\n");
    assert_interactor_wa(&result);
}

/// INT-2 (C++ variant): the solution returns 1 when cin fails after the interactor quit.
#[test]
fn int2_cpp_nonzero_exit_on_eof_after_the_interactor_quit_is_the_interactors_wa() {
    let mut g = game(76, INTERACTOR);
    let gives_up = "#include <iostream>\n#include <string>\nint main(){ std::string r; while (true) { std::cout << 1 << std::endl; if (!(std::cin >> r)) return 1; } }\n";
    let result = play(&mut g, gives_up);
    assert_interactor_wa(&result);
}

/// INT-3a: an interactor that hangs forever is a jury bug, not the contestant's TLE.
#[test]
fn int3_a_hanging_interactor_is_an_error_not_tle() {
    let hangs = "#include \"testlib.h\"\nint main(int c, char** v){ registerInteraction(c, v); volatile long s = 0; while (true) s++; }\n";
    let mut g = game(78, hangs);
    let result = play(&mut g, GOOD);
    assert!(result.is_err(), "a hung interactor should be IE: {result:?}");
}

/// INT-3b: an interactor that computes for ~5 s (within its own CPU and wall limits) before answering.
#[test]
fn int3_a_slow_interactor_within_its_limits_does_not_give_the_solution_tle() {
    let slow = r#"#include "testlib.h"
#include <ctime>
int main(int argc, char* argv[]) {
    registerInteraction(argc, argv);
    int secret = inf.readInt(), asked = 0;
    clock_t start = clock();
    volatile long s = 0;
    while (clock() - start < 5 * CLOCKS_PER_SEC) s++;
    while (true) {
        int guess = ouf.readInt();
        if (++asked > 40) quitf(_wa, "too many guesses");
        if (guess == secret) { std::cout << "=" << std::endl; quitf(_ok, "found in %d", asked); }
        std::cout << (guess < secret ? "<" : ">") << std::endl;
    }
}
"#;
    let mut g = game(80, slow);
    let result = play(&mut g, GOOD);
    assert!(
        matches!(&result, Err(GradeError::Checker(m)) if m.starts_with("interactor")),
        "the jury's time is IE, not the contestant's TLE: {result:?}"
    );
}
