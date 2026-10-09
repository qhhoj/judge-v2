//! `qhhoj-judge`: the judge daemon, `run` (one local grade) and `selftest` (one a+b per language).

mod local;
mod run_cmd;
mod selftest;

use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Parser)]
#[command(name = "qhhoj-judge", version, about = "QHHOJ judge daemon (judge-v2)")]
struct Cli {
    #[arg(long, default_value = "/etc/qhhoj-judge/config.toml", global = true)]
    config: PathBuf,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Judge one source against a local problem directory.
    Run(run_cmd::RunArgs),
    /// Compile and run an accepted a+b for every available language.
    Selftest(selftest::SelftestArgs),
}

/// 0 when `ok`, 1 when not, 2 on an error (a bad config included).
fn exit(ok: anyhow::Result<bool>) -> ExitCode {
    match ok {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(error) => {
            eprintln!("qhhoj-judge: {error:#}");
            ExitCode::from(2)
        }
    }
}

fn selftest_paths(
    config: &std::path::Path,
    args: &selftest::SelftestArgs,
) -> anyhow::Result<(PathBuf, PathBuf)> {
    if let (Some(languages), Some(testlib)) = (&args.languages, &args.testlib) {
        return Ok((languages.clone(), testlib.clone()));
    }
    let config = judge_daemon::config::load(config)?;
    Ok((config.languages_file, config.testlib_dir))
}

fn daemon(config: &std::path::Path) -> anyhow::Result<bool> {
    let runtime = tokio::runtime::Runtime::new()?;
    runtime.block_on(judge_daemon::run::run_daemon(config)).map(|()| true)
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    exit(match cli.command {
        Some(Command::Run(args)) => run_cmd::run(args),
        Some(Command::Selftest(args)) => {
            selftest_paths(&cli.config, &args).and_then(|(l, t)| selftest::selftest(&l, &t, &args))
        }
        None => daemon(&cli.config),
    })
}
