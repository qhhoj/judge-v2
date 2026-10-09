//! Writes the current shape to `proto/compat/v1.shape.json`. Run it only after
//! `cargo test -p judge-proto --test compat` passes, i.e. for additive changes.
use std::{error::Error, fs, path::Path};

fn main() -> Result<(), Box<dyn Error>> {
    let shape = judge_proto::shape::shape_of(judge_proto::FILE_DESCRIPTOR_SET)?;
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../proto/compat/v1.shape.json");
    fs::create_dir_all(path.parent().expect("has a parent"))?;
    fs::write(&path, serde_json::to_string_pretty(&shape)? + "\n")?;
    println!("wrote {}", path.display());
    Ok(())
}
