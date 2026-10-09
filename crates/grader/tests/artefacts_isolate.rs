#![cfg(feature = "isolate")]
mod common;

use common::{artefacts, cpp_helper, sandbox};
use judge_grader::error::GradeError;
use std::time::{Duration, Instant};

const OK_CHECKER: &str =
    "#include \"testlib.h\"\nint main(int c, char** v){ registerTestlibCmd(c, v); quitf(_ok, \"ok\"); }\n";

#[test]
fn compiles_once_per_key_and_serves_the_second_from_the_cache() {
    let (meta, cache) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut helper = sandbox(0, meta.path());
    let artefacts = artefacts(cache.path());
    let first = artefacts.program(&mut helper, &cpp_helper(OK_CHECKER)).unwrap();
    let started = Instant::now();
    let second = artefacts.program(&mut helper, &cpp_helper(OK_CHECKER)).unwrap();
    assert!(started.elapsed() < Duration::from_millis(200), "recompiled");
    assert_eq!(first.dir, second.dir);
    assert!(first.dir.join("main").exists());
    let other = artefacts.program(&mut helper, &cpp_helper(&format!("{OK_CHECKER}// changed\n"))).unwrap();
    assert_ne!(first.dir, other.dir);
}

#[test]
fn a_broken_checker_is_a_checker_error_and_leaves_nothing_cached() {
    let (meta, cache) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut helper = sandbox(1, meta.path());
    let artefacts = artefacts(cache.path());
    let broken = cpp_helper("int main( {");
    for _ in 0..2 {
        let error = artefacts.program(&mut helper, &broken).err().unwrap();
        assert!(matches!(error, GradeError::Checker(m) if m.starts_with("checker failed to compile")));
    }
}
