//! TLE re-run (COR-48, docs/grading.md#tle-re-run): a case that ends TLE on its own, under the hard cap, is
//! judged once more while it holds the host alone (`RunGate::exclusive`), and that second result is the case's.
//! CPU time under load is not the program's own: other slots share the cache and memory bandwidth (x1.46 for
//! cache-bound code measured on production with 4 slots busy), so a TLE under load may be an AC alone.
//!
//! A TLE the wall clock ended, its CPU time within the limit (#95), is judged again alone too: a program blocked
//! on a busy shared disk accrues wall time but no CPU. One that sleeps or blocks on its own times out again, so
//! it stays TLE after one extra run. Such a TLE is reported with the feedback `wall time` (`WALL_TIME`). A
//! signal from inside the box can be forged, so a count caps these: at most `MAX_WALL_RERUNS` per submission,
//! flips counted, and none after one confirmed the TLE (the program blocks by itself).
//!
//! The re-run sits inside `grade_cases`'s judge, so short-circuiting sees only the re-run's verdict. Only
//! re-runs that confirm the TLE spend the submission's budget (`GraderDefaults::tle_reruns`); one that flips
//! the verdict does not. Every re-run, a flip too, counts against `MAX_RERUNS` per submission, so the time one
//! submission holds the host alone is bounded; past it a TLE stands. A case is re-run at most once. A run killed
//! at the hard cap (`spec::hard_cap_s`) is never re-run: it is not a near miss, and re-running it would only stall
//! the host.
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

/// Wall-clock TLE re-runs per submission, flips counted; a confirmed one ends them (`TleRerun::wall_reruns`).
pub const MAX_WALL_RERUNS: u32 = 2;

/// Re-runs per submission of any kind, flips and wall-clock ones counted (`TleRerun::reruns`).
pub const MAX_RERUNS: u32 = 20;

/// Wraps a submission's case judge; one per submission, so the budget is per submission.
pub struct TleRerun<'a> {
    pub judge: &'a mut dyn CaseJudge,
    pub gate: Arc<RunGate>,
    pub time_limit_s: f64,
    /// Re-runs that may still confirm a TLE; 0 turns re-runs off.
    pub budget: u32,
    /// Wall-clock TLE re-runs left, `MAX_WALL_RERUNS` at the start: a flip spends one, a confirmation all.
    pub wall_reruns: u32,
    /// Re-runs of any kind left, `MAX_RERUNS` at the start: every re-run spends one.
    pub reruns: u32,
    /// The job's abort flag (`Reporter::abort_signal`).
    pub abort: Option<Arc<AtomicBool>>,
}

/// Feedback of a TLE the wall clock ended (`wall_clock_tle`), so such cases can be told apart and counted.
pub const WALL_TIME: &str = "wall time";

/// A TLE on CPU time that ended on its own: over the time limit, at most the hard cap, in the whole
/// milliseconds isolate limits and reports (`spec::whole_ms`).
pub fn worth_rerun(verdict: &CaseVerdict, time_limit_s: f64) -> bool {
    let ms = whole_ms(verdict.time_s);
    let ended_on_its_own = ms <= whole_ms(hard_cap_s(time_limit_s));
    verdict.status == Status::Tle && ms > whole_ms(time_limit_s) && ended_on_its_own
}

