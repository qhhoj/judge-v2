//! Whole-file helpers for the checkers that compare lines or tokens in memory.
//! Inputs are the capped output and answer files, so reading them whole is fine.

/// Splits on CR or LF. A final line break does not start another (empty) line.
pub fn split_lines(data: &[u8]) -> Vec<&[u8]> {
    let mut lines: Vec<&[u8]> = data.split(|b| matches!(b, b'\n' | b'\r')).collect();
    if lines.last().is_some_and(|last| last.is_empty()) {
        lines.pop();
    }
    lines
}

/// Whitespace-separated tokens (ASCII whitespace).
pub fn tokens(data: &[u8]) -> Vec<&[u8]> {
    data.split(|b| b.is_ascii_whitespace()).filter(|t| !t.is_empty()).collect()
}
