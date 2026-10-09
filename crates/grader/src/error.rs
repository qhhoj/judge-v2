use judge_problem::data_version::DataError;
use judge_sandbox::error::SandboxError;

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum GradeError {
    #[error(transparent)]
    Sandbox(#[from] SandboxError),
    #[error(transparent)]
    Data(#[from] DataError),
    #[error("i/o: {0}")]
    Io(String),
    #[error("checker: {0}")]
    Checker(String),
    #[error("unsupported: {0}")]
    Unsupported(String),
    #[error("runtime missing: {0}")]
    RuntimeMissing(String),
    #[error("aborted")]
    Aborted,
    /// The submission itself cannot be read (an OUTPUT zip entry with an unsupported method, encryption or a
    /// bad checksum): the contestant's fault, graded as WA by the caller, never a host failure.
    #[error("unreadable submission: {0}")]
    UnreadableSubmission(String),
}

impl From<std::io::Error> for GradeError {
    fn from(error: std::io::Error) -> Self {
        GradeError::Io(error.to_string())
    }
}

impl From<crate::checkers::CheckError> for GradeError {
    fn from(error: crate::checkers::CheckError) -> Self {
        GradeError::Checker(error.0)
    }
}
