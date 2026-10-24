//! Walks the case plan: short-circuit, points, clipping (docs/grading.md#cases-batches-and-points, limits in docs/protocol.md#limits).
//!
//! DMOJ semantics: a case that is not AC skips the rest of its batch (SC); with the submission's
//! `short_circuit` it skips every later case. A failed case worth 0 points (a pretest) skips every later case. The plan already holds the run's cases.

use crate::checkers::custom::{MAX_EXTENDED, MAX_FEEDBACK, clip_text};
use crate::error::GradeError;
use crate::report::{CaseReport, Reporter};
use judge_problem::plan::{CasePlan, PlannedCase};
use judge_problem::short_circuit::ShortCircuit;
use judge_problem::verdict::Status;

#[derive(Debug, Clone, PartialEq)]
pub struct CaseVerdict {
    pub status: Status,
    /// Share of the case total earned, 0..=1. A failed case keeps the checker's partial share (linecount).
    pub fraction: f64,
    pub time_s: f64,
    /// CPU of the checker and interactor runs (see `CaseReport::jury_time_s`).
    pub jury_time_s: f64,
    pub memory_kib: u64,
    pub feedback: String,
    pub extended: String,
    pub output_prefix: String,
}

impl CaseVerdict {
    pub fn failed(status: Status, time_s: f64, memory_kib: u64, feedback: String) -> CaseVerdict {
        CaseVerdict {
            status,
            fraction: 0.0,
            time_s,
            jury_time_s: 0.0,
            memory_kib,
            feedback,
            extended: String::new(),
            output_prefix: String::new(),
        }
    }
}

pub trait CaseJudge {
    fn judge_case(&mut self, case: &PlannedCase) -> Result<CaseVerdict, GradeError>;
}

fn skipped(case: &PlannedCase) -> CaseReport {
    CaseReport {
        position: case.position,
        status: Status::Sc,
        time_s: 0.0,
        jury_time_s: 0.0,
        memory_kib: 0,
        points: 0.0,
        total: case.total,
        batch: case.batch,
        feedback: String::new(),
        extended_feedback: String::new(),
        output_prefix: String::new(),
    }
}

fn graded(case: &PlannedCase, verdict: CaseVerdict) -> CaseReport {
    CaseReport {
        position: case.position,
        status: verdict.status,
        time_s: verdict.time_s,
        jury_time_s: verdict.jury_time_s,
        memory_kib: verdict.memory_kib,
        points: verdict.fraction * case.total,
        total: case.total,
        batch: case.batch,
        feedback: clip_text(&verdict.feedback, MAX_FEEDBACK),
        extended_feedback: clip_text(&verdict.extended, MAX_EXTENDED),
        output_prefix: clip_text(&verdict.output_prefix, MAX_EXTENDED),
    }
}

