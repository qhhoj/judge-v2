//! `selftest`: every row of the conformance manifest (`tests/programs/manifest.toml`, shipped in the release)
//! judged by the real grader with the installed languages file and isolate, on two spare boxes pinned to a
//! core no slot owns at a low priority, so the live judge keeps running. Fails on any verdict that differs from the
//! manifest's and on a required language the judge does not advertise; the others are skipped.

use crate::local::{Boxes, LocalJudge};
use judge_daemon::config::Config;
use judge_daemon::cores::{SYSFS_CPU, physical_cores, worker_cores};
use judge_grader::languages::{LanguageRegistry, host_languages};
use judge_grader::manifest::{self, Manifest, Row};
use judge_grader::report::{CaseReport, Reporter};
use judge_sandbox::boxes::owned_box_ids;
use judge_sandbox::{RunGate, SandboxConfig};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

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
    /// The CPU to run on, never a slot's; default: the last core no slot owns (unpinned without a config).
    #[arg(long)]
    pub core: Option<u32>,
    /// Niceness of the selftest and its boxes.
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

/// `--core`, else the last core no slot owns; never a slot's core, so live verdicts keep their core.
fn spare_core(requested: Option<u32>, slot_cores: &[u32], sysfs: &Path) -> anyhow::Result<u32> {
    if let Some(core) = requested {
        anyhow::ensure!(!slot_cores.contains(&core), "core {core} runs a slot: pass a --core no slot uses");
        return Ok(core);
    }
    let spare = physical_cores(sysfs).into_iter().rev().find(|core| !slot_cores.contains(core));
    spare
        .ok_or_else(|| anyhow::anyhow!("every core runs a slot ({slot_cores:?}): pass a --core no slot uses"))
}

/// Refuses boxes that are a live slot's (`live`) or past the last box id.
fn check_boxes(base: u32, live: &[u32]) -> anyhow::Result<()> {
    let helper =
        base.checked_add(1).ok_or_else(|| anyhow::anyhow!("--box-base {base} is the last box id"))?;
    if let (Some(id), Some(last)) = ([base, helper].iter().find(|id| live.contains(id)), live.last()) {
        anyhow::bail!("box {id} belongs to a live slot: pick a --box-base above {last}");
    }
    Ok(())
}

fn plan_with_config(
    config: &Config,
    args: &SelftestArgs,
    programs: PathBuf,
    sysfs: &Path,
) -> anyhow::Result<Plan> {
    let slot_cores = worker_cores(config, sysfs);
    let live_boxes = owned_box_ids(config.box_base, slot_cores.len() as u32);
    check_boxes(args.box_base, &live_boxes)?;
    let sandbox = SandboxConfig {
        isolate_bin: config.isolate.bin.clone(),
        taskset_bin: config.isolate.taskset.clone(),
        gate: Arc::new(RunGate::new(Some(config.run_lock()))),
        ..SandboxConfig::default()
    };
    let core = Some(spare_core(args.core, &slot_cores, sysfs)?);
    Ok(Plan {
        languages: args.languages.clone().unwrap_or_else(|| config.languages_file.clone()),
        testlib: args.testlib.clone().unwrap_or_else(|| config.testlib_dir.clone()),
        programs,
        boxes: Boxes { base: args.box_base, core, sandbox, defaults: manifest::DEFAULTS },
        live_boxes,
    })
}

