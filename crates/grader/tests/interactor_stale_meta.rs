//! An interactive run whose isolate processes end without writing their meta files (killed by a signal: the
//! file is then missing or empty) is a sandbox failure, never graded from the meta file a previous run in the
//! same box left. No real isolate: a stand-in answers `--init` and runs nothing.
mod common;

use common::registry;
use judge_grader::checkers::custom::PartialPoints;
use judge_grader::compile::program_for;
use judge_grader::error::GradeError;
use judge_grader::interactor::{Boxes, Interaction, Interactive, interact};
use judge_grader::run_case::{CaseLimits, Runnable};
use judge_sandbox::{IsolateBox, SandboxConfig, SandboxError};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

/// Answers `--init` with a root per box id and exits 0 on every other call, writing no meta file.
const NO_META: &str = r#"#!/bin/sh
d=$(dirname "$0")
case "$*" in *--init*) mkdir -p "$d/root$2/box"; echo "$d/root$2" ;; esac
"#;

/// As `NO_META`, but a run leaves its meta file empty, as isolate killed after opening it does.
const EMPTY_META: &str = r#"#!/bin/sh
d=$(dirname "$0")
case "$*" in *--init*) mkdir -p "$d/root$2/box"; echo "$d/root$2" ;; esac
for a in "$@"; do case "$a" in --meta=*) : > "${a#--meta=}" ;; esac; done
"#;

/// A meta file every check would grade: the solution timed out, the interactor finished cleanly.
const STALE_SOLUTION_META: &str = "status:TO\ntime:1.100\ntime-wall:1.200\n";
const STALE_INTERACTOR_META: &str = "time:0.010\ntime-wall:1.200\nexitcode:0\n";

fn fake_isolate(dir: &Path, script: &str) -> SandboxConfig {
    let bin = dir.join("isolate");
    std::fs::write(&bin, script).unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    SandboxConfig { isolate_bin: bin, meta_dir: dir.join("meta"), ..SandboxConfig::default() }
}

/// A directory holding one file, `name`.
fn dir_with(root: &Path, dir: &str, name: &str) -> std::path::PathBuf {
    let path = root.join(dir);
    std::fs::create_dir_all(&path).unwrap();
    std::fs::write(path.join(name), "").unwrap();
    path
}

/// One interactive run under the stand-in `script`, after a previous run left a meta file in both boxes.
fn interact_after_a_previous_run(script: &str) -> Result<Interactive, GradeError> {
    let dir = tempfile::tempdir().unwrap();
    let config = fake_isolate(dir.path(), script);
    let mut solution_box = IsolateBox::open(&config, 7, None).unwrap();
    let mut helper_box = IsolateBox::open(&config, 8, None).unwrap();
    std::fs::write(config.meta_dir.join("7.meta"), STALE_SOLUTION_META).unwrap();
    std::fs::write(config.meta_dir.join("8.meta"), STALE_INTERACTOR_META).unwrap();
    let registry = registry();
    let cpp = registry.get("CPP17").unwrap();
    let solution = program_for(cpp, &dir_with(dir.path(), "solution", "main"), b"");
    let interactor = program_for(cpp, &dir_with(dir.path(), "interactor", "main"), b"");
    let limits = CaseLimits { time_limit_s: 1.0, memory_kib: 262_144, jvm_reserve_kib: 0 };
    let data = dir_with(dir.path(), "data", "input");
    std::fs::write(data.join("answer"), "").unwrap();
    let x = Interaction {
        solution: &Runnable { program: &solution, limits: &limits },
        interactor: &interactor,
        input: &data.join("input"),
        answer: &data.join("answer"),
        points: 1.0,
        partial: PartialPoints::Absolute,
    };
    interact(Boxes { solution: &mut solution_box, helper: &mut helper_box }, &x)
}

/// One test, so the two stand-in scripts are never written while another test thread forks (ETXTBSY).
#[test]
fn an_interactive_run_without_its_own_meta_is_a_sandbox_failure_not_the_previous_runs_verdict() {
    for (name, script) in [("no meta file", NO_META), ("empty meta file", EMPTY_META)] {
        let result = interact_after_a_previous_run(script);
        let refused = matches!(result, Err(GradeError::Sandbox(SandboxError::Meta(_))));
        assert!(refused, "{name}: {result:?}");
    }
}
