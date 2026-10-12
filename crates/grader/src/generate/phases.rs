//! The three passes over the tests: generate every input, validate every input, solve every input.
//! Test `position` (0-based, in plan order) names its files; `GenerationTest.index` is only reported.

use super::Control;
use super::failure::{GenerationFailure, Kind, Stage, verdict_failure};
use super::line_endings::copy_lf;
use super::pack::Budget;
use super::plan::{GenerationPlan, PlanTest, ProgramLimits, TestSource};
use super::programs::bundle_file;
use super::run::{ProgramExit, ProgramRun, run_program, validator_args};
use crate::file_io::SolutionFileIo;
use crate::judge::JudgeEnv;
use crate::program::Program;
use crate::run_case::{CaseIo, CaseLimits, Runnable, run_case};
use judge_sandbox::classify::RunVerdict;
use std::collections::HashMap;
use std::fs::File;
use std::path::{Path, PathBuf};

/// A generator command: the same command twice makes the same input, so it runs once.
type Command = (String, Vec<String>);

pub struct Generation<'a> {
    pub env: &'a mut JudgeEnv,
    pub plan: &'a GenerationPlan,
    pub control: &'a Control<'a>,
    /// Compiled programs by name.
    pub programs: HashMap<String, Program>,
    /// The unpacked bundle, and where inputs and answers are kept (`inputs/<n>`, `answers/<n>`).
    pub bundle: PathBuf,
    pub files: PathBuf,
    pub budget: Budget,
}

/// The compiled program of a plan program.
fn compiled<'p>(
    programs: &'p HashMap<String, Program>,
    name: &str,
) -> Result<&'p Program, GenerationFailure> {
    programs.get(name).ok_or_else(|| GenerationFailure::internal(format!("program {name} was not compiled")))
}

/// A run with no stdin and discarded stdout, to be adjusted by the caller.
fn run_of<'r>(
    program: &'r Program,
    (limits, args): (&'r ProgramLimits, &'r [String]),
    (output_cap, jvm_reserve_kib): (u64, u64),
) -> ProgramRun<'r> {
    ProgramRun { program, args, stdin: None, stdout_to: None, limits, output_cap, jvm_reserve_kib }
}

