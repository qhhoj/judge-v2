use crate::languages::ExceptionStyle;
use judge_problem::dir_cache::CacheLease;
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq)]
pub struct SourceFile {
    pub name: String,
    pub bytes: Vec<u8>,
}

/// A runnable program: a snapshot directory copied into a fresh box before each run.
#[derive(Debug, Clone)]
pub struct Program {
    pub dir: PathBuf,
    pub run: Vec<String>,
    pub processes: u32,
    pub jvm: bool,
    pub dirs: Vec<String>,
    pub env: Vec<(String, String)>,
    /// From the language: stderr text of an uncaught exception.
    pub exception_marker: Option<String>,
    pub exception_style: ExceptionStyle,
    /// Holds a cached artefact (checker, interactor) alive while in use.
    pub lease: Option<Arc<CacheLease>>,
}
