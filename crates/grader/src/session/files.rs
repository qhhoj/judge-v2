//! Jury files carried out of the helper box: a checker run resets the box, so whatever the jury wrote for it
//! (`tout`, and later `output`, `nextpass`, `.qhhoj-session`) is copied to scratch first.

use crate::error::GradeError;
use crate::session::channel::{SESSION_PAGE, SESSION_PAGE_BYTES};
use judge_sandbox::IsolateBox;
use std::path::Path;

/// Copies the regular box file `name` to `into`. `Ok(false)` when `open_output` fails for any reason (the file
/// is absent, a symlink or a FIFO, or reading its metadata failed); the caller decides what that means. Errors
/// creating `into` or copying are `Err`.
// NOTE: later callers (split output, nextpass) must tell an I/O error apart from absence; today both are `false`.
pub fn save_box_file(helper: &IsolateBox, name: &str, into: &Path) -> Result<bool, GradeError> {
    let Ok(mut file) = helper.open_output(name) else { return Ok(false) };
    std::io::copy(&mut file, &mut std::fs::File::create(into)?)?;
    Ok(true)
}

/// The zeroed session page in the jury box: all zero = the jury has not reported yet. Mode 0666, so the jury
/// (box uid) opens it read-write and maps it shared; the daemon reads it back after the run.
pub fn stage_session_page(helper: &IsolateBox) -> Result<(), GradeError> {
    helper.put_writable(SESSION_PAGE, &[0u8; SESSION_PAGE_BYTES])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::fake_isolate::fake_box;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn the_session_page_is_a_zeroed_page_the_jury_can_write() {
        let dir = tempfile::tempdir().unwrap();
        let helper = fake_box(dir.path(), 7);
        stage_session_page(&helper).unwrap();
        let path = helper.dir().join(SESSION_PAGE);
        assert_eq!(std::fs::read(&path).unwrap(), vec![0u8; SESSION_PAGE_BYTES]);
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o666);
    }
}
