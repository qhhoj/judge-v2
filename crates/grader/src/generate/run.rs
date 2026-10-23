//! One run of a generator or validator in a fresh box.

use super::plan::ProgramLimits;
use crate::error::GradeError;
use crate::languages::{Placeholders, expand};
use crate::program::Program;
use judge_sandbox::classify::{OutputUse, RunVerdict, classify};
use judge_sandbox::spec::{Limits, RunSpec, Stdio};
use judge_sandbox::{IsolateBox, Room};
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::path::Path;

const STDIN_FILE: &str = "input";
const STDOUT_FILE: &str = "output";
const STDERR_FILE: &str = ".stderr";
const DISCARD: &str = "/dev/null";
/// How much of a program's stderr is kept: a validator's reason, a generator's `FAIL` line.
pub const MAX_STDERR_BYTES: u64 = 512;
const EXTRA_TIME_S: f64 = 0.5;
const OPEN_FILES: u32 = 64;

/// What a run is given. `stdout_to` receives the program's stdout (at most `output_cap` bytes);
/// without it stdout is discarded.
pub struct ProgramRun<'a> {
    pub program: &'a Program,
    pub args: &'a [String],
    pub stdin: Option<&'a File>,
    pub stdout_to: Option<&'a Path>,
    pub limits: &'a ProgramLimits,
    pub output_cap: u64,
    pub jvm_reserve_kib: u64,
}

#[derive(Debug)]
pub struct ProgramExit {
    pub verdict: RunVerdict,
    /// The start of stderr, lossy UTF-8.
    pub stderr: String,
}

/// `--testset tests --group <group>`, the group left out when the test has none.
pub fn validator_args(group: &str) -> Vec<String> {
    let mut args: Vec<String> = ["--testset", "tests"].map(String::from).to_vec();
    if !group.is_empty() {
        args.extend(["--group".to_string(), group.to_string()]);
    }
    args
}

fn sandbox_limits(run: &ProgramRun) -> Limits {
    let reserve = if run.program.jvm { run.jvm_reserve_kib } else { 0 };
    Limits {
        time_s: run.limits.time_s,
        wall_s: run.limits.wall_s,
        extra_time_s: EXTRA_TIME_S,
        memory_kib: run.limits.memory_kib + reserve,
        processes: run.program.processes.max(1),
        // one KiB of slack so output of exactly the cap is not cut off
        fsize_kib: run.output_cap.div_ceil(1024) + 1,
        open_files: OPEN_FILES,
    }
}

fn spec_of(run: &ProgramRun) -> RunSpec {
    let memory = Placeholders { memory_kib: run.limits.memory_kib, class: None };
    let mut argv = expand(&run.program.run, &memory);
    argv.extend(run.args.iter().cloned());
    let stdout = if run.stdout_to.is_some() { STDOUT_FILE } else { DISCARD };
    RunSpec {
        argv,
        limits: sandbox_limits(run),
        stdio: Stdio {
            stdin: Some(if run.stdin.is_some() { STDIN_FILE } else { DISCARD }.into()),
            stdout: Some(stdout.into()),
            stderr: Some(STDERR_FILE.into()),
        },
        env: run.program.env.clone(),
        dirs: run.program.dirs.clone(),
        inherit_fds: false,
    }
}

/// Resets the box and puts the program and its stdin in it; the disk quota also holds an output at the cap.
fn stage(sandbox: &mut IsolateBox, run: &ProgramRun) -> Result<(), GradeError> {
    let stdin_len = match run.stdin {
        Some(stdin) => stdin.metadata()?.len(),
        None => 0,
    };
    sandbox.reset_with_room(
        Room::for_files(&[&run.program.dir])?.plus_file(stdin_len).plus_file(run.output_cap),
    )?;
    sandbox.put_tree(&run.program.dir)?;
    if let Some(stdin) = run.stdin {
        // The handle's own inode, reached through the kernel's fd link; nothing is looked up by path.
        sandbox.put(STDIN_FILE, Path::new(&format!("/proc/self/fd/{}", stdin.as_raw_fd())), false)?;
    }
    Ok(())
}

