//! Split signature (`signature_grader.split`): the jury entry and the contestant's functions run in two boxes,
//! joined by two pipes, with C++ glue generated from the problem header (`judge_interface`); the contestant box
//! holds only the contestant's program (docs/grading.md#split-signature).
//!
//! Where things live: what the build reads from the problem in `sources`; the jury build (roles from the
//! compiled entry, cached per language key) in `jury`; the contestant build in `contestant`; `nm` listings in
//! `symbols`; one case run in `run`. The verdict rules are `session::verdict::decide_split`.

pub mod contestant;
pub mod jury;
pub mod run;
pub mod sources;
pub mod symbols;

/// Compile-error text for a submission in a C language to a split signature problem (the interface is C++).
pub const SPLIT_LANGUAGES: &str = "This problem accepts only C++ submissions.";
/// The file the contestant's source is compiled from (the one-process grader uses the same name).
pub const USER_SOURCE: &str = "user.cpp";

/// The language's `signature` compiler as a shell command prefix.
pub(crate) fn compiler_command(compiler: &[String]) -> String {
    compiler.join(" ")
}
