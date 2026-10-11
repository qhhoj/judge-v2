//! Test generation for a Polygon standard package (feature `generate.polygon`).
//!
//! A bundle (a zip of sources, headers and manual inputs) and a plan go in; a zip of every test's input and
//! answer comes out. Everything the package ships runs in the worker's two isolate boxes: the programs are
//! compiled with the headers beside them, each distinct generator command runs once, the validator checks
//! every input, the main solution makes every answer. The job stops at the first failure, at the deadline
//! and on cancel (both are checked between runs, not inside one). Manual inputs are copied with LF line
//! endings, as Polygon's Linux package has them.
//!
//! A plan with no tests is a compile check: its checker and interactor programs are compiled and the
//! result zip is empty.

mod failure;
mod line_endings;
mod pack;
mod phases;
mod plan;
mod programs;
mod run;

pub use failure::{CANCELLED, GenerationFailure, Kind, Stage, verdict_failure};
pub use plan::{
    GenerationPlan, PlanLimits, PlanProgram, PlanTest, ProgramLimits, Role, TestSource, check_plan,
};

use crate::judge::JudgeEnv;
use judge_problem::archive::{ZipLimits, extract_zip};
use pack::{Budget, write_result};
use phases::Generation;
use plan::is_compile_check;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

/// Entries and unpacked bytes a bundle may expand to.
const BUNDLE_LIMITS: ZipLimits = ZipLimits { max_entries: 10_000, max_total_bytes: 1 << 30 };
/// The job's files live here, under the worker's scratch directory.
const WORK_DIR: &str = "generation";

/// The deadline and the cancel flag, checked between runs.
pub struct Control<'a> {
    pub deadline: Instant,
    pub abort: &'a AtomicBool,
}

impl Control<'_> {
    pub fn check(&self) -> Result<(), GenerationFailure> {
        if self.abort.load(Ordering::SeqCst) {
            return Err(GenerationFailure::cancelled());
        }
        if Instant::now() >= self.deadline {
            return Err(GenerationFailure::job(Kind::JobTimeLimit, "the generation deadline passed"));
        }
        Ok(())
    }
}

/// Where the job reads its bundle and writes its result.
pub struct GenerationJob {
    pub plan: GenerationPlan,
    pub bundle_zip: PathBuf,
    pub result_zip: PathBuf,
}

/// The job's scratch directory, removed when the job ends however it ends.
struct Workdir(PathBuf);

impl Workdir {
    fn create(path: PathBuf) -> Result<Workdir, GenerationFailure> {
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(path.join("inputs"))?;
        std::fs::create_dir_all(path.join("answers"))?;
        Ok(Workdir(path))
    }
}

impl Drop for Workdir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn unpack_bundle(zip: &Path, into: &Path) -> Result<(), GenerationFailure> {
    extract_zip(zip, into, &BUNDLE_LIMITS)
        .map_err(|error| GenerationFailure::internal(format!("the bundle cannot be unpacked: {error}")))
}

/// Runs the job on the worker's boxes; on success `job.result_zip` holds the tests.
pub fn generate_tests(
    env: &mut JudgeEnv,
    job: &GenerationJob,
    control: &Control,
) -> Result<(), GenerationFailure> {
    let plan = &job.plan;
    check_plan(plan).map_err(|reason| GenerationFailure::internal(format!("invalid plan: {reason}")))?;
    let work = Workdir::create(env.scratch.join(WORK_DIR))?;
    let bundle = work.0.join("bundle");
    unpack_bundle(&job.bundle_zip, &bundle)?;
    let programs =
        programs::compile_all(env, plan, (&bundle, &work.0.join("programs")), &|| control.check())?;
    if is_compile_check(plan) {
        return write_result(
            &[],
            |_| unreachable!("a compile check has no tests"),
            (&job.result_zip, plan.limits.max_upload_bytes),
        );
    }
    let budget = Budget::new(plan.limits.max_total_bytes);
    let mut generation = Generation { env, plan, control, programs, bundle, files: work.0.clone(), budget };
    generation.generate_inputs()?;
    generation.validate_inputs()?;
    generation.solve_inputs()?;
    let files = |position| (generation.input_path(position), generation.answer_path(position));
    write_result(&plan.tests, files, (&job.result_zip, plan.limits.max_upload_bytes))
}
