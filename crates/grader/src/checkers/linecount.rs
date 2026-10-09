use super::{CheckError, CheckInput, CheckOutcome, Checker, read_file};

pub struct LineCount;

fn lines(bytes: &[u8]) -> Vec<&[u8]> {
    let mut lines: Vec<&[u8]> = bytes.split(|b| *b == b'\n').map(|l| l.trim_ascii()).collect();
    while lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    lines
}

impl Checker for LineCount {
    fn check(&self, io: &CheckInput) -> Result<CheckOutcome, CheckError> {
        let (output, answer) = (read_file(io.output)?, read_file(io.answer)?);
        let (got, want) = (lines(&output), lines(&answer));
        if want.is_empty() {
            return Ok(if got.is_empty() { CheckOutcome::pass() } else { CheckOutcome::fail("") });
        }
        let matched = want.iter().zip(&got).filter(|(w, g)| w == g).count();
        let passed = matched == want.len() && got.len() == want.len();
        let fraction = matched as f64 / want.len() as f64;
        Ok(CheckOutcome { passed, fraction, ..CheckOutcome::fail("") })
    }
}

#[cfg(test)]
mod tests {
    use super::LineCount;
    use crate::checkers::testing::run;

    #[test]
    fn partial_points_per_matching_line() {
        let r = run(&LineCount, b"a\n b \nx\n", b"a\nb\nc\nd\n");
        assert!(!r.passed);
        assert!((r.fraction - 0.5).abs() < 1e-9);
    }

    #[test]
    fn all_lines_pass_and_trailing_blank_lines_are_ignored() {
        let r = run(&LineCount, b"a\nb\n\n\n", b"a\nb\n");
        assert!(r.passed);
        assert_eq!(r.fraction, 1.0);
    }

    #[test]
    fn extra_lines_fail_but_keep_points() {
        let r = run(&LineCount, b"a\nb\nzzz\n", b"a\nb\n");
        assert!(!r.passed);
        assert_eq!(r.fraction, 1.0);
    }
}
