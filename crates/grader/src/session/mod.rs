//! Sessions: one manager (jury) process and one contestant process run together, joined by a channel the daemon
//! creates and immediately lets go of. Interactive problems are the first preset (`crate::interactor`); split
//! signature and Communication follow.
//!
//! Where things live: spawning, waiting and killing in `run`; how a finished pair becomes a verdict in `verdict`
//! (one predicate per rule); reading a manager's exit as a score in `score`; carrying a jury file out of the
//! helper box before a checker resets it in `files`.

pub mod files;
pub mod run;
pub mod score;
pub mod verdict;

use judge_sandbox::IsolateBox;

/// The worker's two boxes: the contestant runs in `solution`, every jury program in `helper`.
pub struct Boxes<'a> {
    pub solution: &'a mut IsolateBox,
    pub helper: &'a mut IsolateBox,
}
