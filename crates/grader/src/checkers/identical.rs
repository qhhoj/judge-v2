use super::standard::same_tokens;
use super::{CheckError, CheckInput, CheckOutcome, Checker, read_file};

pub const PRESENTATION_ERROR: &str = "Presentation Error, check your whitespace";

pub struct Identical {
    pub pe_allowed: bool,
}

impl Checker for Identical {
    fn check(&self, io: &CheckInput) -> Result<CheckOutcome, CheckError> {
        let (output, answer) = (read_file(io.output)?, read_file(io.answer)?);
        if output == answer {
            return Ok(CheckOutcome::pass());
        }
        let whitespace_only = self.pe_allowed && same_tokens(&output, &answer, false);
        Ok(CheckOutcome::fail(if whitespace_only { PRESENTATION_ERROR } else { "" }))
    }
}

#[cfg(test)]
mod tests {
    use super::Identical;
    use crate::checkers::testing::run;

    #[test]
    fn exact_bytes_pass_whitespace_only_is_presentation_error() {
        let c = Identical { pe_allowed: true };
        assert!(run(&c, b"1 2\n", b"1 2\n").passed);
        let pe = run(&c, b"1  2\n", b"1 2\n");
        assert!(!pe.passed);
        assert_eq!(pe.feedback, "Presentation Error, check your whitespace");
        let strict = run(&Identical { pe_allowed: false }, b"1  2\n", b"1 2\n");
        assert_eq!((strict.passed, strict.feedback.as_str()), (false, ""));
        assert_eq!(run(&c, b"1 3\n", b"1 2\n").feedback, "");
    }
}
