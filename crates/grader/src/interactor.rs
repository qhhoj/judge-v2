//! Interactive problems: two isolate runs joined by two pipes (docs/grading.md#checkers).
//!
//! The solution runs in its box and the interactor, which is problem-author code, in the helper box.
//! The daemon only creates the pipes and hands one end of each to a child as its stdio; it never reads or
//! writes them. The parent's copies are closed as soon as both children are spawned, so a side that exits
//! gives the other EOF. A pair that blocks on each other is ended by the sandbox wall-time limits; a host
//! deadline on top of that kills both isolate processes if a limit does not fire.
//!
//! Built on `crate::session`: this module only says how an interactive pair is laid out and how its testlib
//! rules compose.

use crate::checkers::CheckOutcome;
use crate::checkers::custom::{PartialPoints, PartialScale, checker_limits, helper_argv};
use crate::error::GradeError;
use crate::languages::{Placeholders, expand};
use crate::program::Program;
use crate::run_case::{
    Runnable, STDERR_FILE, keeps_stderr, own_files_kib, refine, reported_memory, solution_limits,
};
use crate::session::files::save_box_file;
use crate::session::run::{
    ChannelPlan, MemberSpec, host_deadline, ignoring_sigpipe, spawn_session, wait_all,
};
use crate::session::score::{MANAGER_STDERR, testlib_score};
use crate::session::verdict::{MemberRun, interactor_failed, solution_wins, stalled_by_jury};
use judge_sandbox::classify::{OutputUse, RunVerdict, classify};
use judge_sandbox::error::SandboxError;
use judge_sandbox::spec::{Limits, RunSpec, Stdio};
use judge_sandbox::{IsolateBox, Room};
use std::path::Path;

pub use crate::session::Boxes;

/// stdout of the solution is a pipe, not a file, so this only sizes `--fsize` for its other files.
const SOLUTION_FILE_CAP: u64 = 64 << 20;
/// testlib's `tout`: what the interactor wrote for the checker (Polygon's interactive checker reads it as the
/// contestant's output).
const TOUT: &str = "tout";

pub struct Interaction<'a> {
    pub solution: &'a Runnable<'a>,
    pub interactor: &'a Program,
    pub input: &'a Path,
    pub answer: &'a Path,
    /// What the case is worth: testlib's `points N` is N points of it, as for checkers.
    pub points: f64,
    /// Whether `points N` is N percent of the case instead.
    pub partial: PartialPoints,
}

#[derive(Debug)]
pub struct Interactive {
    pub verdict: RunVerdict,
    pub time_s: f64,
    /// The interactor's own CPU seconds.
    pub jury_time_s: f64,
    pub memory_kib: u64,
    /// `None` when the solution's own verdict wins.
    pub check: Option<CheckOutcome>,
}

/// Wall = solution wall + 2 s, CPU = max(10 s, 2 x TL), 1 GiB, one process.
pub fn interactor_limits(solution: &Limits) -> Limits {
    let mut limits = checker_limits(solution.time_s);
    limits.wall_s = solution.wall_s + 2.0;
    limits
}

fn solution_spec(x: &Interaction) -> RunSpec {
    let (program, limits) = (x.solution.program, x.solution.limits);
    let stderr = if keeps_stderr(program) { STDERR_FILE } else { "/dev/null" };
    RunSpec {
        argv: expand(&program.run, &Placeholders { memory_kib: limits.memory_kib, class: None }),
        limits: solution_limits(x.solution, SOLUTION_FILE_CAP),
        stdio: Stdio { stdin: None, stdout: None, stderr: Some(stderr.into()) },
        env: program.env.clone(),
        dirs: program.dirs.clone(),
        inherit_fds: false,
    }
}

fn interactor_spec(x: &Interaction, solution: &Limits) -> RunSpec {
    let mut limits = interactor_limits(solution);
    limits.processes = x.interactor.processes.max(1);
    let mut argv = ignoring_sigpipe(helper_argv(x.interactor, &limits));
    argv.extend(["input", TOUT, "answer"].map(String::from));
    RunSpec {
        argv,
        limits,
        stdio: Stdio { stdin: None, stdout: None, stderr: Some(MANAGER_STDERR.into()) },
        env: x.interactor.env.clone(),
        dirs: x.interactor.dirs.clone(),
        inherit_fds: false,
    }
}

fn prepare(boxes: &mut Boxes, x: &Interaction) -> Result<(), GradeError> {
    boxes.solution.reset_with_room(Room::for_files(&[&x.solution.program.dir])?)?;
    boxes.solution.put_tree(&x.solution.program.dir)?;
    boxes.helper.reset_with_room(Room::for_files(&[&x.interactor.dir, x.input, x.answer])?)?;
    boxes.helper.put_tree(&x.interactor.dir)?;
    boxes.helper.put("input", x.input, false)?;
    boxes.helper.put("answer", x.answer, false)?;
    Ok(())
}

