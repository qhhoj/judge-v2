//! `check_shape <released shape file>`: exits 1 when the current contract breaks the shape
//! of a released tag. CI feeds it `git show <latest proto-v1.* tag>:proto/compat/v1.shape.json`.
use std::{error::Error, fs, process::ExitCode};

use judge_proto::shape::{Shape, breaking_changes, shape_of};

fn main() -> Result<ExitCode, Box<dyn Error>> {
    let path = std::env::args().nth(1).ok_or("usage: check_shape <released shape file>")?;
    let released: Shape = serde_json::from_str(&fs::read_to_string(path)?)?;
    let problems = breaking_changes(&released, &shape_of(judge_proto::FILE_DESCRIPTOR_SET)?);
    problems.iter().for_each(|problem| eprintln!("{problem}"));
    Ok(if problems.is_empty() { ExitCode::SUCCESS } else { ExitCode::FAILURE })
}