/// The plan from the flags and, when present, the daemon config at `config_path`.
pub fn plan(config_path: &Path, args: &SelftestArgs) -> anyhow::Result<Plan> {
    let programs = match &args.programs {
        Some(dir) => dir.clone(),
        None => default_programs()?,
    };
    match daemon_config(config_path, args)? {
        Some(config) => plan_with_config(&config, args, programs, Path::new(SYSFS_CPU)),
        None => {
            check_boxes(args.box_base, &[])?;
            Ok(Plan {
                languages: args.languages.clone().unwrap_or_default(),
                testlib: args.testlib.clone().unwrap_or_default(),
                programs,
                boxes: Boxes {
                    core: args.core,
                    defaults: manifest::DEFAULTS,
                    ..Boxes::unpinned(args.box_base)
                },
                live_boxes: Vec::new(),
            })
        }
    }
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

/// The row's statuses (`manifest::statuses`) and every case's report.
fn judge_row(
    judge: &mut LocalJudge,
    problems: &Path,
    programs: &Path,
    row: &Row,
) -> anyhow::Result<(Vec<String>, Vec<CaseReport>)> {
    let source = std::fs::read(programs.join(&row.file))
        .map_err(|e| anyhow::anyhow!("{}: {e}", programs.join(&row.file).display()))?;
    let jvm = judge.registry.get(&row.lang).is_some_and(|l| l.jvm);
    let mut cases = Cases::default();
    let problem = problems.join(row.problem());
    let outcome = judge.judge(&problem, &manifest::submission(&row.lang, jvm, source), &mut cases)?;
    let codes: Vec<&str> = cases.0.iter().map(|c| c.status.code()).collect();
    Ok((manifest::statuses(&outcome, &codes), cases.0))
}

/// One code when every case has it, else the codes joined: `AC`, `AC,WA`, `CE`.
fn short(statuses: &[String]) -> String {
    let uniform = statuses.len() == manifest::CASES.len() && statuses.iter().all(|s| *s == statuses[0]);
    if uniform { statuses[0].clone() } else { statuses.join(",") }
}

/// The row with the slowest case's time and the largest memory.
fn print_row(row: &Row, got: &[String], cases: &[CaseReport]) {
    let time = cases.iter().map(|c| c.time_s).fold(0.0, f64::max);
    let memory = cases.iter().map(|c| c.memory_kib).max().unwrap_or(0);
    let mark = if matches(row, got, cases) { "ok" } else { "MISMATCH" };
    let (want, got) = (short(&row.expected()), short(got));
    println!("{:<7} {:<26} {want:<5} {got:<5} {time:>6.3}s {memory:>7}K  {mark}", row.lang, row.file);
}

fn matches(row: &Row, got: &[String], cases: &[CaseReport]) -> bool {
    row.matches(got, &cases.iter().map(|c| c.points).collect::<Vec<_>>())
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
    manifest::write_problems(work.path(), programs)?;
    println!("{:<7} {:<26} {:<5} {:<5} {:>7} {:>8}", "LANG", "FILE", "WANT", "GOT", "TIME", "MEM");
    let mut tally = Tally::default();
    for row in rows {
        if judge.registry.get(&row.lang).is_none() {
            tally.skipped.insert(row.lang.clone());
            continue;
        }
        let (got, cases) = judge_row(judge, work.path(), programs, row)?;
        print_row(row, &got, &cases);
        tally.judged += 1;
        tally.mismatched += usize::from(!matches(row, &got, &cases));
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
    let rows =
        Manifest::parse(&text).map_err(|e| anyhow::anyhow!("{}: {e}", manifest_file.display()))?.expanded();
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

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser)]
    struct Cli {
        #[command(flatten)]
        args: SelftestArgs,
    }

    /// cpu0..cpu5, one thread per core (the production shape: slots on cores 2-5).
    fn six_cores() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for cpu in 0..6 {
            let topology = dir.path().join(format!("cpu{cpu}/topology"));
            std::fs::create_dir_all(&topology).unwrap();
            std::fs::write(topology.join("thread_siblings_list"), format!("{cpu}\n")).unwrap();
        }
        dir
    }

    fn plan_for(box_base: u32, flags: &[&str]) -> anyhow::Result<Plan> {
        let text = format!(
            "name = 'j'\nbridge_url = 'http://127.0.0.1:1'\nkey_file = '/k'\nproblem_data_root = '/d'\n\
             cache_dir = '/c'\nlanguages_file = '/l'\ntestlib_dir = '/t'\ncores = [2, 3, 4, 5]\nbox_base = {box_base}\n"
        );
        let config = judge_daemon::config::parse(&text).unwrap();
        let args = Cli::parse_from([&["selftest"][..], flags].concat()).args;
        plan_with_config(&config, &args, "/p".into(), six_cores().path())
    }

    #[test]
    fn the_selftest_never_takes_a_live_slots_box_or_core() {
        let free = plan_for(0, &[]).unwrap();
        assert_eq!((free.boxes.base, free.boxes.core), (900, Some(1)), "boxes 900-901 on the last free core");
        assert!(plan_for(0, &["--core", "0"]).is_ok());
        let refusals = [plan_for(0, &["--core", "3"]), plan_for(895, &[]), plan_for(0, &["--box-base", "7"])];
        for refused in refusals {
            assert!(refused.is_err(), "a slot's core or box was accepted");
        }
        assert!(
            plan_for(0, &["--box-base", &u32::MAX.to_string()]).is_err(),
            "the helper box would wrap to 0"
        );
    }
}
