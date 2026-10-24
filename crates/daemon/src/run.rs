//! Wires the daemon together (docs/architecture.md): config → logs → languages this host can run → fresh cache
//! dirs → owned boxes cleaned → workers → metrics → link; on SIGTERM or SIGINT it drains and stops.

use crate::config::{Config, load};
use crate::cores::{SYSFS_CPU, worker_cores};
use crate::cpuset::{Pinning, host_pinning};
use crate::drain::wait_until;
use crate::gates::Gates;
use crate::generation::GrpcGeneration;
use crate::link::{Connection, LinkContext, SlotPool, run_link};
use crate::metrics::{Metrics, serve};
use crate::outbox::{OUTBOX_WATERMARK, Outboxes};
use crate::pool::Pool;
use crate::runtimes::{available, detect_versions};
use crate::source::GrpcFetch;
use crate::wire::{FEATURES, HelloInfo};
use crate::worker::{FETCH_TIMEOUT, Runner, WorkerDeps};
use anyhow::Context;
use judge_grader::artefacts::{ArtefactSetup, Artefacts};
use judge_grader::judge::{GraderDefaults, JudgeEnv};
use judge_grader::languages::LanguageRegistry;
use judge_problem::archive::ZipLimits;
use judge_problem::data_version::{FsStamps, StampSource, WaitPolicy};
use judge_problem::dir_cache::DirCache;
use judge_problem::resolve::Resolver;
use judge_sandbox::boxes::{box_pair, cleanup_owned};
use judge_sandbox::{DiskQuota, IsolateBox, SandboxConfig, disk_quota_works};
use std::collections::HashMap;
use std::future::Future;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{RwLock, watch};

const DATA_POLL: Duration = Duration::from_millis(250);
const DRAIN_POLL: Duration = Duration::from_millis(200);

/// What the workers share: one stamp source for the resolver and every `JudgeEnv` (the end-of-grade
/// recheck must read stamps the way the resolver did).
struct Shared {
    cache: Arc<DirCache>,
    artefacts: Arc<Artefacts>,
    stamps: Arc<dyn StampSource>,
}

/// The running parts the link and the drain need.
struct Daemon {
    pool: Arc<Pool>,
    outboxes: Arc<Outboxes>,
    metrics: Arc<Metrics>,
    connection: Arc<RwLock<Option<Connection>>>,
    hello: HelloInfo,
}

/// Stale cache entries are never trusted: every start begins with empty cache dirs.
fn fresh_dirs(config: &Config) -> anyhow::Result<()> {
    for name in ["data", "artefacts", "work", "meta"] {
        let dir = config.cache_dir.join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    }
    Ok(())
}

fn sandbox_config(config: &Config) -> SandboxConfig {
    SandboxConfig {
        isolate_bin: config.isolate.bin.clone(),
        taskset_bin: config.isolate.taskset.clone(),
        meta_dir: config.cache_dir.join("meta"),
        disk_quota: config
            .isolate
            .disk_quota
            .map(|q| DiskQuota { headroom_kib: q.headroom_kib, headroom_inodes: q.headroom_inodes }),
    }
}

/// The sandbox config, without its disk quota (warned once) when isolate's box root cannot take one.
fn checked_sandbox(config: &Config) -> anyhow::Result<SandboxConfig> {
    let mut sandbox = sandbox_config(config);
    if sandbox.disk_quota.is_some() && !disk_quota_works(&sandbox, config.box_base)? {
        tracing::warn!("isolate's box root has no user quotas: runs get no disk quota (DEPLOY.md)");
        sandbox.disk_quota = None;
    }
    Ok(sandbox)
}

/// Logs how the boxes are kept on their cores; refuses to start when isolate would fail every run.
fn check_pinning(config: &Config, cores: &[u32]) -> anyhow::Result<()> {
    match host_pinning(&config.isolate.config, config.box_base, cores) {
        Pinning::Cpuset => tracing::info!("boxes pinned to their cores by cpuset"),
        Pinning::TasksetOnly(boxes) => tracing::warn!(
            ?boxes,
            "boxes pinned by taskset only: a run can move to another core (DEPLOY.md, `qhhoj-judge isolate-cpus`)"
        ),
        Pinning::Broken(reason) => anyhow::bail!("{reason}"),
    }
    Ok(())
}