/// Copies the box's stdout file to `dest`, at most `cap + 1` bytes (one past the cap tells over-limit);
/// returns the number of bytes copied.
fn collect_stdout(sandbox: &IsolateBox, dest: &Path, cap: u64) -> Result<u64, GradeError> {
    let mut target = File::create(dest)?;
    // a program that replaced its stdout file with a symlink or a fifo has written nothing we read
    let Ok(file) = sandbox.open_output(STDOUT_FILE) else { return Ok(0) };
    let copied = std::io::copy(&mut file.take(cap + 1), &mut target)?;
    target.flush()?;
    Ok(copied)
}

fn collect_stderr(sandbox: &IsolateBox) -> String {
    let bytes = sandbox.read_output(STDERR_FILE, MAX_STDERR_BYTES).unwrap_or_default();
    String::from_utf8_lossy(&bytes).trim().to_string()
}

pub fn run_program(sandbox: &mut IsolateBox, run: &ProgramRun) -> Result<ProgramExit, GradeError> {
    stage(sandbox, run)?;
    let spec = spec_of(run);
    let meta = sandbox.run(&spec)?;
    let written = match run.stdout_to {
        Some(dest) => collect_stdout(sandbox, dest, run.output_cap)?,
        None => 0,
    };
    let verdict =
        classify(&meta, OutputUse { written, cap: run.output_cap, own_files_kib: None }, &spec.limits);
    Ok(ProgramExit { verdict, stderr: collect_stderr(sandbox) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_validator_gets_its_testset_and_the_group_only_when_there_is_one() {
        assert_eq!(validator_args("3"), ["--testset", "tests", "--group", "3"]);
        assert_eq!(validator_args(""), ["--testset", "tests"]);
    }

    fn program(jvm: bool, processes: u32) -> Program {
        Program {
            dir: ".".into(),
            run: vec!["./main".into(), "{memory_mib}".into()],
            processes,
            jvm,
            dirs: vec![],
            env: vec![],
            exception_marker: None,
            exception_style: Default::default(),
            lease: None,
        }
    }

    #[test]
    fn limits_follow_the_plan_with_the_jvm_reserve_and_the_languages_process_count() {
        let limits = ProgramLimits { time_s: 10.0, wall_s: 20.0, memory_kib: 1 << 20 };
        let args = vec!["7".to_string(), "x".to_string()];
        let (native, java) = (program(false, 1), program(true, 24));
        let run = |program, stdin| ProgramRun {
            program,
            args: &args,
            stdin,
            stdout_to: Some(Path::new("/tmp/out")),
            limits: &limits,
            output_cap: 2048,
            jvm_reserve_kib: 4096,
        };
        let spec = spec_of(&run(&native, None));
        assert_eq!(spec.argv, ["./main", "1024", "7", "x"]);
        assert_eq!((spec.limits.time_s, spec.limits.wall_s, spec.limits.processes), (10.0, 20.0, 1));
        assert_eq!((spec.limits.memory_kib, spec.limits.fsize_kib), (1 << 20, 3));
        assert_eq!(spec.stdio.stdin.as_deref(), Some(DISCARD));
        let java = spec_of(&run(&java, None));
        assert_eq!((java.limits.memory_kib, java.limits.processes), ((1 << 20) + 4096, 24));
    }

    #[test]
    fn stdout_is_discarded_unless_a_destination_is_given() {
        let limits = ProgramLimits::default();
        let program = program(false, 1);
        let run = ProgramRun {
            program: &program,
            args: &[],
            stdin: None,
            stdout_to: None,
            limits: &limits,
            output_cap: 1,
            jvm_reserve_kib: 0,
        };
        assert_eq!(spec_of(&run).stdio.stdout.as_deref(), Some(DISCARD));
    }
}
