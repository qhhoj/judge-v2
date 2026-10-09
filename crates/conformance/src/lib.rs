//! The protocol conformance suite: scenario files (`proto/conformance/scenarios`),
//! the matcher both repos share, and transcript players for each role.
pub mod expand;
pub mod matcher;
pub mod scenario;

pub use scenario::{Role, Scenario, Step};
