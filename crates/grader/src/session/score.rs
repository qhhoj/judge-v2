//! Score sources: how a finished manager's run becomes a check outcome. `testlib_score`: testlib exit code plus
//! stderr message, as for interactors today. The problem checker and the plain manager exit join with split
//! signature and Communication.

use crate::checkers::CheckOutcome;
use crate::checkers::custom::{ExitReport, PartialScale, finished_code, interpret};
use crate::checkers::select::CustomKind;
use crate::error::GradeError;
use judge_sandbox::IsolateBox;
use judge_sandbox::classify::RunVerdict;

/// Where a manager's stderr (the testlib message) is captured in the helper box.
pub const MANAGER_STDERR: &str = ".stderr";
const MAX_MANAGER_STDERR: u64 = 64 << 10;

pub fn testlib_score(
    helper: &IsolateBox,
    verdict: &RunVerdict,
    exit_code: Option<i32>,
    scale: PartialScale,
) -> Result<CheckOutcome, GradeError> {
    let code = finished_code(verdict, exit_code, "interactor")?;
    let stderr = helper.read_output(MANAGER_STDERR, MAX_MANAGER_STDERR).unwrap_or_default();
    let report = ExitReport { code, stdout: String::new(), stderr: String::from_utf8_lossy(&stderr).into() };
    interpret(CustomKind::Testlib, &report, scale)
        .map_err(|e| GradeError::Checker(format!("interactor: {}", e.0)))
}
