use super::tokens::tokens;
use super::{CheckError, CheckInput, CheckOutcome, Checker, read_file};

pub struct Standard {
    pub unicode: bool,
}

pub fn same_tokens(output: &[u8], answer: &[u8], unicode: bool) -> bool {
    tokens(output, unicode) == tokens(answer, unicode)
}

impl Checker for Standard {
    fn check(&self, io: &CheckInput) -> Result<CheckOutcome, CheckError> {
        let (output, answer) = (read_file(io.output)?, read_file(io.answer)?);
        Ok(if same_tokens(&output, &answer, self.unicode) {
            CheckOutcome::pass()
        } else {
            CheckOutcome::fail("")
        })
    }
}

#[cfg(test)]
mod tests {
    use super::Standard;
    use crate::checkers::testing::run;

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
}
