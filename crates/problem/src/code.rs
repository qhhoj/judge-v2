//! A problem code becomes a path only after this check (backend `PROBLEM_CODE_PATTERN`).

use crate::data_version::DataError;
use std::path::{Path, PathBuf};

fn is_valid_code(code: &str) -> bool {
    (1..=32).contains(&code.len())
        && code.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

pub fn problem_dir(root: &Path, code: &str) -> Result<PathBuf, DataError> {
    if !is_valid_code(code) {
        return Err(DataError::BadCode(code.to_string()));
    }
    Ok(root.join(code))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_codes_map_under_the_root() {
        assert_eq!(problem_dir(Path::new("/data"), "e2e_sum").unwrap(), Path::new("/data/e2e_sum"));
    }

    #[test]
    fn traversal_and_odd_codes_are_refused() {
        for code in ["", "..", "a/b", "../etc", "A", "a.b", "x".repeat(33).as_str(), "a b", ".data-version"] {
            assert!(matches!(problem_dir(Path::new("/data"), code), Err(DataError::BadCode(_))), "{code:?}");
        }
    }
}
