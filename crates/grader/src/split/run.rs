//! One case of a split problem: the jury in the helper box (stdin `input`, stdout `output`, as the one-process
//! program had them) and the contestant in the solution box (no input, stdin and stdout `/dev/null`), joined by
//! two pipes on fds 3/4. Each side is measured in its own box: the contestant is charged its own CPU and
//! memory plus the jury CPU its calls used (the jury records it in `CHARGE_FILE`), the jury is held to its own
//! limits.

use crate::checkers::custom::checker_limits;
use crate::error::GradeError;
use crate::languages::{Placeholders, expand};
use crate::program::Program;
use crate::run_case::{
    Runnable, STDERR_FILE, keeps_stderr, own_files_kib, refine, reported_memory, solution_limits,
};
use crate::session::Boxes;
use crate::session::channel::PipeChannel;
use crate::session::run::{
    ChannelPlan, MemberSpec, host_deadline, ignoring_sigpipe, spawn_session, wait_all,
};
use crate::session::verdict::MemberRun;
use judge_interface::runtime::{CHARGE_FILE, PROTOCOL_FAULT_FILE};
use judge_sandbox::classify::{OutputUse, RunVerdict, classify};
use judge_sandbox::error::SandboxError;
use judge_sandbox::spec::{Limits, RunSpec, Stdio};
use judge_sandbox::{IsolateBox, Room};
use std::fs::File;
use std::os::fd::AsRawFd;
use std::path::Path;

const JURY_OUTPUT: &str = "output";
const CHARGE_BYTES: u64 = 16;
/// The protocol fault file holds one short reason.
const FAULT_BYTES: u64 = 64;

pub struct SplitCase<'a> {
    pub contestant: &'a Runnable<'a>,
    pub jury: &'a Program,
    pub input: &'a File,
    pub output_cap: u64,
}

#[derive(Debug)]
pub struct SplitRun {
    pub contestant: MemberRun,
    /// The limits the contestant box ran under.
    pub contestant_limits: Limits,
    pub jury: RunVerdict,
    pub jury_time_s: f64,
    /// Jury CPU spent serving the contestant's calls.
    pub served_s: f64,
    pub protocol_fault: bool,
}

/// Jury CPU max(10 s, 2 x TL) (as an interactor's), wall the contestant's + 2 s, memory at least the problem's
/// (the one process had it all) and 1 GiB, its `output` held to the case output cap.
pub fn jury_limits(contestant: &Limits, output_cap: u64) -> Limits {
    let mut limits = checker_limits(contestant.time_s);
    limits.wall_s = contestant.wall_s + 2.0;
    limits.memory_kib = limits.memory_kib.max(contestant.memory_kib);
    limits.fsize_kib = output_cap.div_ceil(1024) + 1;
    limits
}

fn contestant_spec(case: &SplitCase) -> RunSpec {
    let (program, limits) = (case.contestant.program, case.contestant.limits);
    let stderr = if keeps_stderr(program) { STDERR_FILE } else { "/dev/null" };
    RunSpec {
        argv: expand(&program.run, &Placeholders { memory_kib: limits.memory_kib, class: None }),
        limits: solution_limits(case.contestant, case.output_cap),
        stdio: Stdio {
            stdin: Some("/dev/null".into()),
            stdout: Some("/dev/null".into()),
            stderr: Some(stderr.into()),
        },
        env: program.env.clone(),
        dirs: program.dirs.clone(),
        inherit_fds: true,
    }
}

fn jury_spec(case: &SplitCase, contestant: &Limits) -> RunSpec {
    let mut limits = jury_limits(contestant, case.output_cap);
    limits.processes = case.jury.processes.max(1);
    let memory = Placeholders { memory_kib: limits.memory_kib, class: None };
    RunSpec {
        argv: ignoring_sigpipe(expand(&case.jury.run, &memory)),
        limits,
        stdio: Stdio {
            stdin: Some("input".into()),
            stdout: Some(JURY_OUTPUT.into()),
            stderr: Some("/dev/null".into()),
        },
        env: case.jury.env.clone(),
        dirs: case.jury.dirs.clone(),
        inherit_fds: true,
    }
}

/// Fresh boxes: the contestant program alone, and the jury program with the case input.
fn prepare(boxes: &mut Boxes, case: &SplitCase) -> Result<(), GradeError> {
    let program = case.contestant.program;
    boxes.solution.reset_with_room(Room::for_files(&[&program.dir])?.plus_file(case.output_cap))?;
    boxes.solution.put_tree(&program.dir)?;
    let input_bytes = case.input.metadata()?.len();
    let records = Room::default().plus_file(CHARGE_BYTES).plus_file(FAULT_BYTES);
    let room =
        Room::for_files(&[&case.jury.dir])?.plus_file(input_bytes).plus_file(case.output_cap).plus(records);
    boxes.helper.reset_with_room(room)?;
    boxes.helper.put_tree(&case.jury.dir)?;
    let handle = format!("/proc/self/fd/{}", case.input.as_raw_fd());
    boxes.helper.put("input", Path::new(&handle), false)?;
    Ok(())
}

