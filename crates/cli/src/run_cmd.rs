//! `qhhoj-judge run`: judge one source against a local problem directory.

use crate::local::LocalJudge;
use judge_grader::judge::{Outcome, Submission, SubmissionSource};
use judge_grader::report::{CaseReport, Reporter};
use std::path::PathBuf;

#[derive(clap::Args)]
pub struct RunArgs {
    #[arg(long)]
    pub problem: PathBuf,
    #[arg(long)]
    pub lang: String,
    #[arg(long, default_value_t = 1.0)]
    pub time: f64,
    #[arg(long, default_value_t = 262_144)]
    pub memory: u64,
    #[arg(long, default_value = "/opt/qhhoj-judge/current/languages.toml")]
    pub languages: PathBuf,
    #[arg(long, default_value = "/opt/qhhoj-judge/current/assets/testlib")]
    pub testlib: PathBuf,
    #[arg(long, default_value_t = 900)]
    pub box_base: u32,
    pub source: PathBuf,
}

struct PrintReporter;

impl Reporter for PrintReporter {
    fn compiled(&mut self, log: &str) {
        if !log.trim().is_empty() {
            println!("compiler: {}", log.trim());
        }
    }
    fn started(&mut self, pretested: bool, case_count: u32) {
        println!("{case_count} cases{}", if pretested { " (pretests)" } else { "" });
    }
    fn case(&mut self, c: CaseReport) {
        let batch = c.batch.map(|b| format!(" [batch {b}]")).unwrap_or_default();
        println!(
            "#{} {} {:.3}s {}KiB {}/{}{batch} {}",
            c.position,
            c.status.code(),
            c.time_s,
            c.memory_kib,
            c.points,
            c.total,
            c.feedback
        );
    }
    fn before_case(&mut self) -> bool {
        true
    }
}

pub fn outcome_text(outcome: &Outcome) -> String {
    match outcome {
        Outcome::Graded => "graded".into(),
        Outcome::CompileError(log) => format!("compile error:\n{log}"),
        Outcome::InternalError(message) => format!("internal error: {message}"),
        Outcome::Aborted => "aborted".into(),
        Outcome::Retry(reason) => format!("retry: {reason:?}"),
    }
}

/// True when the submission was graded (whatever the verdicts).
pub fn run(args: RunArgs) -> anyhow::Result<bool> {
    let mut judge = LocalJudge::new(&args.languages, &args.testlib, args.box_base)?;
    let submission = Submission {
        language_key: args.lang,
        source: SubmissionSource::File(args.source),
        time_limit_s: args.time,
        memory_kib: args.memory,
        short_circuit: false,
        pretests_only: false,
    };
    let outcome = judge.judge(&args.problem, &submission, &mut PrintReporter)?;
    println!("{}", outcome_text(&outcome));
    Ok(outcome == Outcome::Graded)
}

#[cfg(test)]
mod tests {
    use super::*;
    use judge_grader::judge::RetryReason;

    #[test]
    fn every_outcome_has_a_distinct_line() {
        let lines = [
            outcome_text(&Outcome::Graded),
            outcome_text(&Outcome::CompileError("bad".into())),
            outcome_text(&Outcome::InternalError("boom".into())),
            outcome_text(&Outcome::Aborted),
            outcome_text(&Outcome::Retry(RetryReason::RuntimeMissing)),
        ];
        assert_eq!(lines[0], "graded");
        assert!(lines[1].contains("bad") && lines[2].contains("boom") && lines[4].contains("RuntimeMissing"));
        let distinct: std::collections::HashSet<_> = lines.iter().collect();
        assert_eq!(distinct.len(), lines.len());
    }
}
