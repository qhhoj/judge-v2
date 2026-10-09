use super::tokens::same_tokens;
use super::{CheckError, CheckInput, CheckOutcome, Checker};

/// Tokens must match in order and on the same lines; blank lines and trailing whitespace are ignored.
pub struct Standard {
    pub unicode: bool,
}

impl Checker for Standard {
    fn check(&self, io: &CheckInput) -> Result<CheckOutcome, CheckError> {
        let same = same_tokens(io.answer, io.output, self.unicode)?;
        Ok(if same { CheckOutcome::pass() } else { CheckOutcome::fail("") })
    }
}

#[cfg(test)]
mod tests {
    use super::Standard;
    use crate::checkers::testing::run;

    fn passes(output: &[u8], answer: &[u8]) -> bool {
        run(&Standard { unicode: false }, output, answer).passed
    }

    #[test]
    fn whitespace_differences_pass_and_token_differences_fail() {
        let c = Standard { unicode: false };
        assert!(run(&c, b"1  2\n3\n", b"1 2\n3").passed);
        assert!(!run(&c, b"1 2 4", b"1 2 3").passed);
        assert!(!run(&c, b"1 2", b"1 2 3").passed, "missing token");
        assert!(!run(&c, b"1 2 3 4", b"1 2 3").passed, "extra token");
        assert_eq!(run(&c, b"1 2 3", b"1 2 3").fraction, 1.0);
        assert_eq!(run(&c, b"x", b"y").fraction, 0.0);
    }

    #[test]
    fn whitespace_inside_a_line_collapses() {
        assert!(passes(b"1   2\t3", b"1 2 3"));
    }

    #[test]
    fn case_must_match() {
        assert!(!passes(b"Yes", b"yes"));
    }

    #[test]
    fn extra_trailing_newlines_and_spaces_are_ignored() {
        assert!(passes(b"1 2\n\n\n", b"1 2"));
        assert!(passes(b"1 2  \n", b"1 2\n"));
    }

    #[test]
    fn a_missing_line_fails() {
        assert!(!passes(b"1\n2", b"1\n2\n3"));
    }

    #[test]
    fn line_breaks_are_significant() {
        assert!(!passes(b"1 2\n3", b"1\n2 3"), "a token moved across a line break");
        assert!(!passes(b"1 2", b"1\n2"), "space against line break");
    }

    #[test]
    fn blank_lines_between_lines_collapse() {
        assert!(passes(b"1\n\n\n2", b"1\n2"));
    }
}
