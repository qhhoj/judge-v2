use super::text::trim_space;
use super::tokens::open;
use super::{CheckError, CheckInput, CheckOutcome, Checker};
use std::io::{self, BufRead};

/// Compares non-empty lines, each trimmed. Partial points per matching line; any extra line earns nothing.
pub struct LineCount;

/// Reads the non-empty, trimmed lines of a stream one at a time. Lines end at `\n` or `\r`.
pub struct Lines<R> {
    reader: R,
    line: Vec<u8>,
}

impl<R: BufRead> Lines<R> {
    pub fn new(reader: R) -> Self {
        Lines { reader, line: Vec::new() }
    }

    pub fn next_line(&mut self) -> io::Result<Option<&[u8]>> {
        loop {
            self.line.clear();
            if !self.read_segment()? {
                return Ok(None);
            }
            if !trim_space(&self.line).is_empty() {
                return Ok(Some(trim_space(&self.line)));
            }
        }
    }

    /// Appends bytes up to the next line break. False when the input ended with nothing read.
    fn read_segment(&mut self) -> io::Result<bool> {
        let mut read_any = false;
        loop {
            let buf = self.reader.fill_buf()?;
            if buf.is_empty() {
                return Ok(read_any);
            }
            read_any = true;
            let end = buf.iter().position(|b| matches!(b, b'\n' | b'\r'));
            let take = end.unwrap_or(buf.len());
            self.line.extend_from_slice(&buf[..take]);
            self.reader.consume(end.map_or(take, |i| i + 1));
            if end.is_some() {
                return Ok(true);
            }
        }
    }
}

impl Checker for LineCount {
    fn check(&self, io: &CheckInput) -> Result<CheckOutcome, CheckError> {
        let mut got = Lines::new(open(io.output)?);
        let mut want = Lines::new(open(io.answer)?);
        let (matched, total, extra) = compare(&mut got, &mut want).map_err(|e| CheckError(e.to_string()))?;
        Ok(outcome(matched, total, extra))
    }
}

fn compare<A: BufRead, B: BufRead>(
    got: &mut Lines<A>,
    want: &mut Lines<B>,
) -> io::Result<(usize, usize, bool)> {
    let (matched, total) = count_matches(got, want)?;
    let extra = got.next_line()?.is_some();
    Ok((matched, total, extra))
}

fn count_matches<A: BufRead, B: BufRead>(
    got: &mut Lines<A>,
    want: &mut Lines<B>,
) -> io::Result<(usize, usize)> {
    let (mut matched, mut total) = (0, 0);
    while let Some(expected) = want.next_line()? {
        total += 1;
        if got.next_line()?.is_some_and(|line| line == expected) {
            matched += 1;
        }
    }
    Ok((matched, total))
}

fn outcome(matched: usize, total: usize, extra: bool) -> CheckOutcome {
    if extra {
        return CheckOutcome::fail("");
    }
    let fraction = if total == 0 { 1.0 } else { matched as f64 / total as f64 };
    CheckOutcome {
        passed: matched == total,
        fraction,
        feedback: String::new(),
        extended: String::new(),
        jury_time_s: 0.0,
    }
}

#[cfg(test)]
mod tests {
    use super::LineCount;
    use crate::checkers::testing::run;

    #[test]
    fn partial_points_per_trimmed_line_and_extra_lines_earn_nothing() {
        // (output, answer, passed, fraction)
        let table: [(&[u8], &[u8], bool, f64); 8] = [
            (b"a\n b \nx\n", b"a\nb\nc\nd\n", false, 0.5),
            (b"a\nb\n\n\n", b"a\nb\n", true, 1.0), // trailing blank lines are ignored
            (b"a\nb\nzzz\n", b"a\nb\n", false, 0.0), // an extra line earns nothing
            (b"a\n\nb\n", b"a\nb\n", true, 1.0),   // blank lines in the middle are dropped
            (b"a\rb\r", b"a\nb\n", true, 1.0),     // carriage return also ends a line
            (b"\n\n", b"", true, 1.0),
            (b"x", b"", false, 0.0),
            (b"\x0ba\x0b\n", b"a\n", true, 1.0), // vertical tab is trimmed like other whitespace
        ];
        for (output, answer, passed, fraction) in table {
            let r = run(&LineCount, output, answer);
            assert_eq!(r.passed, passed, "{output:?} vs {answer:?}");
            assert!((r.fraction - fraction).abs() < 1e-9, "{output:?} vs {answer:?}: {}", r.fraction);
        }
    }
}
