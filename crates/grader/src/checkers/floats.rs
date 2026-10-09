use super::tokens::tokens;
use super::{CheckError, CheckInput, CheckOutcome, Checker, read_file};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorMode {
    Default,
    Absolute,
    Relative,
}

pub struct Floats {
    pub precision: u32,
    pub mode: ErrorMode,
}

pub fn within(expected: f64, got: f64, epsilon: f64, mode: ErrorMode) -> bool {
    if !got.is_finite() {
        return false;
    }
    let diff = (expected - got).abs();
    let absolute = diff <= epsilon;
    let relative = if expected == 0.0 { absolute } else { diff <= epsilon * expected.abs() };
    match mode {
        ErrorMode::Absolute => absolute,
        ErrorMode::Relative => relative,
        ErrorMode::Default => absolute || relative,
    }
}

fn parse(token: &[u8]) -> Option<f64> {
    std::str::from_utf8(token).ok()?.parse().ok()
}

impl Floats {
    fn token_matches(&self, expected: &[u8], got: &[u8]) -> bool {
        let epsilon = 10f64.powi(-(self.precision as i32));
        match parse(expected) {
            Some(e) => parse(got).is_some_and(|g| within(e, g, epsilon, self.mode)),
            None => expected == got,
        }
    }
}

impl Checker for Floats {
    fn check(&self, io: &CheckInput) -> Result<CheckOutcome, CheckError> {
        let (output, answer) = (read_file(io.output)?, read_file(io.answer)?);
        let (got, want) = (tokens(&output, false), tokens(&answer, false));
        let ok = got.len() == want.len() && want.iter().zip(&got).all(|(w, g)| self.token_matches(w, g));
        Ok(if ok { CheckOutcome::pass() } else { CheckOutcome::fail("") })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::checkers::testing::run;

    #[test]
    fn tolerance_by_mode() {
        assert!(within(1.0, 1.0000005, 1e-6, ErrorMode::Absolute));
        assert!(!within(1.0, 1.00001, 1e-6, ErrorMode::Absolute));
        assert!(within(1e9, 1e9 + 100.0, 1e-6, ErrorMode::Relative));
        assert!(!within(1e9, 1e9 + 100.0, 1e-6, ErrorMode::Absolute));
        assert!(within(1e9, 1e9 + 100.0, 1e-6, ErrorMode::Default));
        assert!(within(0.0, 1e-7, 1e-6, ErrorMode::Relative), "relative falls back to absolute at zero");
    }

    #[test]
    fn non_finite_output_fails() {
        assert!(!within(1.0, f64::NAN, 1e-6, ErrorMode::Default));
        assert!(!within(1.0, f64::INFINITY, 1e-6, ErrorMode::Default));
    }

    #[test]
    fn mixed_tokens_and_counts() {
        let c = Floats { precision: 3, mode: ErrorMode::Default };
        assert!(run(&c, b"YES 3.1416", b"YES 3.14159").passed);
        assert!(!run(&c, b"NO 3.1416", b"YES 3.14159").passed);
        assert!(!run(&c, b"YES abc", b"YES 3.14159").passed);
        assert!(!run(&c, b"YES 3.1416 1", b"YES 3.14159").passed);
        assert!(!run(&c, b"YES 3.2", b"YES 3.14159").passed);
    }
}