impl Generation<'_> {
    pub fn input_path(&self, position: usize) -> PathBuf {
        self.files.join("inputs").join(position.to_string())
    }

    pub fn answer_path(&self, position: usize) -> PathBuf {
        self.files.join("answers").join(position.to_string())
    }

    pub fn generate_inputs(&mut self) -> Result<(), GenerationFailure> {
        let mut made: HashMap<Command, usize> = HashMap::new();
        for (position, test) in self.plan.tests.iter().enumerate() {
            self.control.check()?;
            match test.source() {
                Some(TestSource::Manual(path)) => self.copy_manual(position, test, path)?,
                Some(TestSource::Generated { generator, args }) => {
                    let command = (generator.to_string(), args.to_vec());
                    self.generate_one((position, test.index), &command, &made)?;
                    made.entry(command).or_insert(position);
                }
                None => return Err(GenerationFailure::internal("test without a source")),
            }
            self.budget.charge(file_len(&self.input_path(position))?)?;
        }
        Ok(())
    }

    fn copy_manual(&self, position: usize, test: &PlanTest, path: &str) -> Result<(), GenerationFailure> {
        let missing = || {
            GenerationFailure::new(Stage::Generate, Kind::MissingFile, format!("{path} is not in the bundle"))
                .at_test(test.index)
        };
        let source = bundle_file(&self.bundle, path).ok_or_else(missing)?;
        if file_len(&source)? > self.plan.limits.max_file_bytes {
            let detail = format!("{path} is larger than {} bytes", self.plan.limits.max_file_bytes);
            return Err(
                GenerationFailure::new(Stage::Generate, Kind::OutputLimit, detail).at_test(test.index)
            );
        }
        copy_lf(File::open(source)?, File::create(self.input_path(position))?)?;
        Ok(())
    }

    /// Runs the command, or copies the input an earlier test with the same command made.
    fn generate_one(
        &mut self,
        (position, index): (usize, u32),
        command: &Command,
        made: &HashMap<Command, usize>,
    ) -> Result<(), GenerationFailure> {
        if let Some(&first) = made.get(command) {
            std::fs::copy(self.input_path(first), self.input_path(position))?;
            return Ok(());
        }
        let program = compiled(&self.programs, &command.0)?;
        let output = self.input_path(position);
        let caps = (self.plan.limits.max_file_bytes, self.env.defaults.jvm_reserve_kib);
        let mut run = run_of(program, (&self.plan.limits.generator, &command.1), caps);
        run.stdout_to = Some(&output);
        let exit = run_program(&mut self.env.solution, &run)?;
        crashed_or_ok(&exit, Stage::Generate).map_err(|failure| failure.of_program(&command.0).at_test(index))
    }

    pub fn validate_inputs(&mut self) -> Result<(), GenerationFailure> {
        let Some(validator) = self.plan.validator() else { return Ok(()) };
        for (position, test) in self.plan.tests.iter().enumerate() {
            self.control.check()?;
            self.validate_one(position, test, &validator.name)
                .map_err(|failure| failure.of_program(&validator.name).at_test(test.index))?;
        }
        Ok(())
    }

    fn validate_one(
        &mut self,
        position: usize,
        test: &PlanTest,
        name: &str,
    ) -> Result<(), GenerationFailure> {
        let program = compiled(&self.programs, name)?;
        let input = File::open(self.input_path(position))?;
        let args = validator_args(&test.group);
        let caps = (self.plan.limits.max_file_bytes, self.env.defaults.jvm_reserve_kib);
        let mut run = run_of(program, (&self.plan.limits.validator, &args), caps);
        run.stdin = Some(&input);
        let exit = run_program(&mut self.env.solution, &run)?;
        if let RunVerdict::Ir { .. } = exit.verdict {
            return Err(GenerationFailure::new(Stage::Validate, Kind::Rejected, exit.stderr));
        }
        crashed_or_ok(&exit, Stage::Validate)
    }

    pub fn solve_inputs(&mut self) -> Result<(), GenerationFailure> {
        let solution = self.plan.solution().ok_or_else(|| GenerationFailure::internal("no solution"))?;
        for (position, test) in self.plan.tests.iter().enumerate() {
            self.control.check()?;
            self.solve_one(position, &solution.name)
                .map_err(|failure| failure.of_program(&solution.name).at_test(test.index))?;
        }
        Ok(())
    }

    fn solve_one(&mut self, position: usize, name: &str) -> Result<(), GenerationFailure> {
        let program = compiled(&self.programs, name)?;
        let limits = &self.plan.limits.solution;
        let case_limits = CaseLimits {
            time_limit_s: limits.time_s,
            memory_kib: limits.memory_kib,
            jvm_reserve_kib: self.env.defaults.jvm_reserve_kib,
        };
        let input = File::open(self.input_path(position))?;
        let file_io = self.plan.file_io.as_ref().map(|io| SolutionFileIo::new(io, b""));
        let io =
            CaseIo { input: &input, output_cap: self.plan.limits.max_file_bytes, file_io: file_io.as_ref() };
        let runnable = Runnable { program, limits: &case_limits };
        let answer = self.answer_path(position);
        let case = run_case(&mut self.env.solution, &runnable, &io, &answer)?;
        if let Some((kind, detail)) = verdict_failure(&case.verdict) {
            return Err(GenerationFailure::new(Stage::Solve, kind, detail));
        }
        self.budget.charge(case.output_len)
    }
}

fn file_len(path: &Path) -> Result<u64, GenerationFailure> {
    Ok(std::fs::metadata(path)?.len())
}

/// Ok for a clean exit; else the failure of the run at `stage` (a crash, or the program's stderr
/// with it when there is any).
fn crashed_or_ok(exit: &ProgramExit, stage: Stage) -> Result<(), GenerationFailure> {
    let Some((kind, mut detail)) = verdict_failure(&exit.verdict) else { return Ok(()) };
    if kind == Kind::Crashed && !exit.stderr.is_empty() {
        detail = format!("{detail}: {}", exit.stderr);
    }
    Err(GenerationFailure::new(stage, kind, detail))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exit(verdict: RunVerdict, stderr: &str) -> ProgramExit {
        ProgramExit { verdict, stderr: stderr.into() }
    }

    #[test]
    fn a_crash_carries_the_programs_stderr_and_other_failures_do_not() {
        assert!(crashed_or_ok(&exit(RunVerdict::Ok, "noise"), Stage::Generate).is_ok());
        let crashed = crashed_or_ok(&exit(RunVerdict::Ir { exit_code: 3 }, "FAIL bad n"), Stage::Generate);
        assert_eq!(crashed.unwrap_err().detail, "exit code 3: FAIL bad n");
        let silent = crashed_or_ok(&exit(RunVerdict::Ir { exit_code: 1 }, ""), Stage::Validate).unwrap_err();
        assert_eq!(
            (silent.stage, silent.kind, silent.detail.as_str()),
            (Stage::Validate, Kind::Crashed, "exit code 1")
        );
        let slow = crashed_or_ok(&exit(RunVerdict::Tle, "x"), Stage::Generate).unwrap_err();
        assert_eq!((slow.kind, slow.detail.as_str()), (Kind::TimeLimit, "time limit exceeded"));
    }
}
