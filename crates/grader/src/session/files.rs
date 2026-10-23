//! Jury files carried out of the helper box: a checker run resets the box, so whatever the jury wrote for it
//! (`tout`, and later `output`, `nextpass`, `.qhhoj-session`) is copied to scratch first.

use crate::error::GradeError;
use judge_sandbox::IsolateBox;
use std::path::Path;

/// Copies the regular box file `name` to `into`. `Ok(false)` when the program left no such regular file
/// (absent, a symlink or a FIFO: `open_output` refuses those); the caller decides what that means.
pub fn save_box_file(helper: &IsolateBox, name: &str, into: &Path) -> Result<bool, GradeError> {
    let Ok(mut file) = helper.open_output(name) else { return Ok(false) };
    std::io::copy(&mut file, &mut std::fs::File::create(into)?)?;
    Ok(true)
}
