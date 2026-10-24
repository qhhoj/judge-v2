//! What one graded case reports, and where grading events go (the proto's `CaseResults`).

use judge_problem::verdict::Status;

/// One case as the bridge stores it. `points` is this case's share of `total`; inside a batch every case
/// carries the batch total, and the batch score (the minimum over its cases) is the bridge's to compute.
#[derive(Debug, Clone, PartialEq)]
pub struct CaseReport {
    pub position: u32,
    pub status: Status,
    pub time_s: f64,
    /// CPU of the problem's own programs for this case (custom checker, interactor); 0 for built-in checkers.
    pub jury_time_s: f64,
    pub memory_kib: u64,
    pub points: f64,
    pub total: f64,
    pub batch: Option<u32>,
    pub feedback: String,
    pub extended_feedback: String,
    pub output_prefix: String,
}

/// Where grading events go. The daemon's implementation feeds the outbox.
pub trait Reporter {
    fn compiled(&mut self, log: &str);
    fn started(&mut self, pretested: bool, case_count: u32);
    fn case(&mut self, report: CaseReport);
    /// Several cases that belong in one message (the daemon then batches them together whatever the clock).
    /// Grading reports one `case` at a time and never calls this; a scripted runner (the conformance replay) does.
    fn cases(&mut self, reports: Vec<CaseReport>) {
        reports.into_iter().for_each(|report| self.case(report));
    }
    /// Called before each case; false stops grading (abort). May block for backpressure.
    fn before_case(&mut self) -> bool;
}