/// A TLE with its CPU time within the limit: the wall clock ended it (`classify` gives TLE on CPU only past
/// the limit in whole ms).
pub fn wall_clock_tle(verdict: &CaseVerdict, time_limit_s: f64) -> bool {
    verdict.status == Status::Tle && whole_ms(verdict.time_s) <= whole_ms(time_limit_s)
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

    /// A CPU TLE worth one more run, or a wall-clock TLE while wall re-runs are left; both need budget.
    fn wants_rerun(&self, first: &CaseVerdict, wall_clock: bool) -> bool {
        let wanted = if wall_clock { self.wall_reruns > 0 } else { worth_rerun(first, self.time_limit_s) };
        self.budget > 0 && wanted
    }

    /// The first run's verdict, or the re-run's when `wants_rerun` and re-runs are left. A TLE that stands
    /// only because `MAX_RERUNS` is spent is logged, so it can be found and rejudged.
    fn final_verdict(&mut self, case: &PlannedCase, first: CaseVerdict) -> Result<CaseVerdict, GradeError> {
        let wall_clock = wall_clock_tle(&first, self.time_limit_s);
        if !self.wants_rerun(&first, wall_clock) {
            return Ok(first);
        }
        if self.reruns == 0 {
            tracing::info!(case = case.position, input = %case.input, first_time_s = first.time_s, "re-run cap reached");
            return Ok(first);
        }
        self.reruns -= 1;
        let alone = self.judge_alone(case)?;
        let confirmed = alone.status == Status::Tle;
        if confirmed {
            self.budget -= 1;
        }
        if wall_clock {
            self.wall_reruns = if confirmed { 0 } else { self.wall_reruns - 1 };
        }
        tracing::info!(
            case = case.position,
            input = %case.input,
            first_time_s = first.time_s,
            first_wall_clock = wall_clock,
            rerun_time_s = alone.time_s,
            outcome = ?alone.status,
            "TLE run again alone"
        );
        Ok(alone)
    }
}

