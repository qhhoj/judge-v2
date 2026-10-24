//! The protocol conformance suite: scenario files (`proto/conformance/scenarios`),
//! the matcher both repos share, and transcript players for each role.
pub mod expand;
pub mod fake_bridge;
pub mod host_stall;
pub mod matcher;
pub mod play;
pub mod scenario;
pub mod transcript_judge;

pub use scenario::{Role, Scenario, Step};
