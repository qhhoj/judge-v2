//! Generates the prost/tonic types and their proto3-JSON serde impls from `proto/`.
use std::{env, error::Error, fs, path::PathBuf};

const PROTO_ROOT: &str = "../../proto";
const PROTO_FILE: &str = "../../proto/qhhoj/judge/v1/judge.proto";

fn main() -> Result<(), Box<dyn Error>> {
    let descriptor = PathBuf::from(env::var("OUT_DIR")?).join("judge_descriptor.bin");
    tonic_prost_build::configure()
        .file_descriptor_set_path(&descriptor)
        .compile_protos(&[PROTO_FILE], &[PROTO_ROOT])?;
    pbjson_build::Builder::new()
        .register_descriptors(&fs::read(&descriptor)?)?
        .build(&[".qhhoj.judge.v1"])?;
    println!("cargo:rerun-if-changed={PROTO_ROOT}");
    Ok(())
}
