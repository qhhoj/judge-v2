#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SandboxError {
    #[error("isolate failed: {0}")]
    Isolate(String),
    #[error("sandbox i/o: {0}")]
    Io(String),
    #[error("not a plain box file name: {0:?}")]
    BadName(String),
    #[error("box was already used; reset() before running again")]
    Dirty,
    #[error("bad meta file: {0}")]
    Meta(String),
}

impl From<std::io::Error> for SandboxError {
    fn from(error: std::io::Error) -> Self {
        SandboxError::Io(error.to_string())
    }
}
