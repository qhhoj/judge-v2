use super::text::split_lines;
use super::{CheckError, CheckInput, CheckOutcome, Checker, read_file};

/// Partial points per line: `point_distribution` weights the answer's non-empty lines.
pub struct LineMatches {
    pub point_distribution: Vec<u64>,
    pub filler_lines_required: bool,
}

fn non_empty_lines(data: &[u8]) -> Vec<&[u8]> {
    split_lines(data).into_iter().filter(|l| !l.is_empty()).collect()
}

impl Checker for LineMatches {
    fn check(&self, io: &CheckInput) -> Result<CheckOutcome, CheckError> {
        let (got, want) = (read_file(io.output)?, read_file(io.answer)?);
        let (got, want) = (non_empty_lines(&got), non_empty_lines(&want));
        let total: u64 = self.point_distribution.iter().sum();
        if want.len() != self.point_distribution.len() {
            return Err(CheckError(format!(
                "answer has {} lines but point_distribution has {} entries",
                want.len(),
                self.point_distribution.len()
            )));
        }
        if total == 0 {
            return Err(CheckError("point_distribution sums to 0".into()));
        }
        if self.filler_lines_required && got.len() != want.len() {
            return Ok(CheckOutcome::fail("wrong number of lines"));
        }
        let earned: u64 = got
            .iter()
            .zip(&want)
            .zip(&self.point_distribution)
            .filter(|((g, w), _)| g == w)
            .map(|(_, weight)| weight)
            .sum();
        let share = earned as f64 / total as f64;
        Ok(CheckOutcome {
            passed: share > 0.0,
            fraction: share,
            feedback: String::new(),
            extended: String::new(),
            jury_time_s: 0.0,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::LineMatches;
    use crate::checkers::testing::{files, run};
    use crate::checkers::{CheckInput, Checker};

    fn lm(weights: &[u64], filler: bool) -> LineMatches {
        LineMatches { point_distribution: weights.to_vec(), filler_lines_required: filler }
    }

    #[test]
    fn share_is_the_matching_weight_over_the_total() {
        let r = run(&lm(&[1, 3], true), b"a\nx\n", b"a\nb\n");
        assert!(r.passed);
        assert!((r.fraction - 0.25).abs() < 1e-9);
        let none = run(&lm(&[1, 3], true), b"x\ny\n", b"a\nb\n");
        assert!(!none.passed && none.fraction == 0.0);
        let all = run(&lm(&[1, 3], true), b"a\n\nb\n", b"a\nb\n");
        assert!(all.passed && all.fraction == 1.0, "empty lines are dropped");
    }

    #[test]
    fn filler_lines_decide_what_a_line_count_mismatch_means() {
        let strict = run(&lm(&[1, 1], true), b"a\n", b"a\nb\n");
        assert!(!strict.passed && strict.fraction == 0.0);
        let lenient = run(&lm(&[1, 1], false), b"a\n", b"a\nb\n");
        assert!(lenient.passed && (lenient.fraction - 0.5).abs() < 1e-9);
        let extra = run(&lm(&[1, 1], false), b"a\nb\nc\n", b"a\nb\n");
        assert!(extra.passed && extra.fraction == 1.0, "extra lines are ignored without the requirement");
    }

    #[test]
    fn a_distribution_that_does_not_fit_the_answer_is_an_error() {
        let check = |c: LineMatches, answer: &[u8]| {
            let f = files(b"a\n", answer);
            c.check(&CheckInput { input: &f.input, output: &f.output, answer: &f.answer, points: 1.0 })
        };
        assert!(check(lm(&[1], true), b"a\nb\n").is_err());
        assert!(check(lm(&[0, 0], true), b"a\nb\n").is_err());
        assert!(check(lm(&[1], true), b"a\n").is_ok());
    }
}