impl CaseJudge for TleRerun<'_> {
    fn judge_case(&mut self, case: &PlannedCase) -> Result<CaseVerdict, GradeError> {
        let first = {
            let _case = self.gate.shared()?;
            self.judge.judge_case(case)?
        };
        let verdict = self.final_verdict(case, first)?;
        if wall_clock_tle(&verdict, self.time_limit_s) {
            return Ok(CaseVerdict { feedback: WALL_TIME.into(), ..verdict });
        }
        Ok(verdict)
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
        let (wall_reruns, reruns) = (MAX_WALL_RERUNS, MAX_RERUNS);
        let mut judge = TleRerun {
            judge: &mut scripted,
            gate,
            time_limit_s: 1.0,
            budget,
            wall_reruns,
            reruns,
            abort: None,
        };
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

    /// (status, time, feedback) of each report, and how many runs held the host alone.
    fn outcome(yaml: &str, script: &[(Status, f64)], budget: u32) -> (Vec<(Status, f64, String)>, usize) {
        let (reports, asked) = grade(yaml, script, budget);
        let shape = reports.iter().map(|r| (r.status, r.time_s, r.feedback.clone())).collect();
        (shape, asked.iter().filter(|(_, held)| *held == "alone").count())
    }

    fn cases(n: usize) -> String {
        "test_cases:\n".to_string() + &ONE_CASE.repeat(n)
    }

    #[test]
    fn only_confirmed_tles_spend_the_budget_and_tles_killed_at_the_hard_cap_are_not_run_again() {
        let script = [
            (Status::Tle, 1.2),
            (Status::Tle, 1.1), // confirmed: 1 left
            (Status::Tle, 1.2),
            (Status::Ac, 0.8),    // flipped: the budget is not spent
            (Status::Tle, 2.001), // killed at the hard cap (2 s)
            (Status::Tle, 1.2),
            (Status::Tle, 1.3), // confirmed: none left
            (Status::Tle, 1.2), // no budget: stands
        ];
        let (reports, alone) = outcome(&cases(5), &script, 2);
        let times: Vec<_> = reports.iter().map(|(_, time_s, _)| *time_s).collect();
        assert_eq!(times, [1.1, 0.8, 2.001, 1.3, 1.2]);
        assert!(reports.iter().all(|(_, _, feedback)| feedback.is_empty()), "CPU TLEs: {reports:?}");
        assert_eq!(alone, 3);
        let (_, alone) = outcome(&cases(1), &[(Status::Tle, 1.2)], 0);
        assert_eq!(alone, 0, "a budget of 0 turns re-runs off");
    }

    #[test]
    fn a_wall_clock_tle_is_run_again_alone_and_a_flip_keeps_the_budget_up_to_two_wall_reruns() {
        let script = [
            (Status::Tle, 0.2),
            (Status::Ac, 0.4), // flipped: the budget of 1 is left
            (Status::Tle, 0.1),
            (Status::Ac, 0.3), // flipped: no wall re-run left
            (Status::Tle, 0.2),
        ];
        let (reports, alone) = outcome(&cases(3), &script, 1);
        let ac = |time_s| (Status::Ac, time_s, String::new());
        let wall = (Status::Tle, 0.2, WALL_TIME.to_string());
        assert_eq!(reports, [ac(0.4), ac(0.3), wall], "a stall on the host is not the program's");
        assert_eq!(alone, 2);
    }

    #[test]
    fn flips_count_too_so_a_submission_holds_the_host_alone_at_most_max_reruns_times() {
        let n = MAX_RERUNS as usize;
        let mut script = [(Status::Tle, 1.2), (Status::Ac, 0.8)].repeat(n);
        script.push((Status::Tle, 1.2)); // past the cap: stands
        let (reports, alone) = outcome(&cases(n + 1), &script, 2);
        assert_eq!(alone, n);
        assert_eq!(reports.last().map(|(status, time_s, _)| (*status, *time_s)), Some((Status::Tle, 1.2)));
    }

    #[test]
    fn once_a_wall_clock_tle_is_confirmed_no_other_is_run_again() {
        let sleeper = [(Status::Tle, 0.0); 6]; // one re-run, then 4 more cases
        let (reports, alone) = outcome(&cases(5), &sleeper, 2);
        assert!(reports.iter().all(|(status, _, feedback)| *status == Status::Tle && feedback == WALL_TIME));
        assert_eq!(alone, 1, "the budget of 2 is not spent on a program that blocks by itself");
    }

    #[test]
    fn a_wall_clock_tle_stays_tle_after_one_run_alone_and_says_wall_time() {
        let script = [
            (Status::Tle, 1.2),
            (Status::Tle, 0.5), // a CPU TLE whose re-run the wall clock ended: 1 left, no third run
            (Status::Tle, 0.0),
            (Status::Tle, 0.01), // a program that sleeps: none left
            (Status::Tle, 0.0),  // no budget: stands
        ];
        let (reports, alone) = outcome(&cases(3), &script, 2);
        let wall = |time_s| (Status::Tle, time_s, WALL_TIME.to_string());
        assert_eq!(reports, [wall(0.5), wall(0.01), wall(0.0)]);
        assert_eq!(alone, 2);
    }

    #[test]
    fn the_limits_are_compared_in_whole_milliseconds_as_isolate_counts_them() {
        let tle = |time_s| CaseVerdict::failed(Status::Tle, time_s, 0, String::new());
        // TL 1.18 s: the cap is 2.36 s; isolate kills at 2.361 at the earliest
        assert!(worth_rerun(&tle(2.36), 1.18) && !worth_rerun(&tle(2.361), 1.18));
        // TL 2.01 s: isolate's TO starts at 2.011; 2.010 is under the limit (a wall-clock TLE)
        assert!(worth_rerun(&tle(2.011), 2.01) && !worth_rerun(&tle(2.01), 2.01));
        assert!(wall_clock_tle(&tle(2.01), 2.01) && !wall_clock_tle(&tle(2.011), 2.01));
    }

    #[test]
    fn an_aborted_job_does_not_wait_for_the_host() {
        let gate = Arc::new(RunGate::default());
        let script = [(Status::Tle, 1.3)].into_iter().collect();
        let mut scripted = Scripted { script, gate: gate.clone(), asked: vec![] };
        let abort = Some(Arc::new(AtomicBool::new(true)));
        let (wall_reruns, reruns) = (MAX_WALL_RERUNS, MAX_RERUNS);
        let mut judge =
            TleRerun { judge: &mut scripted, gate, time_limit_s: 1.0, budget: 2, wall_reruns, reruns, abort };
        let case = &plan(&format!("test_cases:\n{ONE_CASE}")).cases[0];
        assert_eq!(judge.judge_case(case), Err(GradeError::Aborted));
        assert_eq!(scripted.asked.len(), 1, "no re-run");
    }
}