pub fn interact(mut boxes: Boxes, x: &Interaction) -> Result<Interactive, GradeError> {
    prepare(&mut boxes, x)?;
    let solution = solution_spec(x);
    let interactor = interactor_spec(x, &solution.limits);
    let manager = MemberSpec { sandbox: &*boxes.helper, run: &interactor };
    let contestant = MemberSpec { sandbox: &*boxes.solution, run: &solution };
    let mut session = spawn_session(&manager, &contestant, ChannelPlan::StdioPipes)?;
    wait_all(&mut session, host_deadline(&interactor.limits))?;
    decide(&boxes, x, &solution.limits, &interactor.limits)
}

fn solution_result(boxes: &Boxes, x: &Interaction, limits: &Limits) -> Result<MemberRun, GradeError> {
    let meta = boxes.solution.read_meta()?;
    // The solution's stdin and stdout are pipes: its own files are its kept stderr alone.
    let own_files = Some(own_files_kib(boxes.solution, x.solution.program, 0, None));
    let verdict = classify(&meta, OutputUse { own_files_kib: own_files, ..OutputUse::UNMEASURED }, limits);
    if let RunVerdict::Internal(message) = &verdict {
        return Err(SandboxError::Isolate(message.clone()).into());
    }
    let verdict = refine(verdict, x.solution.program, boxes.solution);
    let memory_kib = reported_memory(&meta, own_files, &verdict);
    Ok(MemberRun { verdict, time_s: meta.time_s, memory_kib })
}

/// Copies the interactor's `tout` out of the helper box to `into`, before a checker run resets the box. An
/// interactor that left none is a problem fault (IE), not a sandbox failure to retry.
pub fn save_tout(helper: &IsolateBox, into: &Path) -> Result<(), GradeError> {
    if save_box_file(helper, TOUT, into)? {
        return Ok(());
    }
    Err(GradeError::Checker(format!("the interactor left no `{TOUT}` file for the checker")))
}

fn decide(
    boxes: &Boxes,
    x: &Interaction,
    limits: &Limits,
    interactor_limits: &Limits,
) -> Result<Interactive, GradeError> {
    let result = solution_result(boxes, x, limits)?;
    let interactor = boxes.helper.read_meta()?;
    if stalled_by_jury(&result, interactor.time_s, limits) {
        let used = interactor.time_s;
        return Err(GradeError::Checker(format!(
            "interactor used {used:.1} s of CPU; the solution timed out waiting"
        )));
    }
    let interactor_verdict = classify(&interactor, OutputUse::UNMEASURED, interactor_limits);
    let own = Interactive {
        verdict: result.verdict,
        time_s: result.time_s,
        jury_time_s: interactor.time_s,
        memory_kib: result.memory_kib,
        check: None,
    };
    if solution_wins(&own.verdict, &interactor_verdict) && !interactor_failed(&interactor_verdict) {
        return Ok(own);
    }
    let scale = PartialScale { mode: x.partial, case_points: x.points };
    let check = testlib_score(boxes.helper, &interactor_verdict, interactor.exit_code, scale)?;
    Ok(Interactive { check: Some(check), ..own })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_java_interactor_gets_a_heap_below_its_box_memory() {
        use crate::run_case::CaseLimits;
        let registry =
            crate::languages::LanguageRegistry::from_toml(include_str!("../../../languages.toml")).unwrap();
        let java = registry.get("JAVA8").unwrap();
        let cpp = registry.get("CPP17").unwrap();
        let interactor = crate::compile::program_for(java, Path::new("/i"), b"public class Interactor {}");
        let solution = crate::compile::program_for(cpp, Path::new("/s"), b"");
        let limits = CaseLimits { time_limit_s: 1.0, memory_kib: 262_144, jvm_reserve_kib: 0 };
        let runnable = Runnable { program: &solution, limits: &limits };
        let x = Interaction {
            solution: &runnable,
            interactor: &interactor,
            input: Path::new("/x/input"),
            answer: Path::new("/x/answer"),
            points: 1.0,
            partial: PartialPoints::Absolute,
        };
        let spec = interactor_spec(&x, &solution_limits(&runnable, SOLUTION_FILE_CAP));
        assert!(spec.argv.iter().all(|a| !a.contains('{')), "unexpanded placeholder: {:?}", spec.argv);
        let heap = crate::checkers::testing::heap_kib(&spec.argv);
        assert!(
            heap > 0 && heap < spec.limits.memory_kib,
            "heap {heap} KiB in a {} KiB box",
            spec.limits.memory_kib
        );
    }
}
