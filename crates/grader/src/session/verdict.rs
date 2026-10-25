//! How a finished session becomes a verdict: one predicate per rule, composed in order by each preset's `decide`
//! (interactive: `crate::interactor::decide`; split signature: `decide_split`).

use judge_sandbox::classify::RunVerdict;
use judge_sandbox::spec::Limits;

/// One box's classified result.
#[derive(Debug)]
pub struct MemberRun {
    pub verdict: RunVerdict,
    pub time_s: f64,
    pub memory_kib: u64,
}

const SIGPIPE_REASON: &str = "SIGPIPE";
/// testlib's exit code for `_fail` (a jury error).
const TESTLIB_FAIL_EXIT: i32 = 3;
/// testlib's exit codes for `_wa` and `_points`: verdicts formed from what the interactor read. Its other
/// non-zero codes (2 is also testlib's unexpected EOF, 4, 8) can be the symptom of a solution that died.
const TESTLIB_JUDGED_EXITS: [i32; 2] = [1, 7];

/// A solution that failed on its own wins over the interactor, with two exceptions where the failure is the
/// consequence of the interactor having finished first: SIGPIPE (it wrote to the closed pipe), and an
/// exception or non-zero exit (EOF, `BrokenPipeError`) after the interactor quit with WA or partial points.
/// A crash by any other signal stays the solution's own.
pub fn solution_wins(solution: &RunVerdict, interactor: &RunVerdict) -> bool {
    let interactor_judged =
        matches!(interactor, RunVerdict::Ir { exit_code } if TESTLIB_JUDGED_EXITS.contains(exit_code));
    match solution {
        RunVerdict::Ok => false,
        RunVerdict::Rte { reason } if reason == SIGPIPE_REASON => false,
        RunVerdict::Rte { reason } if reason.starts_with("SIG") => true,
        RunVerdict::Rte { .. } | RunVerdict::Ir { .. } => !interactor_judged,
        _ => true,
    }
}

/// The interactor itself failed: testlib's `_fail` (exit 3) or a crash other than a broken pipe. The case is
/// then IE whatever the solution did (a solution that read the dead interactor's EOF is not to blame).
pub fn interactor_failed(verdict: &RunVerdict) -> bool {
    match verdict {
        RunVerdict::Ir { exit_code } => *exit_code == TESTLIB_FAIL_EXIT,
        RunVerdict::Rte { reason } => reason != SIGPIPE_REASON,
        _ => false,
    }
}

/// A solution killed by the wall clock within its CPU limit, while the interactor used more CPU than the
/// wall allows on top of that limit, was waiting for the interactor: jury time, not the contestant's TLE.
/// This also catches an interactor that hangs, which the wall limit ends. Policy: a problem whose
/// interactor needs that much CPU cannot be judged within these limits, so it is IE, not a verdict.
pub fn stalled_by_jury(contestant: &MemberRun, jury_work_s: f64, limits: &Limits) -> bool {
    contestant.verdict == RunVerdict::Tle
        && contestant.time_s < limits.time_s
        && jury_work_s > limits.wall_s - limits.time_s
}

/// What a finished split signature pair left (`crate::split::run`).
pub struct SplitFacts<'a> {
    /// The contestant box's own verdict and CPU.
    pub contestant: &'a MemberRun,
    /// Jury CPU spent serving the contestant's calls (its callbacks), which one process would have charged.
    pub served_s: f64,
    pub jury: &'a RunVerdict,
    pub jury_time_s: f64,
    /// The jury read a frame from the contestant that breaks the protocol.
    pub protocol_fault: bool,
    /// The contestant's limits.
    pub limits: &'a Limits,
}

