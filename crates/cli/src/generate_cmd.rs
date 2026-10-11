//! `qhhoj-judge generate`: build the tests of a Polygon standard package locally, with the daemon's code.

use crate::local::LocalJudge;
use anyhow::Context;
use judge_daemon::wire::{generation_plan, v1};
use judge_grader::generate::{Control, GenerationFailure, GenerationJob, GenerationPlan};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

/// The plan's `deadlineS` when it names none.
const DEFAULT_DEADLINE_S: u32 = 1800;

#[derive(clap::Args)]
pub struct GenerateArgs {
    /// The bundle zip: sources, headers and manual inputs.
    #[arg(long)]
    pub bundle: PathBuf,
    /// A `GenerateTests` message as proto3 JSON (camelCase).
    #[arg(long)]
    pub plan: PathBuf,
    /// Where the result zip of inputs and answers goes.
    #[arg(long)]
    pub out: PathBuf,
    #[arg(long, default_value = "/opt/qhhoj-judge/current/languages.toml")]
    pub languages: PathBuf,
    #[arg(long, default_value = "/opt/qhhoj-judge/current/assets/testlib")]
    pub testlib: PathBuf,
    #[arg(long, default_value_t = 900)]
    pub box_base: u32,
}

fn read_plan(path: &Path) -> anyhow::Result<GenerationPlan> {
    let text = std::fs::read_to_string(path).with_context(|| format!("plan {}", path.display()))?;
    let message: v1::GenerateTests = serde_json::from_str(&text).context("plan is not a GenerateTests")?;
    let mut plan = generation_plan(message);
    if plan.deadline_s == 0 {
        plan.deadline_s = DEFAULT_DEADLINE_S;
    }
    Ok(plan)
}

/// One line: where it failed, what happened, and the program's own words.
pub fn failure_text(failure: &GenerationFailure) -> String {
    let program = if failure.program.is_empty() { String::new() } else { format!(" in {}", failure.program) };
    let test =
        if failure.test_index == 0 { String::new() } else { format!(" at test {}", failure.test_index) };
    let detail = if failure.detail.is_empty() { String::new() } else { format!(": {}", failure.detail) };
    format!("generation failed ({:?}, {:?}{program}{test}){detail}", failure.stage, failure.kind)
}

/// True when the result zip was written; a failed generation prints its failure and is false.
pub fn generate(args: GenerateArgs) -> anyhow::Result<bool> {
    let plan = read_plan(&args.plan)?;
    let deadline = Instant::now() + Duration::from_secs(plan.deadline_s.into());
    let mut judge = LocalJudge::new(&args.languages, &args.testlib, args.box_base)?;
    let job = GenerationJob { plan, bundle_zip: args.bundle, result_zip: args.out };
    let abort = AtomicBool::new(false);
    match judge.generate(&job, &Control { deadline, abort: &abort }) {
        Ok(()) => {
            println!("{}", job.result_zip.display());
            Ok(true)
        }
        Err(failure) => {
            println!("{}", failure_text(&failure));
            Ok(false)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use judge_grader::generate::{Kind, Stage};

    #[test]
    fn a_failure_names_stage_kind_program_test_and_detail_when_it_has_them() {
        let failure =
            GenerationFailure::new(Stage::Validate, Kind::Rejected, "bad n").of_program("val").at_test(7);
        assert_eq!(failure_text(&failure), "generation failed (Validate, Rejected in val at test 7): bad n");
        let job = GenerationFailure::job(Kind::JobTimeLimit, "");
        assert_eq!(failure_text(&job), "generation failed (Job, JobTimeLimit)");
    }

    #[test]
    fn a_plan_without_a_deadline_gets_the_default_and_a_bad_plan_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("plan.json");
        std::fs::write(&path, r#"{"generationId":"g","tests":[{"index":1,"inputName":"1.in","answerName":"1.out","manualPath":"t/1"}]}"#)
            .unwrap();
        let plan = read_plan(&path).unwrap();
        assert_eq!((plan.deadline_s, plan.tests.len()), (DEFAULT_DEADLINE_S, 1));
        std::fs::write(&path, "{\"nope\": 1}").unwrap();
        assert!(read_plan(&path).is_err());
        assert!(read_plan(&dir.path().join("missing.json")).is_err());
    }
}
