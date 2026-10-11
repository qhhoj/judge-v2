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

    #[test]
    fn tokens_and_line_breaks_matter_other_whitespace_does_not() {
        // (output, answer, passes)
        let table: [(&[u8], &[u8], bool); 13] = [
            (b"1  2\n3\n", b"1 2\n3", true),
            (b"1   2\t3", b"1 2 3", true),
            (b"1 2\n\n\n", b"1 2", true),
            (b"1 2  \n", b"1 2\n", true),
            (b"1\n\n\n2", b"1\n2", true), // blank lines between lines collapse
            (b"1 2 4", b"1 2 3", false),
            (b"1 2", b"1 2 3", false),     // missing token
            (b"1 2 3 4", b"1 2 3", false), // extra token
            (b"Yes", b"yes", false),       // case matters
            (b"1\n2", b"1\n2\n3", false),  // missing line
            (b"1 2\n3", b"1\n2 3", false), // a token moved across a line break
            (b"1 2", b"1\n2", false),      // space against line break
            (b"x", b"y", false),
        ];
        for (output, answer, passes) in table {
            let outcome = run(&Standard { unicode: false }, output, answer);
            let want_fraction = if passes { 1.0 } else { 0.0 };
            assert_eq!(
                (outcome.passed, outcome.fraction),
                (passes, want_fraction),
                "{output:?} vs {answer:?}"
            );
        }
    }
}
