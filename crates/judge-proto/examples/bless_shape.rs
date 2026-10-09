//! Writes the current shape to `proto/compat/v1.shape.json`. Refuses when the contract
//! changed without a bump of `proto/version.json`, and refuses a breaking change
//! unless `--allow-breaking` is given (v1 never breaks; the flag is for shape-file migrations).
use std::{error::Error, fs, path::Path};

use judge_proto::shape::{Shape, bless_problems, shape_of};

fn main() -> Result<(), Box<dyn Error>> {
    let allow_breaking = std::env::args().any(|argument| argument == "--allow-breaking");
    let current = shape_of(judge_proto::FILE_DESCRIPTOR_SET)?;
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../proto/compat/v1.shape.json");
    if let Ok(text) = fs::read_to_string(&path) {
        let stored: Shape = serde_json::from_str(&text)?;
        bless_problems(&stored, &current, allow_breaking)?;
    }
    fs::create_dir_all(path.parent().expect("has a parent"))?;
    fs::write(&path, serde_json::to_string_pretty(&current)? + "\n")?;
    println!("wrote {}", path.display());
    Ok(())
}
