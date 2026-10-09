//! Which cases are skipped (SC) after a failure.

use crate::plan::PlannedCase;

#[derive(Debug, Default)]
pub struct ShortCircuit {
    whole_submission: bool,
    failed_batch: Option<u32>,
    stopped: bool,
}

impl ShortCircuit {
    pub fn new(whole_submission: bool) -> Self {
        ShortCircuit { whole_submission, ..Default::default() }
    }

    pub fn should_skip(&self, case: &PlannedCase) -> bool {
        self.stopped || (case.batch.is_some() && case.batch == self.failed_batch)
    }

    pub fn record(&mut self, case: &PlannedCase, passed: bool) {
        if passed {
            return;
        }
        if case.batch.is_some() {
            self.failed_batch = case.batch;
        }
        self.stopped |= self.whole_submission;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::init::CheckerSpec;
    use crate::plan::PlannedCase;

    fn case(position: u32, batch: Option<u32>) -> PlannedCase {
        PlannedCase {
            position,
            input: String::new(),
            output: String::new(),
            total: 1.0,
            batch,
            checker: CheckerSpec::Name("standard".into()),
            output_limit: 1,
            output_prefix: 0,
        }
    }

    /// Runs the cases in order with the given pass flags; returns which positions were skipped.
    fn skipped(whole: bool, cases: &[PlannedCase], passed: &[bool]) -> Vec<u32> {
        let mut sc = ShortCircuit::new(whole);
        let mut skipped = Vec::new();
        for (case, ok) in cases.iter().zip(passed) {
            if sc.should_skip(case) {
                skipped.push(case.position);
                continue;
            }
            sc.record(case, *ok);
        }
        skipped
    }

    #[test]
    fn a_failure_skips_the_rest_of_its_batch_only() {
        let cases = [case(1, Some(1)), case(2, Some(1)), case(3, Some(1)), case(4, Some(2)), case(5, None)];
        assert_eq!(skipped(false, &cases, &[true, false, true, true, true]), [3]);
    }

    #[test]
    fn plain_failures_skip_nothing_without_whole_short_circuit() {
        let cases = [case(1, None), case(2, None), case(3, None)];
        assert!(skipped(false, &cases, &[false, false, true]).is_empty());
    }

    #[test]
    fn whole_short_circuit_skips_everything_after_the_first_failure() {
        let cases = [case(1, None), case(2, Some(1)), case(3, Some(1)), case(4, None)];
        assert_eq!(skipped(true, &cases, &[true, false, true, true]), [3, 4]);
    }

    #[test]
    fn a_new_batch_after_a_failed_batch_runs() {
        let cases = [case(1, Some(1)), case(2, Some(1)), case(3, Some(2))];
        assert_eq!(skipped(false, &cases, &[false, true, true]), [2]);
    }
}
