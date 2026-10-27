//! `check-config`: the daemon's config, data root, cache size, pinning and disk-bound checks (the bound on the box root) without starting
//! it; `selftest` covers the languages file. An install runs it with the new binary before it switches `current`,
//! so a config these checks refuse fails before the switch, not in a restart loop after it.

use judge_daemon::cores::{SYSFS_CPU, worker_cores};
use judge_sandbox::boxes::owned_box_ids;
use std::path::Path;

#[derive(clap::Args)]
pub struct CheckConfigArgs {
    /// The box the disk quota probe opens (and wipes); refused when it is a live slot's box.
    #[arg(long, default_value_t = 900)]
    pub probe_box: u32,
}

pub fn check_config(config_path: &Path, args: &CheckConfigArgs) -> anyhow::Result<bool> {
    let config = judge_daemon::config::load(config_path)?;
    judge_daemon::config::check_data_root(&config)?;
    let cores = worker_cores(&config, Path::new(SYSFS_CPU));
    let live = owned_box_ids(config.box_base, cores.len() as u32);
    anyhow::ensure!(
        !live.contains(&args.probe_box),
        "box {} belongs to a live slot: pass a --probe-box no slot uses",
        args.probe_box
    );
    let (_, bound) = judge_daemon::run::start_checks(&config, &cores, args.probe_box)?;
    warn_missing_preloads(&config.languages_file)?;
    println!("check-config: {} ok, disk bound {bound:?}", config_path.display());
    Ok(true)
}

/// A warning (not a failure) per installed language none of whose `preload` paths exists on this host.
fn warn_missing_preloads(languages_file: &Path) -> anyhow::Result<()> {
    let text = std::fs::read_to_string(languages_file)?;
    let registry = judge_grader::languages::LanguageRegistry::from_toml(&text)?;
    for language in judge_daemon::runtimes::unpreloaded(&registry) {
        let (lang, paths) = (&language.key, language.preload.join(" "));
        eprintln!(
            "check-config: warning: {lang}: no preload path exists ({paths}); runs may be charged for it"
        );
    }
    Ok(())
}
