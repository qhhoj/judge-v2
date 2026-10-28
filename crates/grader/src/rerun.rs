//! TLE re-run (COR-48, docs/grading.md#tle-re-run): a case that ends TLE on its own, under the hard cap, is
//! judged once more while it holds the host alone (`RunGate::exclusive`), and that second result is the case's.
//! CPU time under load is not the program's own: other slots share the cache and memory bandwidth (x1.46 for
//! cache-bound code measured on production with 4 slots busy), so a TLE under load may be an AC alone.
//!
//! The re-run sits inside `grade_cases`'s judge, so short-circuiting sees only the re-run's verdict. Only
//! re-runs that confirm the TLE spend the submission's budget (`GraderDefaults::tle_reruns`); one that flips
//! the verdict is free. A run killed at the hard cap (`spec::hard_cap_s`) or by the wall clock alone is never
//! re-run: it is not a near miss, and re-running it would only stall the host.
//!
//! Every case holds a shared pass of the gate for all of its work (staging, runs, a built-in checker in the
//! daemon, reading the output), so a re-run on another slot waits for whole cases, not only isolate runs. An
//! aborted job does not wait for the host.

use crate::error::GradeError;
use crate::grade::{CaseJudge, CaseVerdict};
use judge_problem::plan::PlannedCase;
use judge_problem::verdict::Status;
use judge_sandbox::RunGate;
use judge_sandbox::spec::{hard_cap_s, whole_ms};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Re-runs that may confirm a TLE per submission unless the config says otherwise (`limits.tle_reruns`).
pub const DEFAULT_TLE_RERUNS: u32 = 2;

/// Wraps a submission's case judge; one per submission, so the budget is per submission.
pub struct TleRerun<'a> {
    pub judge: &'a mut dyn CaseJudge,
    pub gate: Arc<RunGate>,
    pub time_limit_s: f64,
    /// Re-runs that may still confirm a TLE; 0 turns re-runs off.
    pub budget: u32,
    /// The job's abort flag (`Reporter::abort_signal`).
    pub abort: Option<Arc<AtomicBool>>,
}

/// A TLE on CPU time that ended on its own: over the time limit, at most the hard cap, in the whole
/// milliseconds isolate limits and reports (`spec::whole_ms`).
pub fn worth_rerun(verdict: &CaseVerdict, time_limit_s: f64) -> bool {
    let ms = whole_ms(verdict.time_s);
    let ended_on_its_own = ms <= whole_ms(hard_cap_s(time_limit_s));
    verdict.status == Status::Tle && ms > whole_ms(time_limit_s) && ended_on_its_own
}

impl TleRerun<'_> {
    fn aborted(&self) -> Result<(), GradeError> {
        match &self.abort {
            Some(abort) if abort.load(Ordering::SeqCst) => Err(GradeError::Aborted),
            _ => Ok(()),
        }
    }

    /// The case judged again while it holds the host alone; an abort before or while it waits ends grading.
    fn judge_alone(&mut self, case: &PlannedCase) -> Result<CaseVerdict, GradeError> {
        self.aborted()?;
        let _alone = self.gate.exclusive()?;
        self.aborted()?;
        self.judge.judge_case(case)
    }
}

