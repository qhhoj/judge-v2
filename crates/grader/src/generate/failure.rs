//! Why a generation stopped, in the terms the protocol reports.

use crate::error::GradeError;
use judge_sandbox::classify::RunVerdict;
use judge_sandbox::error::SandboxError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Compile,
    Generate,
    Validate,
    Solve,
    /// The job as a whole: fetch, deadline, caps, upload, cancel.
    Job,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    CompileError,
    Crashed,
    TimeLimit,
    MemoryLimit,
    OutputLimit,
    Rejected,
    MissingFile,
    UnsupportedLanguage,
    JobTimeLimit,
    Internal,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenerationFailure {
    pub stage: Stage,
    pub kind: Kind,
    /// The program's name, empty for job-level failures.
    pub program: String,
    /// 0 when the failure is not about one test.
    pub test_index: u32,
    pub detail: String,
}

/// The detail of a generation stopped by `cancel`.
pub const CANCELLED: &str = "cancelled";

impl GenerationFailure {
    pub fn new(stage: Stage, kind: Kind, detail: impl Into<String>) -> Self {
        GenerationFailure { stage, kind, program: String::new(), test_index: 0, detail: detail.into() }
    }

    pub fn job(kind: Kind, detail: impl Into<String>) -> Self {
        Self::new(Stage::Job, kind, detail)
    }

    pub fn internal(detail: impl Into<String>) -> Self {
        Self::job(Kind::Internal, detail)
    }

    pub fn cancelled() -> Self {
        Self::internal(CANCELLED)
    }

    pub fn of_program(mut self, name: &str) -> Self {
        self.program = name.to_string();
        self
    }

    pub fn at_test(mut self, index: u32) -> Self {
        self.test_index = index;
        self
    }

    pub fn is_cancelled(&self) -> bool {
        self.stage == Stage::Job && self.kind == Kind::Internal && self.detail == CANCELLED
    }
}

impl From<GradeError> for GenerationFailure {
    fn from(error: GradeError) -> Self {
        match error {
            GradeError::Aborted => Self::cancelled(),
            other => Self::internal(other.to_string()),
        }
    }
}

impl From<SandboxError> for GenerationFailure {
    fn from(error: SandboxError) -> Self {
        Self::internal(error.to_string())
    }
}

impl From<std::io::Error> for GenerationFailure {
    fn from(error: std::io::Error) -> Self {
        Self::internal(format!("i/o: {error}"))
    }
}

impl From<zip::result::ZipError> for GenerationFailure {
    fn from(error: zip::result::ZipError) -> Self {
        Self::internal(format!("zip: {error}"))
    }
}

/// How a run that ended badly is reported; `None` for a clean exit.
pub fn verdict_failure(verdict: &RunVerdict) -> Option<(Kind, String)> {
    match verdict {
        RunVerdict::Ok => None,
        RunVerdict::Tle => Some((Kind::TimeLimit, "time limit exceeded".into())),
        RunVerdict::Mle => Some((Kind::MemoryLimit, "memory limit exceeded".into())),
        RunVerdict::Ole => Some((Kind::OutputLimit, "output limit exceeded".into())),
        RunVerdict::Rte { reason } => Some((Kind::Crashed, signal_detail(reason))),
        RunVerdict::Ir { exit_code } => Some((Kind::Crashed, format!("exit code {exit_code}"))),
        RunVerdict::Internal(message) => Some((Kind::Internal, message.clone())),
    }
}

/// `classify` names a known signal `SIGSEGV` and an unknown one `signal 5`.
fn signal_detail(reason: &str) -> String {
    if reason.starts_with("SIG") { format!("signal {reason}") } else { reason.to_string() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_verdicts_map_to_failure_kinds_with_their_detail() {
        let kind = |verdict| verdict_failure(&verdict);
        assert_eq!(kind(RunVerdict::Ok), None);
        assert_eq!(kind(RunVerdict::Tle).unwrap().0, Kind::TimeLimit);
        assert_eq!(kind(RunVerdict::Mle).unwrap().0, Kind::MemoryLimit);
        assert_eq!(kind(RunVerdict::Ole).unwrap().0, Kind::OutputLimit);
        assert_eq!(
            kind(RunVerdict::Rte { reason: "SIGSEGV".into() }),
            Some((Kind::Crashed, "signal SIGSEGV".into()))
        );
        assert_eq!(
            kind(RunVerdict::Rte { reason: "signal 5".into() }),
            Some((Kind::Crashed, "signal 5".into()))
        );
        assert_eq!(kind(RunVerdict::Ir { exit_code: 3 }), Some((Kind::Crashed, "exit code 3".into())));
    }

    #[test]
    fn only_the_cancel_failure_is_a_cancellation() {
        assert!(GenerationFailure::cancelled().is_cancelled());
        assert!(GenerationFailure::from(GradeError::Aborted).is_cancelled());
        assert!(!GenerationFailure::internal("boom").is_cancelled());
        assert!(!GenerationFailure::new(Stage::Solve, Kind::Internal, CANCELLED).is_cancelled());
    }
}
