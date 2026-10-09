use super::tokens::{open, same_tokens};
use super::{CheckError, CheckInput, CheckOutcome, Checker};
use std::io::{self, BufRead};

pub const PRESENTATION_ERROR: &str = "Presentation Error, check your whitespace";

/// Exact bytes pass. Otherwise, when `pe_allowed` and only whitespace differs, the feedback is a PE.
pub struct Identical {
    pub pe_allowed: bool,
    pub unicode: bool,
}

impl Checker for Identical {
    fn check(&self, io: &CheckInput) -> Result<CheckOutcome, CheckError> {
        if same_bytes(io)? {
            return Ok(CheckOutcome::pass());
        }
        let whitespace_only = self.pe_allowed && same_tokens(io.answer, io.output, self.unicode)?;
        Ok(CheckOutcome::fail(if whitespace_only { PRESENTATION_ERROR } else { "" }))
    }
}

fn same_bytes(io: &CheckInput) -> Result<bool, CheckError> {
    let (mut want, mut got) = (open(io.answer)?, open(io.output)?);
    bytes_equal(&mut want, &mut got).map_err(|e| CheckError(e.to_string()))
}

/// Compares chunk by chunk, so memory stays at the reader buffer size.
fn bytes_equal(a: &mut impl BufRead, b: &mut impl BufRead) -> io::Result<bool> {
    loop {
        let (x, y) = (a.fill_buf()?, b.fill_buf()?);
        if x.is_empty() || y.is_empty() {
            return Ok(x.is_empty() && y.is_empty());
        }
        let n = x.len().min(y.len());
        if x[..n] != y[..n] {
            return Ok(false);
        }
        a.consume(n);
        b.consume(n);
    }
}

#[cfg(test)]
mod tests {
    use super::{Identical, bytes_equal};
    use crate::checkers::testing::run;
    use std::io::{BufReader, Cursor};

    #[test]
    fn exact_bytes_pass_whitespace_only_is_presentation_error() {
        let c = Identical { pe_allowed: true, unicode: false };
        assert!(run(&c, b"1 2\n", b"1 2\n").passed);
        let pe = run(&c, b"1  2\n", b"1 2\n");
        assert!(!pe.passed);
        assert_eq!(pe.feedback, "Presentation Error, check your whitespace");
        let strict = run(&Identical { pe_allowed: false, unicode: false }, b"1  2\n", b"1 2\n");
        assert_eq!((strict.passed, strict.feedback.as_str()), (false, ""));
        assert_eq!(run(&c, b"1 3\n", b"1 2\n").feedback, "");
    }

    #[test]
    fn unicode_hint_decides_whether_a_unicode_space_is_whitespace_for_pe() {
        let text = "a\u{3000}b\n";
        let uni = Identical { pe_allowed: true, unicode: true };
        assert_eq!(
            run(&uni, text.as_bytes(), b"a b\n").feedback,
            "Presentation Error, check your whitespace"
        );
        let ascii = Identical { pe_allowed: true, unicode: false };
        assert_eq!(run(&ascii, text.as_bytes(), b"a b\n").feedback, "");
    }

    #[test]
    fn byte_comparison_works_across_small_reads() {
        let mut a = BufReader::with_capacity(1, "abc".as_bytes());
        assert!(bytes_equal(&mut a, &mut Cursor::new(b"abc")).unwrap());
        let mut b = BufReader::with_capacity(1, "abd".as_bytes());
        assert!(!bytes_equal(&mut b, &mut Cursor::new(b"abc")).unwrap());
        let mut c = BufReader::with_capacity(1, "abcd".as_bytes());
        assert!(!bytes_equal(&mut c, &mut Cursor::new(b"abc")).unwrap(), "length differs");
    }
}