impl SplitFacts<'_> {
    /// The contestant's time: its own CPU and the callbacks it called, as one process would have counted.
    pub fn charged_s(&self) -> f64 {
        self.contestant.time_s + self.served_s
    }

    /// Jury CPU that was not spent on the contestant's calls.
    fn jury_own_s(&self) -> f64 {
        (self.jury_time_s - self.served_s).max(0.0)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum SplitDecision {
    /// The case fails with this verdict; the time shown is `charged_s`, the memory the contestant's.
    Failed(RunVerdict),
    /// WA: only contestant code can break its own side of the channel.
    ProtocolViolation,
    /// IE: the contestant stayed within its limits and the jury's own work ran out the clock.
    JuryStall,
    /// Both ended cleanly: the problem's checker decides on the jury's output.
    Check,
}

/// The split signature rules, first match wins. A verdict is one a single process running both would have got,
/// counted in CPU (never in host load): the side that failed first decides, and the other side then only sees
/// EOF and exits 0. No contestant exit code, signal or output gives IE; only the jury's own work can.
/// 1. The contestant over its memory or output limit: MLE, OLE.
/// 2. Its charged time (own CPU + callbacks served) over the TL: TLE.
/// 3. Its wall clock ran out: IE when the jury's own CPU used the waiting time (`stalled_by_jury`), else TLE.
/// 4. A protocol violation the jury recorded: WA.
/// 5. The contestant crashed (not SIGPIPE) or exited non-zero (any code): RTE or IR.
/// 6. The jury over its time limit (the contestant within its own): IE.
/// 7. The jury failed in any other way (memory, a signal, a non-zero exit): that verdict.
/// 8. Otherwise the checker.
pub fn decide_split(facts: &SplitFacts) -> SplitDecision {
    let contestant = &facts.contestant.verdict;
    if matches!(contestant, RunVerdict::Mle | RunVerdict::Ole) {
        return SplitDecision::Failed(contestant.clone());
    }
    if let Some(decision) = out_of_time(facts) {
        return decision;
    }
    if facts.protocol_fault {
        return SplitDecision::ProtocolViolation;
    }
    if contestant_failed(contestant) {
        return SplitDecision::Failed(contestant.clone());
    }
    match facts.jury {
        RunVerdict::Ok => SplitDecision::Check,
        RunVerdict::Tle => SplitDecision::JuryStall,
        failed => SplitDecision::Failed(failed.clone()),
    }
}

/// Rules 2 and 3.
fn out_of_time(facts: &SplitFacts) -> Option<SplitDecision> {
    if facts.charged_s() > facts.limits.time_s {
        return Some(SplitDecision::Failed(RunVerdict::Tle));
    }
    if facts.contestant.verdict != RunVerdict::Tle {
        return None;
    }
    let waited = MemberRun { verdict: RunVerdict::Tle, time_s: facts.charged_s(), memory_kib: 0 };
    if stalled_by_jury(&waited, facts.jury_own_s(), facts.limits) {
        return Some(SplitDecision::JuryStall);
    }
    Some(SplitDecision::Failed(RunVerdict::Tle))
}

/// SIGPIPE is the glue's quiet end after the jury exited (it ignores SIGPIPE; contestant code may not).
fn contestant_failed(verdict: &RunVerdict) -> bool {
    match verdict {
        RunVerdict::Rte { reason } => reason != SIGPIPE_REASON,
        RunVerdict::Ir { .. } => true,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rte(reason: &str) -> RunVerdict {
        RunVerdict::Rte { reason: reason.into() }
    }

    #[test]
    fn the_interactor_fails_on_fail_and_crashes() {
        let table = [
            (RunVerdict::Ok, false),
            (RunVerdict::Ir { exit_code: 1 }, false), // testlib WA
            (RunVerdict::Ir { exit_code: 3 }, true),  // testlib FAIL
            (rte("SIGSEGV"), true),
            (rte("SIGPIPE"), false),  // the solution hung up
            (RunVerdict::Tle, false), // a hang is caught by `stalled_by_jury`
        ];
        for (verdict, failed) in table {
            assert_eq!(interactor_failed(&verdict), failed, "{verdict:?}");
        }
    }

    #[test]
    fn the_solutions_own_failure_wins_unless_it_followed_the_interactors_verdict() {
        // The interactor's testlib exit code: 0 ok, 1 WA, 2 PE (also unexpected EOF), 7 points.
        let (ok, wa, eof, points) = (0, 1, 2, 7);
        let interactor = |code| if code == 0 { RunVerdict::Ok } else { RunVerdict::Ir { exit_code: code } };
        // (solution, interactor, the solution's verdict stands)
        let table = [
            (RunVerdict::Ok, wa, false),
            (rte("SIGPIPE"), ok, false),
            (rte("SIGSEGV"), eof, true), // it crashed and the interactor read EOF
            (RunVerdict::Ir { exit_code: 1 }, eof, true),
            (rte("SIGSEGV"), ok, true),
            (rte("SIGSEGV"), wa, true), // a real crash, whatever the interactor made of the EOF
            (rte("EOFError"), wa, false), // the interactor quit with WA first
            (rte("BrokenPipeError"), points, false),
            (RunVerdict::Ir { exit_code: 1 }, wa, false),
            (RunVerdict::Tle, wa, true),
            (RunVerdict::Mle, wa, true),
        ];
        for (solution, code, wins) in table {
            assert_eq!(solution_wins(&solution, &interactor(code)), wins, "{solution:?} vs exit {code}");
        }
    }

    #[test]
    fn a_wall_timeout_is_the_jurys_when_it_used_the_waiting_time() {
        let limits = Limits::for_solution(1.0, 1 << 18, 1, 1 << 20); // 1 s CPU, 4 s wall
        let tle = |time_s| MemberRun { verdict: RunVerdict::Tle, time_s, memory_kib: 0 };
        assert!(stalled_by_jury(&tle(0.01), 5.0, &limits), "idle solution, busy interactor");
        assert!(!stalled_by_jury(&tle(0.01), 0.01, &limits), "both idle: a deadlock is the solution's");
        assert!(!stalled_by_jury(&tle(1.5), 5.0, &limits), "it used up its own CPU limit");
        let ok = MemberRun { verdict: RunVerdict::Ok, ..tle(0.01) };
        assert!(!stalled_by_jury(&ok, 5.0, &limits));
    }

    /// 1 s CPU, 4 s wall.
    static LIMITS: std::sync::LazyLock<Limits> =
        std::sync::LazyLock::new(|| Limits::for_solution(1.0, 1 << 18, 1, 1 << 20));

    #[test]
    fn the_split_rules_apply_in_order() {
        use SplitDecision::*;
        let (ok, tle, mle) = (RunVerdict::Ok, RunVerdict::Tle, RunVerdict::Mle);
        let segv = rte("SIGSEGV");
        let ir = |exit_code| RunVerdict::Ir { exit_code };
        // (contestant, its CPU, served, jury, jury CPU, protocol fault, decision)
        let table = [
            (ok.clone(), 0.1, 0.1, ok.clone(), 0.2, false, Check),
            (mle.clone(), 0.1, 0.0, segv.clone(), 0.1, true, Failed(mle.clone())),
            (RunVerdict::Ole, 0.1, 0.0, ir(1), 0.1, false, Failed(RunVerdict::Ole)),
            (ok.clone(), 0.6, 0.5, ok.clone(), 0.6, false, Failed(tle.clone())), // callbacks count
            (segv.clone(), 0.2, 0.9, ok.clone(), 1.0, false, Failed(tle.clone())), // over the TL first
            (tle.clone(), 1.2, 0.0, ok.clone(), 0.1, true, Failed(tle.clone())), // its own CPU
            (tle.clone(), 0.01, 0.0, tle.clone(), 5.0, false, JuryStall),        // the jury's own work
            (tle.clone(), 0.01, 0.0, ok.clone(), 0.01, false, Failed(tle.clone())), // it idled itself
            (tle.clone(), 0.3, 0.6, ok.clone(), 3.4, false, Failed(tle.clone())), // 2.8 s of own jury CPU
            (ok.clone(), 0.1, 0.0, ok.clone(), 0.1, true, ProtocolViolation),
            (ir(117), 0.1, 0.0, ok.clone(), 0.1, true, ProtocolViolation),
            (ir(117), 0.1, 0.0, ok.clone(), 0.1, false, Failed(ir(117))), // any exit code is its own
            (segv.clone(), 0.1, 0.0, ok.clone(), 0.1, false, Failed(segv.clone())),
            (segv.clone(), 0.1, 0.0, tle.clone(), 9.0, false, Failed(segv.clone())), // it crashed first
            (rte("SIGPIPE"), 0.1, 0.0, ok.clone(), 0.1, false, Check),               // the jury had finished
            (rte("SIGPIPE"), 0.1, 0.0, ir(3), 0.1, false, Failed(ir(3))),
            (ok.clone(), 0.1, 0.1, tle.clone(), 10.0, false, JuryStall),
            (ok.clone(), 0.1, 0.0, mle.clone(), 0.1, false, Failed(mle.clone())), // the jury's memory is the program's
            (ok.clone(), 0.1, 0.0, segv.clone(), 0.1, false, Failed(segv.clone())),
        ];
        for (verdict, time_s, served_s, jury, jury_time_s, protocol_fault, decision) in table {
            let contestant = MemberRun { verdict, time_s, memory_kib: 0 };
            let facts = SplitFacts {
                contestant: &contestant,
                served_s,
                jury: &jury,
                jury_time_s,
                protocol_fault,
                limits: &LIMITS,
            };
            assert_eq!(decide_split(&facts), decision, "{contestant:?} {served_s} / {jury:?} {jury_time_s}");
        }
    }
}
