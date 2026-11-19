//! isolate wrapper.

pub mod box_dev;
pub mod boxes;
pub mod classify;
pub mod drain;
pub mod error;
pub mod fds;
pub mod isolate_box;
pub mod isolate_config;
pub mod memory_sampler;
pub mod meta;
pub mod pinned_input;
pub mod preload;
pub mod room;
pub mod run_gate;
pub mod sealed_input;
pub mod spec;

pub use error::SandboxError;
pub use isolate_box::{
    ChannelFds, DiskQuota, DrainedRun, Drains, IsolateBox, SandboxConfig, cleanup_box, disk_quota_works,
};
pub use room::Room;
pub use run_gate::{RunGate, RunPass};
