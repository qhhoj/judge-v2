//! isolate wrapper.

pub mod boxes;
pub mod classify;
pub mod error;
pub mod fds;
pub mod isolate_box;
pub mod meta;
pub mod room;
pub mod spec;

pub use error::SandboxError;
pub use isolate_box::{ChannelFds, DiskQuota, IsolateBox, SandboxConfig, cleanup_box, disk_quota_works};
pub use room::Room;
