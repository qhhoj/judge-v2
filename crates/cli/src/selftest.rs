//! `selftest`: every row of the conformance manifest (`tests/programs/manifest.toml`, shipped in the release)
//! judged by the real grader with the installed languages file and isolate, on two spare boxes pinned to one
//! slot's core at a low priority, so the live judge keeps running. Fails on any verdict that differs from the
//! manifest's and on a required language the judge does not advertise; the others are skipped.

use crate::local::{Boxes, LocalJudge};
use judge_daemon::config::Config;
use judge_daemon::cores::{SYSFS_CPU, worker_cores};
use judge_grader::languages::{LanguageRegistry, host_languages};
use judge_grader::manifest::{self, Manifest, Row};
use judge_grader::report::{CaseReport, Reporter};
use judge_sandbox::SandboxConfig;
use judge_sandbox::boxes::owned_box_ids;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

#[derive(clap::Args)]
pub struct SelftestArgs {
    #[arg(long)]
    pub languages: Option<PathBuf>,
    #[arg(long)]
    pub testlib: Option<PathBuf>,
    /// The directory holding manifest.toml and its programs; default: `tests/programs` next to the binary.
    #[arg(long)]
    pub programs: Option<PathBuf>,
    /// The first of two boxes; refused when either is a live slot's box.
    #[arg(long, default_value_t = 900)]
    pub box_base: u32,
    /// The CPU to run on; default: the last slot's core (from the config), unpinned without a config.
    #[arg(long)]
    pub core: Option<u32>,
    /// Niceness of the selftest and its boxes: the live slot sharing the core keeps priority.
    #[arg(long, default_value_t = 10)]
    pub nice: i32,
    #[arg(long, value_delimiter = ',')]
    pub require: Vec<String>,
    /// Write the host copy of the languages file (runtimes resolved past `/etc`) here and self-test it.
    #[arg(long)]
    pub host_languages: Option<PathBuf>,
}

/// What the selftest runs, and where.
pub struct Plan {
    languages: PathBuf,
    testlib: PathBuf,
    programs: PathBuf,
    boxes: Boxes,
    live_boxes: Vec<u32>,
}

/// The daemon config; none only when it is absent and both files it would name are given.
fn daemon_config(path: &Path, args: &SelftestArgs) -> anyhow::Result<Option<Config>> {
    let files_given = args.languages.is_some() && args.testlib.is_some();
    if files_given && !path.exists() {
        return Ok(None);
    }
    Ok(Some(judge_daemon::config::load(path)?))
}

/// `tests/programs` next to the running binary (the release and image layout).
fn default_programs() -> anyhow::Result<PathBuf> {
    let exe = std::env::current_exe()?.canonicalize()?;
    let dir = exe.parent().ok_or_else(|| anyhow::anyhow!("binary has no directory"))?;
    Ok(dir.join("tests/programs"))
}

fn plan_with_config(config: &Config, args: &SelftestArgs, programs: PathBuf) -> Plan {
    let cores = worker_cores(config, Path::new(SYSFS_CPU));
    let sandbox = SandboxConfig {
        isolate_bin: config.isolate.bin.clone(),
        taskset_bin: config.isolate.taskset.clone(),
        ..SandboxConfig::default()
    };
    let core = args.core.or(cores.last().copied());
    Plan {
        languages: args.languages.clone().unwrap_or_else(|| config.languages_file.clone()),
        testlib: args.testlib.clone().unwrap_or_else(|| config.testlib_dir.clone()),
        programs,
        boxes: Boxes { base: args.box_base, core, sandbox, defaults: manifest::DEFAULTS },
        live_boxes: owned_box_ids(config.box_base, cores.len() as u32),
    }
}

/// The plan from the flags and, when present, the daemon config at `config_path`.
pub fn plan(config_path: &Path, args: &SelftestArgs) -> anyhow::Result<Plan> {
    let programs = match &args.programs {
        Some(dir) => dir.clone(),
        None => default_programs()?,
    };
    let plan = match daemon_config(config_path, args)? {
        Some(config) => plan_with_config(&config, args, programs),
        None => Plan {
            languages: args.languages.clone().unwrap_or_default(),
            testlib: args.testlib.clone().unwrap_or_default(),
            programs,
            boxes: Boxes { core: args.core, defaults: manifest::DEFAULTS, ..Boxes::unpinned(args.box_base) },
            live_boxes: Vec::new(),
        },
    };
    let own = [plan.boxes.base, plan.boxes.base.saturating_add(1)];
    if let (Some(id), Some(last)) =
        (own.iter().find(|id| plan.live_boxes.contains(id)), plan.live_boxes.last())
    {
        anyhow::bail!("box {id} belongs to a live slot: pick a --box-base above {last}");
    }
    Ok(plan)
}

/// Prints each runtime isolate cannot reach; with `out`, writes the rewritten copy and returns its path.
fn host_copy(languages: &Path, out: Option<&Path>) -> anyhow::Result<PathBuf> {
    let text = std::fs::read_to_string(languages)?;
    let registry = LanguageRegistry::from_toml(&text)?;
    let (host, rewrites) = host_languages(&text, &registry);
    for (from, to) in &rewrites {
        println!("rewrite {from} -> {to}");
    }
    let Some(out) = out else { return Ok(languages.to_path_buf()) };
    std::fs::write(out, host)?;
    println!("wrote {}", out.display());
    Ok(out.to_path_buf())
}