impl CaseJudge for TleRerun<'_> {
    fn judge_case(&mut self, case: &PlannedCase) -> Result<CaseVerdict, GradeError> {
        let first = {
            let _case = self.gate.shared()?;
            self.judge.judge_case(case)?
        };
        if self.budget == 0 || !worth_rerun(&first, self.time_limit_s) {
            return Ok(first);
        }
        let alone = self.judge_alone(case)?;
        if alone.status == Status::Tle {
            self.budget -= 1;
        }
        tracing::info!(
            case = case.position,
            input = %case.input,
            first_time_s = first.time_s,
            rerun_time_s = alone.time_s,
            outcome = ?alone.status,
            "TLE run again alone"
        );
        Ok(alone)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grade::grade_cases;
    use crate::report::{CaseReport, Reporter};
    use judge_problem::init::parse_init;
    use judge_problem::plan::{CasePlan, PlanOptions, plan_cases};
    use std::collections::VecDeque;

    #[derive(Default)]
    struct Recorder(Vec<CaseReport>);

    impl Reporter for Recorder {
        fn compiled(&mut self, _log: &str) {}
        fn started(&mut self, _pretested: bool, _case_count: u32) {}
        fn case(&mut self, report: CaseReport) {
            self.0.push(report);
        }
        fn before_case(&mut self) -> bool {
            true
        }
    }

    /// Answers each run from a script of (status, time) and records the input asked and the pass the case held:
    /// "alone", "shared" or "none".
    struct Scripted {
        script: VecDeque<(Status, f64)>,
        gate: Arc<RunGate>,
        asked: Vec<(String, &'static str)>,
    }

    impl CaseJudge for Scripted {
        fn judge_case(&mut self, case: &PlannedCase) -> Result<CaseVerdict, GradeError> {
            let (status, time_s) = self.script.pop_front().expect("a run the script did not plan");
            let held = match (self.gate.is_exclusive(), self.gate.is_shared()) {
                (true, _) => "alone",
                (false, true) => "shared",
                (false, false) => "none",
            };
            self.asked.push((case.input.clone(), held));
            Ok(CaseVerdict {
                fraction: f64::from(status == Status::Ac),
                ..CaseVerdict::failed(status, time_s, 7, String::new())
            })
        }
    }

    fn plan(yaml: &str) -> CasePlan {
        let options =
            PlanOptions { pretests_only: false, default_output_limit: 1 << 20, default_output_prefix: 0 };
        plan_cases(&parse_init(yaml).unwrap(), &options).unwrap()
    }

    /// Grades `yaml` with a 1 s limit and `budget`; the reports and what the judge was asked.
    fn grade(
        yaml: &str,
        script: &[(Status, f64)],
        budget: u32,
    ) -> (Vec<CaseReport>, Vec<(String, &'static str)>) {
        let gate = Arc::new(RunGate::default());
        let mut scripted =
            Scripted { script: script.iter().copied().collect(), gate: gate.clone(), asked: vec![] };
        let mut judge = TleRerun { judge: &mut scripted, gate, time_limit_s: 1.0, budget, abort: None };
        let mut recorder = Recorder::default();
        grade_cases(&plan(yaml), &mut judge, false, &mut recorder).unwrap();
        assert!(scripted.script.is_empty(), "every scripted run happened");
        (recorder.0, scripted.asked)
    }

    const BATCH: &str = "test_cases:\n- points: 40\n  batched:\n  - {in: a, out: b}\n  - {in: c, out: d}\n";

    #[test]
    fn a_tle_run_again_alone_is_replaced_by_the_rerun_before_the_batch_decides() {
        let script = [(Status::Tle, 1.3), (Status::Ac, 0.9), (Status::Ac, 0.5)];
        let (reports, asked) = grade(BATCH, &script, 2);
        let shape: Vec<_> = reports.iter().map(|r| (r.status, r.time_s, r.points)).collect();
        assert_eq!(
            shape,
            [(Status::Ac, 0.9, 40.0), (Status::Ac, 0.5, 40.0)],
            "the batch passes, c is judged"
        );
        let asked: Vec<_> = asked.iter().map(|(input, held)| (input.as_str(), *held)).collect();
        assert_eq!(asked, [("a", "shared"), ("a", "alone"), ("c", "shared")], "a whole case holds a pass");
    }

    const ONE_CASE: &str = "- {in: x, out: y, points: 1}\n";

    #[test]
    fn only_confirmed_tles_spend_the_budget_and_hopeless_or_wall_clock_tles_are_not_run_again() {
        let cases = "test_cases:\n".to_string() + &ONE_CASE.repeat(6);
        let script = [
            (Status::Tle, 1.2),
            (Status::Tle, 1.1), // confirmed: 1 left
            (Status::Tle, 1.2),
            (Status::Ac, 0.8),    // flipped: free
            (Status::Tle, 2.001), // killed at the hard cap (2 s)
            (Status::Tle, 0.2),   // the wall clock: a sleeping program
            (Status::Tle, 1.2),
            (Status::Tle, 1.3), // confirmed: none left
            (Status::Tle, 1.2), // no budget: stands
        ];
        let (reports, asked) = grade(&cases, &script, 2);
        let times: Vec<_> = reports.iter().map(|r| r.time_s).collect();
        assert_eq!(times, [1.1, 0.8, 2.001, 0.2, 1.3, 1.2]);
        assert_eq!(asked.iter().filter(|(_, held)| *held == "alone").count(), 3);
        let (_, asked) = grade(&format!("test_cases:\n{ONE_CASE}"), &[(Status::Tle, 1.2)], 0);
        assert_eq!(asked.len(), 1, "a budget of 0 turns re-runs off");
    }

    #[test]
    fn the_limits_are_compared_in_whole_milliseconds_as_isolate_counts_them() {
        let tle = |time_s| CaseVerdict::failed(Status::Tle, time_s, 0, String::new());
        // TL 1.18 s: the cap is 2.36 s; isolate kills at 2.361 at the earliest
        assert!(worth_rerun(&tle(2.36), 1.18) && !worth_rerun(&tle(2.361), 1.18));
        // TL 2.01 s: isolate's TO starts at 2.011; 2.010 is under the limit (a wall-clock TLE)
        assert!(worth_rerun(&tle(2.011), 2.01) && !worth_rerun(&tle(2.01), 2.01));
    }

    #[test]
    fn an_aborted_job_does_not_wait_for_the_host() {
        let gate = Arc::new(RunGate::default());
        let script = [(Status::Tle, 1.3)].into_iter().collect();
        let mut scripted = Scripted { script, gate: gate.clone(), asked: vec![] };
        let abort = Some(Arc::new(AtomicBool::new(true)));
        let mut judge = TleRerun { judge: &mut scripted, gate, time_limit_s: 1.0, budget: 2, abort };
        let case = &plan(&format!("test_cases:\n{ONE_CASE}")).cases[0];
        assert_eq!(judge.judge_case(case), Err(GradeError::Aborted));
        assert_eq!(scripted.asked.len(), 1, "no re-run");
    }
}