fn grader_defaults(config: &Config) -> GraderDefaults {
    GraderDefaults {
        default_output_limit: config.limits.default_output_limit,
        default_output_prefix: config.limits.default_output_prefix,
        jvm_reserve_kib: config.limits.jvm_reserve_kib,
    }
}

/// Two boxes per worker (solution, helper), pinned to the worker's core.
fn runners(
    config: &Config,
    sandbox: &SandboxConfig,
    cores: &[u32],
    shared: &Shared,
) -> anyhow::Result<Vec<Box<dyn Runner>>> {
    let mut runners: Vec<Box<dyn Runner>> = Vec::new();
    for (worker, core) in cores.iter().enumerate() {
        let ids = box_pair(config.box_base, worker as u32).context("box ids overflow")?;
        let scratch = config.cache_dir.join("work").join(worker.to_string());
        std::fs::create_dir_all(&scratch)?;
        runners.push(Box::new(JudgeEnv {
            solution: IsolateBox::open(sandbox, ids.solution, Some(*core))?,
            helper: IsolateBox::open(sandbox, ids.helper, Some(*core))?,
            artefacts: shared.artefacts.clone(),
            stamps: shared.stamps.clone(),
            scratch,
            defaults: grader_defaults(config),
        }));
    }
    Ok(runners)
}

/// The languages this host can run, the Hello that announces them, and their build ids.
fn languages(config: &Config) -> anyhow::Result<(LanguageRegistry, HelloInfo, HashMap<String, String>)> {
    let text = std::fs::read_to_string(&config.languages_file).context("languages_file")?;
    let registry = LanguageRegistry::from_toml(&text)?;
    let versions = detect_versions(&registry);
    let registry = available(registry, &versions);
    let runtimes = registry
        .keys()
        .into_iter()
        .filter_map(|key| {
            Some((key.to_string(), registry.get(key)?.name.clone(), versions.get(key)?.clone()))
        })
        .collect();
    let hello = HelloInfo {
        protocol_minor: judge_proto::PROTOCOL_MINOR,
        build: env!("CARGO_PKG_VERSION").into(),
        runtimes,
        features: FEATURES.map(String::from).to_vec(),
    };
    Ok((registry, hello, versions))
}

fn resolver(config: &Config, shared: &Shared) -> Resolver {
    Resolver {
        root: config.problem_data_root.clone(),
        cache: shared.cache.clone(),
        wait: WaitPolicy { timeout: Duration::from_millis(config.data_wait_ms), poll: DATA_POLL },
        zip: ZipLimits::for_cache(config.cache_max_bytes),
        stamps: shared.stamps.clone(),
    }
}

fn shared(config: &Config, registry: LanguageRegistry, build_ids: HashMap<String, String>) -> Shared {
    let cache = Arc::new(DirCache::new(config.cache_dir.join("data"), config.cache_max_bytes));
    let setup = ArtefactSetup {
        cache: cache.clone(),
        registry: Arc::new(registry),
        build_ids,
        testlib_dir: config.testlib_dir.clone(),
    };
    Shared { cache, artefacts: Arc::new(Artefacts::new(setup)), stamps: Arc::new(FsStamps) }
}

