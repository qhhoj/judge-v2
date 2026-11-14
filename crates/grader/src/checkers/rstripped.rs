use super::text::{lines, trim_space_end};
use super::{CheckError, CheckInput, CheckOutcome, Checker, read_file};

/// Same lines once trailing whitespace is removed from each; empty lines are kept unless filtered.
pub struct RStripped {
    pub filter_new_line: bool,
}

impl RStripped {
    /// Each line without trailing whitespace, lazily, the empty ones dropped when `filter_new_line`.
    fn lines<'a>(&self, data: &'a [u8]) -> impl Iterator<Item = &'a [u8]> {
        let filter = self.filter_new_line;
        lines(data).map(trim_space_end).filter(move |line| !(filter && line.is_empty()))
    }
}

impl Checker for RStripped {
    fn check(&self, io: &CheckInput) -> Result<CheckOutcome, CheckError> {
        let (got, want) = (read_file(io.output)?, read_file(io.answer)?);
        let same = self.lines(&got).eq(self.lines(&want));
        Ok(if same { CheckOutcome::pass() } else { CheckOutcome::fail("") })
    }
}

#[cfg(test)]
mod tests {
    use super::RStripped;
    use crate::checkers::testing::run;

    #[test]
    fn trailing_whitespace_is_ignored_but_leading_and_line_structure_are_not() {
        let keep = RStripped { filter_new_line: false };
        assert!(run(&keep, b"a  \nb\t\n", b"a\nb").passed);
        assert!(run(&keep, b"a\x0b\n", b"a\n").passed, "vertical tab is trailing whitespace");
        assert!(!run(&keep, b" a\n", b"a\n").passed, "leading whitespace counts");
        assert!(!run(&keep, b"a\n\nb\n", b"a\nb\n").passed, "empty lines are kept by default");
        assert!(!run(&keep, b"a\n", b"a\nb\n").passed);
    }

    #[test]
    fn filter_new_line_drops_empty_lines_first() {
        let filter = RStripped { filter_new_line: true };
        assert!(run(&filter, b"a\n\n  \nb\n", b"a\nb\n").passed);
        assert!(!run(&filter, b"a\n", b"a\nb\n").passed);
    }

    #[test]
    fn crlf_answer_matches_lf_output() {
        let keep = RStripped { filter_new_line: false };
        assert!(run(&keep, b"1 2\n3\n", b"1 2\r\n3\r\n").passed, "CRLF answer vs LF output");
        assert!(run(&keep, b"1 2\r\n3\r\n", b"1 2\n3\n").passed, "CRLF output vs LF answer");
    }
}
