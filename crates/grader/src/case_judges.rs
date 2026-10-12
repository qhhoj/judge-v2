//! `CaseJudge` implementations: a program, an interactive program, or submitted outputs.
//!
//! Case files are opened once per case through the resolver's checked handles; checkers and the
//! interactor get `/proc/self/fd/N` paths of those handles, so nothing is looked up again by name.

use crate::checkers::custom::CustomChecker;
use crate::checkers::{CheckInput, CheckOutcome, Checker};
use crate::error::GradeError;
use crate::file_io::SolutionFileIo;
use crate::grade::{CaseJudge, CaseVerdict};
use crate::interactor::{Boxes, Interaction, interact};
use crate::output_only::{OutputSource, output_for_case};
use crate::program::Program;
use crate::run_case::{CaseIo, CaseLimits, Runnable, read_prefix, run_case};
use judge_problem::plan::PlannedCase;
use judge_problem::resolve::ResolvedProblem;
use judge_problem::verdict::Status;
use judge_sandbox::IsolateBox;
use judge_sandbox::classify::RunVerdict;
use judge_sandbox::error::SandboxError;
use std::fs::File;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};

/// Feedback of every case of an OUTPUT submission whose zip cannot be read (see `UnreadableOutputs`).
pub const UNREADABLE_ZIP: &str = "the submitted zip cannot be read";

pub enum Check {
    Builtin(Box<dyn Checker>),
    Custom(CustomChecker),
}

/// A failed run as a case verdict; Ok(None) when the program ran cleanly and the checker decides.
pub fn failed_run(
    verdict: &RunVerdict,
    time_s: f64,
    memory_kib: u64,
) -> Result<Option<CaseVerdict>, GradeError> {
    let (status, feedback) = match verdict {
        RunVerdict::Ok => return Ok(None),
        RunVerdict::Internal(message) => return Err(SandboxError::Isolate(message.clone()).into()),
        RunVerdict::Tle => (Status::Tle, String::new()),
        RunVerdict::Mle => (Status::Mle, String::new()),
        RunVerdict::Ole => (Status::Ole, String::new()),
        RunVerdict::Rte { reason } => (Status::Rte, reason.clone()),
        RunVerdict::Ir { exit_code } => (Status::Ir, format!("exit code {exit_code}")),
    };
    Ok(Some(CaseVerdict::failed(status, time_s, memory_kib, feedback)))
}

fn checked(outcome: CheckOutcome, time_s: f64, memory_kib: u64, output_prefix: String) -> CaseVerdict {
    CaseVerdict {
        status: if outcome.passed { Status::Ac } else { Status::Wa },
        fraction: outcome.fraction,
        time_s,
        memory_kib,
        feedback: outcome.feedback,
        extended: outcome.extended,
        output_prefix,
    }
}

/// The kernel's path of an open handle: the checked inode itself, valid while `file` lives.
pub fn fd_path(file: &File) -> PathBuf {
    PathBuf::from(format!("/proc/self/fd/{}", file.as_raw_fd()))
}

/// A case's input and answer, opened once, and what the case is worth.
struct CaseFiles {
    input: File,
    answer: File,
    points: f64,
}

fn open_case(problem: &ResolvedProblem, case: &PlannedCase) -> Result<CaseFiles, GradeError> {
    Ok(CaseFiles {
        input: problem.open_case_file(&case.input)?,
        answer: problem.open_case_file(&case.output)?,
        points: case.total,
    })
}

/// Shared by the program and output judges: the problem's files, the checker and the scratch directory.
pub struct CaseContext<'a> {
    pub problem: &'a ResolvedProblem,
    pub check: Check,
    pub scratch: PathBuf,
}

impl CaseContext<'_> {
    fn output_path(&self) -> PathBuf {
        self.scratch.join("output")
    }

    fn check(
        &self,
        helper: &mut IsolateBox,
        files: &CaseFiles,
        output: &Path,
    ) -> Result<CheckOutcome, GradeError> {
        let (input, answer) = (fd_path(&files.input), fd_path(&files.answer));
        let io = CheckInput { input: &input, output, answer: &answer, points: files.points };
        match &self.check {
            Check::Builtin(checker) => Ok(checker.check(&io)?),
            Check::Custom(custom) => custom.check(helper, &io),
        }
    }
}

pub struct ProgramCases<'a> {
    pub context: CaseContext<'a>,
    pub boxes: Boxes<'a>,
    pub program: Program,
    pub limits: CaseLimits,
    pub file_io: Option<SolutionFileIo>,
}

