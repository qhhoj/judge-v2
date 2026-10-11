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
        // DMOJ: a case or batch worth 0 points is a pretest; failing it ends judging, short-circuit or not.
        self.stopped |= self.whole_submission || case.total == 0.0;
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

    fn worth(mut planned: PlannedCase, total: f64) -> PlannedCase {
        planned.total = total;
        planned
    }

    /// (whole short-circuit, cases, passed flags, skipped positions)
    type Row<'a> = (bool, &'a [PlannedCase], &'a [bool], &'a [u32]);

    #[test]
    fn skipped_cases_follow_batches_pretests_and_whole_short_circuit() {
        let pretest_then_tests = [worth(case(1, None), 0.0), case(2, None), case(3, Some(1))];
        let pretest_batch = [worth(case(1, Some(1)), 0.0), worth(case(2, Some(1)), 0.0), case(3, None)];
        let two_batches =
            [case(1, Some(1)), case(2, Some(1)), case(3, Some(1)), case(4, Some(2)), case(5, None)];
        let plain = [case(1, None), case(2, None), case(3, None)];
        let mixed = [case(1, None), case(2, Some(1)), case(3, Some(1)), case(4, None)];
        let next_batch = [case(1, Some(1)), case(2, Some(1)), case(3, Some(2))];
        let table: [Row; 7] = [
            (false, &pretest_then_tests, &[false, true, true], &[2, 3]), // a failed 0-point case ends judging
            (false, &pretest_batch, &[false, true, true], &[2, 3]),
            (false, &pretest_then_tests, &[true, false, true], &[]), // a passed pretest skips nothing
            (false, &two_batches, &[true, false, true, true, true], &[3]), // the rest of its batch only
            (false, &plain, &[false, false, true], &[]),
            (true, &mixed, &[true, false, true, true], &[3, 4]),
            (false, &next_batch, &[false, true, true], &[2]), // a new batch after a failed one runs
        ];
        for (whole, cases, passed, want) in table {
            assert_eq!(skipped(whole, cases, passed), want, "whole={whole} passed={passed:?}");
        }
    }
}
