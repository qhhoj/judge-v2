//! Case results go out every 100 ms or 64 cases (spec §3.5), never more than 256 per message (§3.8).

use judge_grader::report::CaseReport;
use std::time::{Duration, Instant};

pub const MAX_BATCH_CASES: usize = 64;
pub const MAX_BATCH_AGE: Duration = Duration::from_millis(100);
const MAX_PER_MESSAGE: usize = 256;

pub struct CaseBatcher {
    pending: Vec<CaseReport>,
    oldest: Option<Instant>,
    max_cases: usize,
    max_age: Duration,
}

impl CaseBatcher {
    pub fn new(max_cases: usize, max_age: Duration) -> Self {
        CaseBatcher { pending: Vec::new(), oldest: None, max_cases: max_cases.min(MAX_PER_MESSAGE), max_age }
    }

    pub fn push(&mut self, report: CaseReport, now: Instant) -> Option<Vec<CaseReport>> {
        self.oldest.get_or_insert(now);
        self.pending.push(report);
        let old = self.oldest.is_some_and(|t| now.duration_since(t) >= self.max_age);
        (self.pending.len() >= self.max_cases || old).then(|| self.take())
    }

    /// When the oldest pending case must go out; None when nothing is pending.
    pub fn due(&self) -> Option<Instant> {
        self.oldest.map(|t| t + self.max_age)
    }

    pub fn flush(&mut self) -> Option<Vec<CaseReport>> {
        (!self.pending.is_empty()).then(|| self.take())
    }

    fn take(&mut self) -> Vec<CaseReport> {
        self.oldest = None;
        std::mem::take(&mut self.pending)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use judge_problem::verdict::Status;

    fn report(position: u32) -> CaseReport {
        CaseReport {
            position,
            status: Status::Ac,
            time_s: 0.0,
            jury_time_s: 0.0,
            memory_kib: 0,
            points: 1.0,
            total: 1.0,
            batch: None,
            feedback: String::new(),
            extended_feedback: String::new(),
            output_prefix: String::new(),
        }
    }

    #[test]
    fn flushes_at_64_cases() {
        let mut b = CaseBatcher::new(MAX_BATCH_CASES, MAX_BATCH_AGE);
        let now = Instant::now();
        for i in 1..64 {
            assert!(b.push(report(i), now).is_none());
        }
        assert_eq!(b.push(report(64), now).unwrap().len(), 64);
        assert!(b.flush().is_none());
    }

    #[test]
    fn flushes_when_the_oldest_case_is_100ms_old() {
        let mut b = CaseBatcher::new(MAX_BATCH_CASES, MAX_BATCH_AGE);
        let start = Instant::now();
        assert!(b.push(report(1), start).is_none());
        assert_eq!(b.due(), Some(start + MAX_BATCH_AGE));
        assert_eq!(b.push(report(2), start + Duration::from_millis(100)).unwrap().len(), 2);
        assert_eq!(b.due(), None);
    }

    #[test]
    fn never_more_than_256_per_message() {
        let mut b = CaseBatcher::new(1000, MAX_BATCH_AGE);
        let now = Instant::now();
        let mut sizes = vec![];
        for i in 0..600 {
            if let Some(batch) = b.push(report(i), now) {
                sizes.push(batch.len());
            }
        }
        assert!(sizes.iter().all(|s| *s <= 256));
    }
}
