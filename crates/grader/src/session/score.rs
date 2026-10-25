//! Score sources: how a finished manager's run becomes a check outcome. `testlib_score`: testlib exit code plus
//! the message in its result file, as for interactors today. The problem checker and the plain manager exit join with split
//! signature and Communication.

use crate::checkers::CheckOutcome;
use crate::checkers::custom::{PartialScale, finished_code};
use crate::checkers::report_window::{End, read_report};
use crate::checkers::testlib::{self, Ended};
use crate::error::GradeError;
use judge_sandbox::IsolateBox;
use judge_sandbox::classify::RunVerdict;

/// Where a manager's stderr (the testlib message) is captured in the helper box.
pub const MANAGER_STDERR: &str = ".stderr";

pub fn testlib_score(
    helper: &IsolateBox,
    verdict: &RunVerdict,
    exit_code: Option<i32>,
    scale: PartialScale,
) -> Result<CheckOutcome, GradeError> {
    let code = finished_code(verdict, exit_code, "interactor")?;
    let as_interactor = |e: crate::checkers::CheckError| GradeError::Checker(format!("interactor: {}", e.0));
    let message = testlib::read_result(helper).map_err(as_interactor)?;
    let stderr = read_report(helper, MANAGER_STDERR, End::Tail);
    testlib::interpret(&Ended { code, stderr: &stderr, message: &message }, scale).map_err(as_interactor)
}