fn build(config: &Arc<Config>) -> anyhow::Result<Daemon> {
    let (registry, hello, versions) = languages(config)?;
    let cores = worker_cores(config, Path::new(SYSFS_CPU));
    fresh_dirs(config)?;
    for (id, error) in cleanup_owned(&sandbox_config(config), config.box_base, cores.len() as u32) {
        tracing::warn!(box_id = id, %error, "startup cleanup failed");
    }
    check_pinning(config, &cores)?;
    let sandbox = checked_sandbox(config)?;
    let shared = shared(config, registry, versions);
    let runners = runners(config, &sandbox, &cores, &shared)?;
    let (outboxes, metrics) = (Arc::new(Outboxes::new(OUTBOX_WATERMARK)), Arc::new(Metrics::new()));
    let connection: Arc<RwLock<Option<Connection>>> = Arc::default();
    let deps = WorkerDeps {
        judge: config.name.clone(),
        outboxes: outboxes.clone(),
        resolver: Arc::new(resolver(config, &shared)),
        fetch: Arc::new(GrpcFetch { config: config.clone(), connection: connection.clone() }),
        generation: Arc::new(GrpcGeneration { config: config.clone(), connection: connection.clone() }),
        gates: Gates { dir: config.control_dir.clone() },
        metrics: metrics.clone(),
        scratch: config.cache_dir.join("work"),
        fetch_timeout: FETCH_TIMEOUT,
    };
    let pool = Pool::start(Arc::new(deps), runners);
    tokio::spawn(serve(
        config.metrics_listen.clone(),
        metrics.clone(),
        sampler(&pool, &outboxes, shared.cache),
    ));
    Ok(Daemon { pool, outboxes, metrics, connection, hello })
}

fn sampler(
    pool: &Arc<Pool>,
    outboxes: &Arc<Outboxes>,
    cache: Arc<DirCache>,
) -> Arc<dyn Fn(&Metrics) + Send + Sync> {
    let (pool, outboxes) = (pool.clone(), outboxes.clone());
    Arc::new(move |m: &Metrics| {
        m.slots_busy.set(pool.busy() as i64);
        m.outbox_bytes.set(outboxes.total_bytes() as i64);
        m.cache_hits_total.set(cache.hits() as i64);
    })
}

/// The daemon until SIGTERM or SIGINT, then the drain.
pub async fn run_daemon(config_path: &Path) -> anyhow::Result<()> {
    let config = Arc::new(load(config_path)?);
    crate::logging::init(&config.name);
    run_until(config, shutdown_signal()).await
}

/// Runs until `shutdown` resolves; then announces `Draining`, waits up to `drain_timeout_s` for an idle
/// pool and empty outboxes, and stops the link.
pub async fn run_until(config: Arc<Config>, shutdown: impl Future<Output = ()>) -> anyhow::Result<()> {
    let daemon = build(&config)?;
    let (draining_tx, draining) = watch::channel(false);
    let (stop_tx, stop) = watch::channel(false);
    let ctx = LinkContext {
        config: config.clone(),
        outboxes: daemon.outboxes.clone(),
        pool: daemon.pool.clone() as Arc<dyn SlotPool>,
        hello: daemon.hello,
        connection: daemon.connection,
        metrics: daemon.metrics,
        draining,
    };
    let link = tokio::spawn(run_link(ctx, stop));
    shutdown.await;
    tracing::info!(judge = %config.name, "draining");
    daemon.pool.set_draining();
    let _ = draining_tx.send(true);
    let (pool, outboxes) = (daemon.pool, daemon.outboxes);
    let idle = || pool.idle() && outboxes.refs().is_empty();
    let drained = wait_until(idle, Duration::from_secs(config.drain_timeout_s), DRAIN_POLL).await;
    tracing::info!(judge = %config.name, drained, "stopping");
    let _ = stop_tx.send(true);
    let _ = link.await;
    Ok(())
}

async fn shutdown_signal() {
    use tokio::signal::unix::{SignalKind, signal};
    let mut term = signal(SignalKind::terminate()).expect("SIGTERM handler");
    tokio::select! {
        _ = term.recv() => {}
        _ = tokio::signal::ctrl_c() => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_start_empties_the_cache_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = crate::config::parse(crate::config::tests_support::MIN).unwrap();
        config.cache_dir = dir.path().into();
        std::fs::create_dir_all(dir.path().join("data/p@3")).unwrap();
        std::fs::write(dir.path().join("data/p@3/init.yml"), "stale").unwrap();
        std::fs::write(dir.path().join("keep.txt"), "not ours").unwrap();
        fresh_dirs(&config).unwrap();
        for name in ["data", "artefacts", "work", "meta"] {
            assert_eq!(std::fs::read_dir(dir.path().join(name)).unwrap().count(), 0, "{name} is empty");
        }
        assert!(dir.path().join("keep.txt").exists(), "only the daemon's own dirs are reset");
    }
}
