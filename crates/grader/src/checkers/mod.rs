//! Checkers: built-in ones run here over capped files; custom ones run in isolate (Task 19).

pub mod custom;
pub mod easy;
pub mod floats;
pub mod identical;
pub mod linecount;
pub mod linematches;
pub mod rstripped;
pub mod select;
pub mod sorted;
pub mod standard;
pub mod text;
pub mod themis;
pub mod tokens;

use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckError(pub String);

pub struct CheckInput<'a> {
    pub input: &'a Path,
    pub output: &'a Path,
    pub answer: &'a Path,
    /// What the case is worth; a custom checker's partial points are read against it.
    pub points: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CheckOutcome {
    pub passed: bool,
    /// Share of the case total earned, 0..=1.
    pub fraction: f64,
    pub feedback: String,
    pub extended: String,
}

impl CheckOutcome {
    pub fn pass() -> Self {
        CheckOutcome { passed: true, fraction: 1.0, feedback: String::new(), extended: String::new() }
    }

    pub fn fail(feedback: &str) -> Self {
        CheckOutcome { passed: false, fraction: 0.0, feedback: feedback.into(), extended: String::new() }
    }
}

pub trait Checker: Send + Sync {
    fn check(&self, io: &CheckInput) -> Result<CheckOutcome, CheckError>;
}

pub fn read_file(path: &Path) -> Result<Vec<u8>, CheckError> {
    std::fs::read(path).map_err(|e| CheckError(format!("{}: {e}", path.display())))
}

#[cfg(test)]
pub(crate) mod testing {
    use super::*;

    pub struct Files {
        _dir: tempfile::TempDir,
        pub input: std::path::PathBuf,
        pub output: std::path::PathBuf,
        pub answer: std::path::PathBuf,
    }

    pub fn files(output: &[u8], answer: &[u8]) -> Files {
        let dir = tempfile::tempdir().unwrap();
        let (input, out, ans) = (dir.path().join("in"), dir.path().join("out"), dir.path().join("ans"));
        std::fs::write(&input, b"").unwrap();
        std::fs::write(&out, output).unwrap();
        std::fs::write(&ans, answer).unwrap();
        Files { _dir: dir, input, output: out, answer: ans }
    }

    pub fn run(checker: &dyn Checker, output: &[u8], answer: &[u8]) -> CheckOutcome {
        let f = files(output, answer);
        checker
            .check(&CheckInput { input: &f.input, output: &f.output, answer: &f.answer, points: 1.0 })
            .unwrap()
    }
}
