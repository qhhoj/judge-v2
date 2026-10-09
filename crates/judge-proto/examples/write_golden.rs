//! Regenerates every `proto/golden/<name>.bin` from its `<name>.json`.
use std::{error::Error, fs};

use judge_proto::golden::{encode, fixture_names, golden_dir, read_fixture};

fn main() -> Result<(), Box<dyn Error>> {
    for name in fixture_names()? {
        let bytes = encode(&read_fixture(&name)?)?;
        fs::write(golden_dir().join(format!("{name}.bin")), bytes)?;
        println!("wrote {name}.bin");
    }
    Ok(())
}
