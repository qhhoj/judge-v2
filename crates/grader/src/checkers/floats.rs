use super::tokens::{Token, streams_match_files};
use super::{CheckError, CheckInput, CheckOutcome, Checker};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorMode {
    Default,
    Absolute,
    Relative,
}

/// Same tokens on the same lines; numeric answer tokens may differ within the tolerance.
pub struct Floats {
    pub precision: u32,
    pub mode: ErrorMode,
}

pub const MAX_PRECISION: u32 = 18;

pub fn within(expected: f64, got: f64, epsilon: f64, mode: ErrorMode) -> bool {
    if !got.is_finite() {
        return false;
    }
    let diff = (expected - got).abs();
    let absolute = diff <= epsilon;
    let relative = if expected == 0.0 { got == 0.0 } else { diff <= epsilon * expected.abs() };
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
    fn epsilon(&self) -> f64 {
        10f64.powi(-(self.precision.min(MAX_PRECISION) as i32))
    }

    fn token_matches(&self, expected: &[u8], got: &[u8]) -> bool {
        match parse(expected) {
            Some(e) => parse(got).is_some_and(|g| within(e, g, self.epsilon(), self.mode)),
            None => expected == got,
        }
    }
}

impl Checker for Floats {
    fn check(&self, io: &CheckInput) -> Result<CheckOutcome, CheckError> {
        let same = streams_match_files(io.answer, io.output, false, |want: &Token, got: &Token| {
            want.line == got.line && self.token_matches(&want.text, &got.text)
        })?;
        Ok(if same { CheckOutcome::pass() } else { CheckOutcome::fail("") })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::checkers::testing::run;

    #[test]
    fn tolerance_by_mode() {
        use ErrorMode::{Absolute, Default as Either, Relative};
        // (expected, got, mode, within 1e-6)
        let table = [
            (1.0, 1.0000005, Absolute, true),
            (1.0, 1.00001, Absolute, false),
            (1e9, 1e9 + 100.0, Relative, true),
            (1e9, 1e9 + 100.0, Absolute, false),
            (1e9, 1e9 + 100.0, Either, true),
            (0.0, 1e-7, Relative, false), // relative needs an exact zero
            (0.0, 0.0, Relative, true),
            (0.0, 1e-7, Either, true), // default keeps absolute
            (1.0, f64::NAN, Either, false),
            (1.0, f64::INFINITY, Either, false),
        ];
        for (expected, got, mode, ok) in table {
            assert_eq!(within(expected, got, 1e-6, mode), ok, "{expected} {got} {mode:?}");
        }
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

    #[test]
    fn line_structure_must_match_and_blank_lines_collapse() {
        let c = Floats { precision: 3, mode: ErrorMode::Default };
        assert!(!run(&c, b"1 2\n", b"1\n2\n").passed, "tokens on the wrong lines");
        assert!(!run(&c, b"1\n2", b"1 2").passed, "line break against space");
        assert!(run(&c, b"1\n\n2\n\n", b"1\n2\n").passed);
    }

    #[test]
    fn precision_is_clamped_so_huge_values_cannot_make_everything_pass() {
        let c = Floats { precision: u32::MAX, mode: ErrorMode::Absolute };
        assert!(!run(&c, b"1.5", b"1.0").passed);
        assert!(run(&c, b"1.0", b"1.0").passed);
    }
}