/// Raises this thread's niceness; isolate and the programs it runs inherit it.
fn lower_priority(nice: i32) -> anyhow::Result<()> {
    // SAFETY: setpriority only reads its integer arguments.
    let failed = unsafe { libc::setpriority(libc::PRIO_PROCESS, 0, nice) } != 0;
    if failed {
        anyhow::bail!("setpriority {nice}: {}", std::io::Error::last_os_error());
    }
    Ok(())
}

#[derive(Default)]
struct Cases(Vec<CaseReport>);

impl Reporter for Cases {
    fn compiled(&mut self, _: &str) {}
    fn started(&mut self, _: bool, _: u32) {}
    fn case(&mut self, c: CaseReport) {
        self.0.push(c);
    }
    fn before_case(&mut self) -> bool {
        true
    }
}

/// The row's verdict and its first case (time and memory).
fn judge_row(
    judge: &mut LocalJudge,
    problem: &Path,
    programs: &Path,
    row: &Row,
) -> anyhow::Result<(String, Option<CaseReport>)> {
    let source = std::fs::read(programs.join(&row.file))
        .map_err(|e| anyhow::anyhow!("{}: {e}", programs.join(&row.file).display()))?;
    let jvm = judge.registry.get(&row.lang).is_some_and(|l| l.jvm);
    let mut cases = Cases::default();
    let outcome = judge.judge(problem, &manifest::submission(&row.lang, jvm, source), &mut cases)?;
    let first = cases.0.into_iter().next();
    Ok((manifest::verdict(&outcome, first.as_ref().map(|c| c.status.code())), first))
}

fn print_row(row: &Row, got: &str, case: Option<&CaseReport>) {
    let (time, memory) = case.map_or((0.0, 0), |c| (c.time_s, c.memory_kib));
    let mark = if got == row.expect { "ok" } else { "MISMATCH" };
    println!(
        "{:<7} {:<26} {:<4} {:<4} {time:>6.3}s {memory:>7}K  {mark}",
        row.lang, row.file, row.expect, got
    );
}

/// Rows of languages the judge advertises are judged; the others are skipped.
#[derive(Default)]
struct Tally {
    judged: usize,
    mismatched: usize,
    skipped: BTreeSet<String>,
}

fn run_rows(judge: &mut LocalJudge, programs: &Path, rows: &[Row]) -> anyhow::Result<Tally> {
    let work = tempfile::tempdir()?;
    let problem = work.path().join("aplusb");
    manifest::write_problem(&problem, &manifest::CASES)?;
    println!("{:<7} {:<26} {:<4} {:<4} {:>7} {:>8}", "LANG", "FILE", "WANT", "GOT", "TIME", "MEM");
    let mut tally = Tally::default();
    for row in rows {
        if judge.registry.get(&row.lang).is_none() {
            tally.skipped.insert(row.lang.clone());
            continue;
        }
        let (got, case) = judge_row(judge, &problem, programs, row)?;
        print_row(row, &got, case.as_ref());
        tally.judged += 1;
        tally.mismatched += usize::from(got != row.expect);
    }
    Ok(tally)
}

/// Required keys the judge does not advertise or the manifest has no row for.
fn missing_required(required: &[String], judge: &LocalJudge, rows: &[Row]) -> Vec<String> {
    let has_rows = |key: &String| rows.iter().any(|row| &row.lang == key);
    let missing = required.iter().filter(|key| judge.registry.get(key).is_none() || !has_rows(key));
    missing.cloned().collect()
}

fn print_placement(plan: &Plan, nice: i32) {
    let core = plan.boxes.core.map_or("any core".to_string(), |c| format!("core {c}"));
    let live = match (plan.live_boxes.first(), plan.live_boxes.last()) {
        (Some(first), Some(last)) => format!("live slots own boxes {first}-{last}"),
        _ => "no daemon config".to_string(),
    };
    let base = plan.boxes.base;
    println!("selftest: boxes {base}-{} on {core}, nice {nice}; {live}", base + 1);
}

pub fn selftest(plan: Plan, args: &SelftestArgs) -> anyhow::Result<bool> {
    let manifest_file = plan.programs.join("manifest.toml");
    let text = std::fs::read_to_string(&manifest_file)
        .map_err(|e| anyhow::anyhow!("{}: {e} (pass --programs)", manifest_file.display()))?;
    let rows = Manifest::parse(&text)?.expanded();
    lower_priority(args.nice)?;
    let languages = host_copy(&plan.languages, args.host_languages.as_deref())?;
    print_placement(&plan, args.nice);
    let mut judge = LocalJudge::new(&languages, &plan.testlib, plan.boxes)?;
    let tally = run_rows(&mut judge, &plan.programs, &rows)?;
    let missing = missing_required(&args.require, &judge, &rows);
    let skipped: Vec<_> = tally.skipped.iter().filter(|key| !missing.contains(key)).collect();
    println!("skipped (not advertised): {skipped:?}");
    if !missing.is_empty() {
        println!("required but not advertised or without rows: {missing:?}");
    }
    let ok = tally.mismatched == 0 && missing.is_empty() && tally.judged > 0;
    let result = if ok { "ok" } else { "FAIL" };
    println!("selftest: {} judged, {} mismatched: {result}", tally.judged, tally.mismatched);
    Ok(ok)
}
