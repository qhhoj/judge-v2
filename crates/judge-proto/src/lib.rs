//! Rust types of `proto/qhhoj/judge/v1/judge.proto`: prost messages, the tonic
//! client and server of `JudgeLink`, and proto3-JSON serde (pbjson).

/// Package `qhhoj.judge.v1` (generated; exempt from the lints that hold for hand-written code).
#[allow(clippy::all, clippy::too_many_lines, clippy::cognitive_complexity, clippy::too_many_arguments)]
pub mod v1 {
    tonic::include_proto!("qhhoj.judge.v1");
    include!(concat!(env!("OUT_DIR"), "/qhhoj.judge.v1.serde.rs"));
}

/// The encoded `FileDescriptorSet` of the contract, for the compatibility check.
pub const FILE_DESCRIPTOR_SET: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/judge_descriptor.bin"));

pub mod golden;
pub mod shape;

/// `proto/version.json`: bumped with every additive change inside v1 (`Hello`/`Welcome.protocol_minor`).
pub const PROTOCOL_MINOR: u32 = 1;