/// Grades every case of the plan in order. `Err(Aborted)` when the reporter stops it before a case.
pub fn grade_cases(
    plan: &CasePlan,
    judge: &mut dyn CaseJudge,
    short_circuit: bool,
    reporter: &mut dyn Reporter,
) -> Result<(), GradeError> {
    reporter.started(plan.pretested, plan.cases.len() as u32);
    let mut sc = ShortCircuit::new(short_circuit);
    for case in &plan.cases {
        if !reporter.before_case() {
            return Err(GradeError::Aborted);
        }
        if sc.should_skip(case) {
            reporter.case(skipped(case));
            continue;
        }
        let verdict = judge.judge_case(case)?;
        sc.record(case, verdict.status == Status::Ac);
        reporter.case(graded(case, verdict));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::{CaseReport, Reporter};
    use judge_problem::init::parse_init;
    use judge_problem::plan::{PlanOptions, plan_cases};

    #[derive(Default)]
    struct Recorder {
        started: Option<(bool, u32)>,
        cases: Vec<CaseReport>,
        abort_after: Option<usize>,
    }

    impl Reporter for Recorder {
        fn compiled(&mut self, _log: &str) {}
        fn started(&mut self, pretested: bool, case_count: u32) {
            self.started = Some((pretested, case_count));
        }
        fn case(&mut self, report: CaseReport) {
            self.cases.push(report);
        }
        fn before_case(&mut self) -> bool {
            self.abort_after.is_none_or(|n| self.cases.len() < n)
        }
    }

    /// Answers each case from a script of (status, fraction) and records which inputs it was asked.
    struct Scripted {
        script: Vec<(Status, f64)>,
        asked: Vec<String>,
    }

    fn scripted(script: Vec<(Status, f64)>) -> Scripted {
        Scripted { script, asked: Vec::new() }
    }

    impl CaseJudge for Scripted {
        fn judge_case(&mut self, case: &PlannedCase) -> Result<CaseVerdict, GradeError> {
            let (status, fraction) = self.script[self.asked.len()];
            self.asked.push(case.input.clone());
            Ok(CaseVerdict {
                status,
                fraction,
                time_s: 0.1,
                jury_time_s: 0.25,
                memory_kib: 10,
                feedback: "f".repeat(80),
                extended: "e".repeat(600),
                output_prefix: "p".repeat(600),
            })
        }
    }

    fn plan_with(yaml: &str, pretests_only: bool) -> CasePlan {
        let options = PlanOptions { pretests_only, default_output_limit: 1 << 20, default_output_prefix: 0 };
        plan_cases(&parse_init(yaml).unwrap(), &options).unwrap()
    }

    fn plan(yaml: &str) -> CasePlan {
        plan_with(yaml, false)
    }

    const BATCHED: &str = "test_cases:\n- points: 40\n  batched:\n  - {in: a, out: b}\n  - {in: c, out: d}\n  - {in: e, out: f}\n- {in: g, out: h, points: 60}\n";

    #[test]
    fn batch_failure_skips_the_rest_of_the_batch_and_points_scale() {
        let mut rec = Recorder::default();
        let mut judge = scripted(vec![(Status::Ac, 1.0), (Status::Wa, 0.0), (Status::Ac, 0.5)]);
        grade_cases(&plan(BATCHED), &mut judge, false, &mut rec).unwrap();
        let shape: Vec<_> = rec.cases.iter().map(|c| (c.status, c.points, c.total, c.batch)).collect();
        assert_eq!(
            shape,
            [
                (Status::Ac, 40.0, 40.0, Some(1)),
                (Status::Wa, 0.0, 40.0, Some(1)),
                (Status::Sc, 0.0, 40.0, Some(1)),
                (Status::Ac, 30.0, 60.0, None),
            ]
        );
        assert_eq!(judge.asked, ["a", "c", "g"], "the skipped case never ran");
        assert_eq!(rec.started, Some((false, 4)));
        let first = &rec.cases[0];
        assert_eq!(first.position, 1);
        assert_eq!(first.jury_time_s, 0.25, "the jury time reaches the report");
        assert_eq!(
            (first.feedback.len(), first.extended_feedback.len(), first.output_prefix.len()),
            (50, 512, 512),
            "feedback, extended feedback and prefix are clipped"
        );
    }

    #[test]
    fn whole_short_circuit_marks_everything_after_the_failure() {
        let mut rec = Recorder::default();
        let mut judge = scripted(vec![(Status::Tle, 0.0)]);
        grade_cases(&plan(BATCHED), &mut judge, true, &mut rec).unwrap();
        let statuses: Vec<_> = rec.cases.iter().map(|c| c.status).collect();
        assert_eq!(statuses, [Status::Tle, Status::Sc, Status::Sc, Status::Sc]);
        assert_eq!((rec.cases[3].time_s, rec.cases[3].memory_kib, rec.cases[3].total), (0.0, 0, 60.0));
        assert_eq!(rec.cases[3].jury_time_s, 0.0, "a skipped case ran nothing");
    }

    #[test]
    fn abort_stops_before_the_next_case() {
        let mut rec = Recorder { abort_after: Some(1), ..Default::default() };
        let mut judge = scripted(vec![(Status::Ac, 1.0); 4]);
        assert_eq!(grade_cases(&plan(BATCHED), &mut judge, false, &mut rec), Err(GradeError::Aborted));
        assert_eq!(rec.cases.len(), 1);
        assert_eq!(judge.asked.len(), 1);
    }

    #[test]
    fn failed_case_keeps_partial_points_from_the_checker() {
        let mut rec = Recorder::default();
        let mut judge = scripted(vec![(Status::Wa, 0.5)]);
        grade_cases(&plan("test_cases:\n- {in: a, out: b, points: 10}\n"), &mut judge, false, &mut rec)
            .unwrap();
        assert_eq!((rec.cases[0].status, rec.cases[0].points), (Status::Wa, 5.0));
    }

    #[test]
    fn partial_ac_in_a_batch_does_not_skip_the_rest() {
        let mut rec = Recorder::default();
        let mut judge =
            scripted(vec![(Status::Ac, 0.5), (Status::Ac, 1.0), (Status::Ac, 1.0), (Status::Ac, 1.0)]);
        grade_cases(&plan(BATCHED), &mut judge, false, &mut rec).unwrap();
        let points: Vec<_> = rec.cases.iter().map(|c| c.points).collect();
        assert_eq!(points, [20.0, 40.0, 40.0, 60.0]);
    }

    #[test]
    fn a_judge_error_stops_grading_with_that_error() {
        struct Failing;
        impl CaseJudge for Failing {
            fn judge_case(&mut self, _case: &PlannedCase) -> Result<CaseVerdict, GradeError> {
                Err(GradeError::Checker("crash".into()))
            }
        }
        let mut rec = Recorder::default();
        let result = grade_cases(&plan(BATCHED), &mut Failing, false, &mut rec);
        assert_eq!(result, Err(GradeError::Checker("crash".into())));
        assert!(rec.cases.is_empty());
    }
}