impl CaseJudge for ProgramCases<'_> {
    fn judge_case(&mut self, case: &PlannedCase) -> Result<CaseVerdict, GradeError> {
        let files = open_case(self.context.problem, case)?;
        let output = self.context.output_path();
        let io =
            CaseIo { input: &files.input, output_cap: case.output_limit, file_io: self.file_io.as_ref() };
        let runnable = Runnable { program: &self.program, limits: &self.limits };
        let run = run_case(self.boxes.solution, &runnable, &io, &output)?;
        if let Some(failed) = failed_run(&run.verdict, run.time_s, run.memory_kib)? {
            return Ok(failed);
        }
        let outcome = self.context.check(self.boxes.helper, &files, &output)?;
        Ok(checked(outcome, run.time_s, run.memory_kib, read_prefix(&output, case.output_prefix)))
    }
}

pub struct InteractiveCases<'a> {
    pub problem: &'a ResolvedProblem,
    pub boxes: Boxes<'a>,
    pub program: Program,
    pub interactor: Program,
    pub limits: CaseLimits,
}

impl CaseJudge for InteractiveCases<'_> {
    fn judge_case(&mut self, case: &PlannedCase) -> Result<CaseVerdict, GradeError> {
        let files = open_case(self.problem, case)?;
        let (input, answer) = (fd_path(&files.input), fd_path(&files.answer));
        let runnable = Runnable { program: &self.program, limits: &self.limits };
        let x = Interaction {
            solution: &runnable,
            interactor: &self.interactor,
            input: &input,
            answer: &answer,
            points: case.total,
        };
        let boxes = Boxes { solution: &mut *self.boxes.solution, helper: &mut *self.boxes.helper };
        let result = interact(boxes, &x)?;
        // The interactor's verdict, when `interact` kept it, decides: also for a solution killed by SIGPIPE
        // after the interactor had finished. The solution's stdout is a pipe, so there is no output prefix.
        if let Some(outcome) = result.check {
            return Ok(checked(outcome, result.time_s, result.memory_kib, String::new()));
        }
        let failed = failed_run(&result.verdict, result.time_s, result.memory_kib)?;
        failed.ok_or_else(|| GradeError::Checker("interactor gave no verdict".into()))
    }
}

/// TEXT and OUTPUT submissions: no program runs, so time and memory are 0.
pub struct OutputCases<'a> {
    pub context: CaseContext<'a>,
    pub helper: &'a mut IsolateBox,
    pub source: OutputSource,
}

impl CaseJudge for OutputCases<'_> {
    fn judge_case(&mut self, case: &PlannedCase) -> Result<CaseVerdict, GradeError> {
        let output = self.context.output_path();
        let written = match output_for_case(&self.source, &case.output, &output, case.output_limit) {
            Err(GradeError::UnreadableSubmission(_)) => return Ok(unreadable_output()),
            other => other?,
        };
        if written > case.output_limit {
            return Ok(CaseVerdict::failed(Status::Ole, 0.0, 0, String::new()));
        }
        let files = open_case(self.context.problem, case)?;
        let outcome = self.context.check(self.helper, &files, &output)?;
        Ok(checked(outcome, 0.0, 0, read_prefix(&output, case.output_prefix)))
    }
}

/// WA with `UNREADABLE_ZIP`: the contestant's mistake, so not IE (spec §2 keeps IE for judge faults).
fn unreadable_output() -> CaseVerdict {
    CaseVerdict::failed(Status::Wa, 0.0, 0, UNREADABLE_ZIP.into())
}

/// An OUTPUT submission that is not a zip at all, or whose directory is corrupt: every case is
/// `unreadable_output`. A single entry that cannot be inflated (unsupported method, encryption, bad
/// checksum) makes only its case `unreadable_output`; a failed scratch write stays an error (IE).
pub struct UnreadableOutputs;

impl CaseJudge for UnreadableOutputs {
    fn judge_case(&mut self, _case: &PlannedCase) -> Result<CaseVerdict, GradeError> {
        Ok(unreadable_output())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn fd_path_reads_the_opened_file_even_after_the_name_moves() {
        let dir = tempfile::tempdir().unwrap();
        let (path, moved) = (dir.path().join("a"), dir.path().join("b"));
        std::fs::write(&path, "original").unwrap();
        let file = File::open(&path).unwrap();
        std::fs::rename(&path, &moved).unwrap();
        std::fs::write(&path, "replacement").unwrap();
        let mut text = String::new();
        File::open(fd_path(&file)).unwrap().read_to_string(&mut text).unwrap();
        assert_eq!(text, "original");
    }
}
