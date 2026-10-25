use super::text::is_space;
use super::{CheckError, CheckInput, CheckOutcome, Checker, read_file};

/// Ignores case, whitespace and order: AC when both hold the same multiset of bytes.
pub struct Easy;

fn byte_counts(data: &[u8]) -> [usize; 256] {
    let mut counts = [0; 256];
    for b in data.iter().filter(|b| !is_space(**b)) {
        counts[usize::from(b.to_ascii_lowercase())] += 1;
    }
    counts
}

impl Checker for Easy {
    fn check(&self, io: &CheckInput) -> Result<CheckOutcome, CheckError> {
        let same = byte_counts(&read_file(io.output)?) == byte_counts(&read_file(io.answer)?);
        Ok(if same { CheckOutcome::pass() } else { CheckOutcome::fail("") })
    }
}

#[cfg(test)]
mod tests {
    use super::Easy;
    use crate::checkers::testing::run;

    #[test]
    fn case_whitespace_and_order_are_ignored_but_byte_counts_are_not() {
        assert!(run(&Easy, b"Yes\n", b"s e Y").passed);
        assert!(run(&Easy, b"a\x0bb", b"ab").passed, "vertical tab is whitespace");
        assert!(!run(&Easy, b"aab", b"abb").passed);
        assert!(!run(&Easy, b"ab", b"abb").passed);
        assert!(run(&Easy, b"\xff\xfe", b"\xfe\xff").passed, "non-ASCII bytes are compared as bytes");
    }
}
