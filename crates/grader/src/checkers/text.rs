//! Whole-file helpers for the checkers that compare lines or tokens in memory.
//! Inputs are the capped output and answer files, so reading them whole is fine.

/// Splits on CRLF, LF or a lone CR (each one break). A final line break does not start another (empty) line.
pub fn split_lines(data: &[u8]) -> Vec<&[u8]> {
    let mut lines: Vec<&[u8]> = data
        .split(|b| *b == b'\n')
        .flat_map(|line| line.strip_suffix(b"\r").unwrap_or(line).split(|b| *b == b'\r'))
        .collect();
    if lines.last().is_some_and(|last| last.is_empty()) {
        lines.pop();
    }
    lines
}

/// The legacy checkers' byte whitespace (C `isspace`): space, \t, \n, \v, \f, \r.
/// `u8::is_ascii_whitespace` leaves out \v.
pub fn is_space(b: u8) -> bool {
    b.is_ascii_whitespace() || b == 0x0B
}

/// `data` without trailing whitespace.
pub fn trim_space_end(data: &[u8]) -> &[u8] {
    let end = data.iter().rposition(|b| !is_space(*b)).map_or(0, |i| i + 1);
    &data[..end]
}

/// `data` without leading and trailing whitespace.
pub fn trim_space(data: &[u8]) -> &[u8] {
    let trimmed = trim_space_end(data);
    let start = trimmed.iter().position(|b| !is_space(*b)).unwrap_or(trimmed.len());
    &trimmed[start..]
}

/// Whitespace-separated tokens.
pub fn tokens(data: &[u8]) -> Vec<&[u8]> {
    data.split(|b| is_space(*b)).filter(|t| !t.is_empty()).collect()
}

#[cfg(test)]
mod tests {
    use super::split_lines;

    #[test]
    fn crlf_lf_and_a_lone_cr_are_each_one_line_break() {
        let table: [(&[u8], &[&[u8]]); 6] = [
            (b"a\r\nb\r\n", &[b"a", b"b"]),
            (b"a\nb", &[b"a", b"b"]),
            (b"a\rb\r", &[b"a", b"b"]),
            (b"a\n\nb", &[b"a", b"", b"b"]),
            (b"a\r\n\r\n", &[b"a", b""]),
            (b"a\r\r\n", &[b"a", b""]),
        ];
        for (data, lines) in table {
            assert_eq!(split_lines(data), lines, "{:?}", String::from_utf8_lossy(data));
        }
    }
}
