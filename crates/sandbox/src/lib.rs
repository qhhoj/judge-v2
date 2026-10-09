//! isolate wrapper.

pub mod boxes;
pub mod classify;
pub mod error;
pub mod isolate_box;
pub mod meta;
pub mod spec;

pub use error::SandboxError;
pub use isolate_box::{IsolateBox, SandboxConfig, cleanup_box};
