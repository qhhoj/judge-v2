#![cfg(feature = "isolate")]
mod common;

use common::{language_or_skip, registry, sandbox};
use judge_grader::compile::{BuildOutcome, CompileJob, build};

fn compile(key: &str, offset: u32, source: &str) -> Option<(BuildOutcome, tempfile::TempDir)> {
    let language = language_or_skip(&registry(), key)?;
    let (meta, out) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let job = CompileJob::for_language(&language, source.as_bytes());
    let outcome = build(&mut sandbox(offset, meta.path()), &job, out.path()).unwrap();
    Some((outcome, out))
}

#[test]
fn a_syntax_error_is_a_compile_error_with_the_log() {
    let Some((outcome, out)) = compile("CPP17", 1, "int main( {") else { return };
    match outcome {
        BuildOutcome::CompileError { log } => assert!(log.contains("error"), "{log}"),
        other => panic!("expected CE, got {other:?}"),
    }
    assert!(!out.path().join("main").exists());
}

#[test]
fn interpreted_sources_are_snapshotted_without_a_compiler() {
    let Some((outcome, out)) = compile("RUBY", 3, "puts 1\n") else { return };
    assert!(matches!(outcome, BuildOutcome::Built { .. }));
    assert_eq!(std::fs::read_to_string(out.path().join("main.rb")).unwrap(), "puts 1\n");
}

#[test]
fn a_compiler_that_runs_past_the_cpu_limit_is_a_timed_out_compile_error() {
    let Some(mut language) = language_or_skip(&registry(), "CPP17") else { return };
    language.compile = Some(["/bin/sh", "-c", "while :; do :; done"].map(String::from).to_vec());
    let (meta, out) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut job = CompileJob::for_language(&language, b"");
    job.limits.time_s = 0.5;
    job.limits.wall_s = 3.0;
    match build(&mut sandbox(4, meta.path()), &job, out.path()).unwrap() {
        BuildOutcome::CompileError { log } => assert!(log.contains("timed out"), "{log}"),
        other => panic!("expected CE, got {other:?}"),
    }
}

#[test]
fn a_compiler_that_plants_a_symlink_does_not_leak_it_into_the_snapshot() {
    let Some(mut language) = language_or_skip(&registry(), "CPP17") else { return };
    language.compile =
        Some(["/bin/sh", "-c", "ln -s /etc/hostname main && echo ok > real"].map(String::from).to_vec());
    let (meta, out) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let job = CompileJob::for_language(&language, b"");
    let outcome = build(&mut sandbox(5, meta.path()), &job, out.path()).unwrap();
    assert!(matches!(outcome, BuildOutcome::Built { .. }));
    assert!(out.path().join("real").exists());
    assert!(!out.path().join("main").exists(), "a symlink must not be copied or followed");
}

/// JAVA with a stand-in compiler that always succeeds, so only a refusal gives a compile error.
fn java_outcome(offset: u32, source: &str) -> BuildOutcome {
    let mut language = registry().get("JAVA").unwrap().clone();
    language.compile = Some(["/bin/sh", "-c", "true"].map(String::from).to_vec());
    language.compile_dirs.clear();
    let (meta, out) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let job = CompileJob::for_language(&language, source.as_bytes());
    build(&mut sandbox(offset, meta.path()), &job, out.path()).unwrap()
}

#[test]
fn a_java_package_line_is_a_compile_error_not_a_runtime_failure() {
    let source = "package solution;\npublic class Main { public static void main(String[] a) {} }\n";
    match java_outcome(6, source) {
        BuildOutcome::CompileError { log } => assert!(log.contains("package"), "{log}"),
        other => panic!("expected CE, got {other:?}"),
    }
}

#[test]
fn a_java_class_name_too_long_for_a_file_name_is_a_compile_error_not_a_sandbox_failure() {
    let source = format!("public class {} {{}}\n", "A".repeat(251));
    match java_outcome(7, &source) {
        BuildOutcome::CompileError { log } => assert!(log.contains("too long"), "{log}"),
        other => panic!("expected CE, got {other:?}"),
    }
}