/// Runs the pair and copies the jury's `output` to `output` (the checker resets the helper box).
pub fn run_split(mut boxes: Boxes, case: &SplitCase, output: &Path) -> Result<SplitRun, GradeError> {
    prepare(&mut boxes, case)?;
    let contestant = contestant_spec(case);
    let jury = jury_spec(case, &contestant.limits);
    let manager = MemberSpec { sandbox: &*boxes.helper, run: &jury };
    let member = MemberSpec { sandbox: &*boxes.solution, run: &contestant };
    let mut session = spawn_session(&manager, &member, ChannelPlan::Pipes(PipeChannel::create()?))?;
    wait_all(&mut session, host_deadline(&jury.limits))?;
    let contestant_run = contestant_result(boxes.solution, case, &contestant.limits)?;
    let (jury_verdict, jury_time_s) = jury_result(boxes.helper, case, (&jury.limits, output))?;
    let charge = boxes.helper.read_output(CHARGE_FILE, CHARGE_BYTES).unwrap_or_default();
    Ok(SplitRun {
        contestant: contestant_run,
        contestant_limits: contestant.limits,
        jury: jury_verdict,
        jury_time_s,
        served_s: served_seconds(&charge, jury_time_s),
        protocol_fault: boxes.helper.open_output(PROTOCOL_FAULT_FILE).is_ok(),
    })
}

/// The charge record's total, plus the callback still running when the jury ended (from its start to the jury's
/// last CPU second). No record (no callback was served) is 0.
fn served_seconds(record: &[u8], jury_time_s: f64) -> f64 {
    let word = |at: usize| record.get(at..at + 8).map_or(0, |b| u64::from_ne_bytes(b.try_into().unwrap()));
    let (charged_ns, open_since_ns) = (word(0), word(8));
    let open_s = if open_since_ns == 0 { 0.0 } else { (jury_time_s - open_since_ns as f64 / 1e9).max(0.0) };
    (charged_ns as f64 / 1e9 + open_s).min(jury_time_s)
}

fn not_internal(verdict: RunVerdict) -> Result<RunVerdict, GradeError> {
    match verdict {
        RunVerdict::Internal(message) => Err(SandboxError::Isolate(message).into()),
        verdict => Ok(verdict),
    }
}

/// The contestant's own files are its kept stderr alone (no input, no output file).
fn contestant_result(
    sandbox: &IsolateBox,
    case: &SplitCase,
    limits: &Limits,
) -> Result<MemberRun, GradeError> {
    let meta = sandbox.read_meta()?;
    let program = case.contestant.program;
    let own_files = Some(own_files_kib(sandbox, program, 0, None));
    let usage = OutputUse { own_files_kib: own_files, ..OutputUse::UNMEASURED };
    let verdict = refine(not_internal(classify(&meta, usage, limits))?, program, sandbox);
    let memory_kib = reported_memory(&meta, own_files, &verdict);
    Ok(MemberRun { verdict, time_s: meta.time_s, memory_kib })
}

/// The jury's verdict (its `output` over the cap is OLE, as the one process's was) and its CPU seconds.
fn jury_result(
    sandbox: &IsolateBox,
    case: &SplitCase,
    (limits, output): (&Limits, &Path),
) -> Result<(RunVerdict, f64), GradeError> {
    let meta = sandbox.read_meta()?;
    // One byte past the cap tells "exactly the cap" (fine) from "over it" (OLE).
    let written = sandbox.read_output(JURY_OUTPUT, case.output_cap + 1).unwrap_or_default();
    std::fs::write(output, &written)?;
    let own_files = own_files_kib(sandbox, case.jury, case.input.metadata()?.len(), Some(JURY_OUTPUT));
    let usage =
        OutputUse { written: written.len() as u64, cap: case.output_cap, own_files_kib: Some(own_files) };
    Ok((not_internal(classify(&meta, usage, limits))?, meta.time_s))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_jury_gets_its_own_limits_never_less_than_the_problems() {
        let contestant = Limits::for_solution(1.0, 2 << 20, 1, 1 << 20); // 1 s, 2 GiB
        let jury = jury_limits(&contestant, 1 << 20);
        assert_eq!((jury.time_s, jury.wall_s), (10.0, contestant.wall_s + 2.0));
        assert_eq!(jury.memory_kib, 2 << 20);
        assert_eq!(jury.fsize_kib, contestant.fsize_kib, "the jury's output is held to the case cap");
        let small = jury_limits(&Limits::for_solution(6.0, 65_536, 1, 1 << 20), 1 << 20);
        assert_eq!((small.time_s, small.memory_kib), (12.0, 1 << 20));
    }

    #[test]
    fn the_served_time_adds_the_callback_the_jury_died_in_and_never_exceeds_the_jurys_cpu() {
        let record = |charged_s: f64, open_since_s: f64| {
            let ns = |s: f64| ((s * 1e9) as u64).to_ne_bytes();
            [ns(charged_s), ns(open_since_s)].concat()
        };
        assert_eq!(served_seconds(&[], 2.0), 0.0, "no record: no callback ran");
        assert!((served_seconds(&record(0.5, 0.0), 2.0) - 0.5).abs() < 1e-9);
        assert!(
            (served_seconds(&record(0.5, 1.5), 4.0) - 3.0).abs() < 1e-9,
            "0.5 s, then 2.5 s in the last call"
        );
        assert_eq!(served_seconds(&record(9.0, 0.0), 2.0), 2.0);
    }
}
