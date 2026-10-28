//! `/etc/qhhoj-judge/config.toml` (docs/operations.md#config).

use judge_grader::rerun::DEFAULT_TLE_RERUNS;
use judge_problem::archive::{BACKEND_ARCHIVE_MAX_BYTES, MIN_CACHE_MAX_BYTES};
use judge_problem::plan::MAX_OUTPUT_LIMIT;
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("config: {0}")]
    Invalid(String),
    #[error("config file: {0}")]
    Io(#[from] std::io::Error),
}

fn invalid(message: impl Into<String>) -> ConfigError {
    ConfigError::Invalid(message.into())
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TlsConfig {
    pub ca_file: PathBuf,
    #[serde(default)]
    pub server_name: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct LimitDefaults {
    pub default_output_limit: u64,
    pub default_output_prefix: u64,
    pub jvm_reserve_kib: u64,
    /// TLE re-runs alone that may confirm a TLE per submission (docs/grading.md#tle-re-run); 0 turns them off.
    pub tle_reruns: u32,
}

impl Default for LimitDefaults {
    fn default() -> Self {
        LimitDefaults {
            default_output_limit: 25_165_824,
            default_output_prefix: 64,
            jvm_reserve_kib: 131_072,
            tle_reruns: DEFAULT_TLE_RERUNS,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Reconnect {
    pub initial_ms: u64,
    pub max_ms: u64,
}

impl Default for Reconnect {
    fn default() -> Self {
        Reconnect { initial_ms: 500, max_ms: 30_000 }
    }
}

/// A per-box disk quota (isolate `--quota`, `kib` in 1 KiB blocks), as headroom: each box's quota is this
/// plus the files the judge puts in it and the output the run may write.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiskQuotaConfig {
    pub headroom_kib: u32,
    pub headroom_inodes: u32,
}

/// Smallest headroom: a run's stderr or another file up to the largest output limit, plus 64 MiB for a
/// compiler's temporary files. Less would fail a legal run with EDQUOT.
pub const MIN_HEADROOM_KIB: u64 = (MAX_OUTPUT_LIMIT + (64 << 20)) / 1024;
/// Fewest headroom inodes: a compile's temporary files and a JVM program's class files.
pub const MIN_HEADROOM_INODES: u32 = 256;
/// isolate refuses numbers above a C `int`.
const ISOLATE_MAX_NUMBER: u32 = i32::MAX as u32;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct IsolatePaths {
    pub bin: PathBuf,
    pub taskset: PathBuf,
    /// isolate's own config, read at start to see whether its `box<id>.cpus` lines pin the boxes.
    pub config: PathBuf,
    /// Off when absent. Needs user quotas on isolate's box root (DEPLOY.md).
    pub disk_quota: Option<DiskQuotaConfig>,
    /// Dev and test stacks only (loopback bridge): start with no `disk_quota` on a box root that shares the
    /// cache's or `/`'s filesystem, where one run can fill the disk.
    pub allow_unbounded_disk: bool,
}

impl Default for IsolatePaths {
    fn default() -> Self {
        IsolatePaths {
            bin: "/usr/local/bin/isolate".into(),
            taskset: "/usr/bin/taskset".into(),
            config: "/usr/local/etc/isolate".into(),
            disk_quota: None,
            allow_unbounded_disk: false,
        }
    }
}

fn default_cache_max() -> u64 {
    20 << 30
}
fn default_metrics() -> String {
    "127.0.0.1:9464".into()
}
fn default_drain() -> u64 {
    120
}
fn default_data_wait() -> u64 {
    10_000
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub name: String,
    pub bridge_url: String,
    pub key_file: PathBuf,
    #[serde(default)]
    pub tls: Option<TlsConfig>,
    #[serde(default)]
    pub slots: Option<u32>,
    #[serde(default)]
    pub cores: Option<Vec<u32>>,
    pub problem_data_root: PathBuf,
    pub cache_dir: PathBuf,
    #[serde(default = "default_cache_max")]
    pub cache_max_bytes: u64,
    pub languages_file: PathBuf,
    pub testlib_dir: PathBuf,
    #[serde(default = "default_metrics")]
    pub metrics_listen: String,
    #[serde(default)]
    pub box_base: u32,
    #[serde(default = "default_drain")]
    pub drain_timeout_s: u64,
    #[serde(default = "default_data_wait")]
    pub data_wait_ms: u64,
    /// Dev and e2e only: hold files `hold-ack`, `hold-begin`, `hold-end` (see `gates.rs`).
    #[serde(default)]
    pub control_dir: Option<PathBuf>,
    #[serde(default)]
    pub limits: LimitDefaults,
    #[serde(default)]
    pub reconnect: Reconnect,
    #[serde(default)]
    pub isolate: IsolatePaths,
}

impl Config {
    /// The host lock of the run gate (`judge_sandbox::run_gate`), shared by the daemon and `selftest`: in the
    /// cache directory, which both run as the judge user, and which a restart does not wipe.
    pub fn run_lock(&self) -> PathBuf {
        self.cache_dir.join("run.lock")
    }
}

/// Parsed with `http::Uri`; a URL with userinfo (`http://localhost:1@evil.com`) is never loopback.
pub fn is_loopback_url(url: &str) -> bool {
    let Ok(uri) = url.parse::<http::Uri>() else { return false };
    let Some(authority) = uri.authority() else { return false };
    let http_scheme = matches!(uri.scheme_str(), Some("http" | "https"));
    let host = authority.host().trim_start_matches('[').trim_end_matches(']');
    http_scheme && !authority.as_str().contains('@') && matches!(host, "127.0.0.1" | "::1" | "localhost")
}

fn validate(config: &Config) -> Result<(), ConfigError> {
    if config.name.trim().is_empty() {
        return Err(invalid("name is empty"));
    }
    let loopback = config.bridge_url.starts_with("http://") && is_loopback_url(&config.bridge_url);
    if !loopback && !(config.bridge_url.starts_with("https://") && config.tls.is_some()) {
        return Err(invalid("a non-loopback bridge needs https:// and [tls] ca_file"));
    }
    if config.control_dir.is_some() && !is_loopback_url(&config.bridge_url) {
        return Err(invalid("control_dir is for local test stacks only"));
    }
    if config.isolate.allow_unbounded_disk && !is_loopback_url(&config.bridge_url) {
        return Err(invalid("isolate.allow_unbounded_disk is for local test stacks only"));
    }
    validate_disk_quota(config.isolate.disk_quota)
}

fn validate_disk_quota(quota: Option<DiskQuotaConfig>) -> Result<(), ConfigError> {
    match quota {
        Some(q) if u64::from(q.headroom_kib) < MIN_HEADROOM_KIB => {
            Err(invalid(format!("isolate.disk_quota.headroom_kib must be at least {MIN_HEADROOM_KIB}")))
        }
        Some(q) if q.headroom_inodes < MIN_HEADROOM_INODES => {
            Err(invalid(format!("isolate.disk_quota.headroom_inodes must be at least {MIN_HEADROOM_INODES}")))
        }
        Some(q) if q.headroom_kib.max(q.headroom_inodes) > ISOLATE_MAX_NUMBER => {
            Err(invalid(format!("isolate.disk_quota values must be at most {ISOLATE_MAX_NUMBER}")))
        }
        _ => Ok(()),
    }
}

pub fn parse(text: &str) -> Result<Config, ConfigError> {
    let config: Config = toml::from_str(text).map_err(|e| invalid(e.to_string()))?;
    validate(&config)?;
    Ok(config)
}

pub fn load(path: &Path) -> Result<Config, ConfigError> {
    parse(&std::fs::read_to_string(path)?)
}

/// One archive may fill half the cache (`ZipLimits::for_cache`): a smaller cache than `MIN_CACHE_MAX_BYTES` could
/// not extract an archive the backend accepts, an internal error on every submission to that problem.
pub fn check_cache_size(config: &Config) -> Result<(), ConfigError> {
    if config.cache_max_bytes < MIN_CACHE_MAX_BYTES {
        return Err(invalid(format!(
            "cache_max_bytes must be at least {MIN_CACHE_MAX_BYTES}: half of it must hold the largest archive the \
             backend accepts ({BACKEND_ARCHIVE_MAX_BYTES} bytes)"
        )));
    }
    Ok(())
}

/// `cache_dir` must be the judge user's alone (no group or other bits): it holds the run gate's lock files
/// (`Config::run_lock`), which anyone who can open them could hold for ever, stalling every run. A `cache_dir`
/// that does not exist yet is fine: the daemon creates it 0700 (`run::fresh_dirs`), as systemd's
/// `CacheDirectoryMode=0700` does.
pub fn check_cache_dir_private(config: &Config) -> Result<(), ConfigError> {
    use std::os::unix::fs::PermissionsExt;
    let mode = match std::fs::metadata(&config.cache_dir) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        meta => meta?.permissions().mode() & 0o777,
    };
    if mode & 0o077 != 0 {
        return Err(invalid(format!(
            "cache_dir {} has mode {mode:o}: it must be 0700, the judge user's alone (it holds the run lock)",
            config.cache_dir.display()
        )));
    }
    Ok(())
}

/// A default of 0 would make every problem without its own `output_limit_length` an output-limit failure.
pub fn check_output_limit(config: &Config) -> Result<(), ConfigError> {
    if config.limits.default_output_limit == 0 {
        return Err(invalid("default_output_limit must be at least 1 byte"));
    }
    Ok(())
}

/// The daemon waits for data that never comes when `problem_data_root` is not there (a missing mount).
pub fn check_data_root(config: &Config) -> Result<(), ConfigError> {
    if !config.problem_data_root.is_dir() {
        return Err(invalid(format!(
            "problem_data_root {} is not a directory",
            config.problem_data_root.display()
        )));
    }
    Ok(())
}

impl Config {
    pub fn read_key(&self) -> Result<String, ConfigError> {
        let credentials = std::env::var_os("CREDENTIALS_DIRECTORY").map(PathBuf::from);
        self.read_key_in(credentials.as_deref())
    }

    pub fn read_key_in(&self, credentials: Option<&Path>) -> Result<String, ConfigError> {
        let path = match credentials {
            Some(dir) if self.key_file.is_relative() => dir.join(&self.key_file),
            _ => self.key_file.clone(),
        };
        let key = std::fs::read_to_string(path)?.trim().to_string();
        if key.is_empty() {
            return Err(invalid("judge key file is empty"));
        }
        Ok(key)
    }
}

#[cfg(test)]
pub mod tests_support {
    pub const MIN: &str = "name = 'j1'\nbridge_url = 'http://127.0.0.1:9990'\nkey_file = '/k'\nproblem_data_root = '/d'\ncache_dir = '/c'\nlanguages_file = '/l'\ntestlib_dir = '/t'\n";
}

#[cfg(test)]
mod tests {
    use super::tests_support::MIN;
    use super::*;

    #[test]
    fn a_cache_must_hold_the_largest_backend_archive() {
        let sized = |bytes: u64| parse(&format!("{MIN}cache_max_bytes = {bytes}\n")).unwrap();
        assert!(check_cache_size(&sized(MIN_CACHE_MAX_BYTES - 1)).is_err());
        assert!(check_cache_size(&sized(MIN_CACHE_MAX_BYTES)).is_ok());
    }

    #[test]
    fn a_cache_dir_others_can_reach_is_refused() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let mut config = parse(MIN).unwrap();
        config.cache_dir = dir.path().join("missing");
        assert!(check_cache_dir_private(&config).is_ok(), "the daemon creates it 0700");
        config.cache_dir = dir.path().into();
        for (mode, private) in [(0o700, true), (0o750, false), (0o705, false)] {
            std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(mode)).unwrap();
            assert_eq!(check_cache_dir_private(&config).is_ok(), private, "mode {mode:o}");
        }
    }

    #[test]
    fn the_default_output_limit_must_not_be_zero() {
        let with = |limit: u64| parse(&format!("{MIN}[limits]\ndefault_output_limit = {limit}\n")).unwrap();
        assert!(check_output_limit(&with(0)).is_err());
        assert!(check_output_limit(&with(1)).is_ok());
    }

    #[test]
    fn the_data_root_must_exist() {
        let rooted = |root: &str| {
            parse(&MIN.replace("problem_data_root = '/d'", &format!("problem_data_root = '{root}'"))).unwrap()
        };
        assert!(check_data_root(&rooted("/nonexistent/qhhoj-data")).is_err());
        assert!(check_data_root(&rooted(&std::env::temp_dir().display().to_string())).is_ok());
    }

    #[test]
    fn remote_bridge_needs_tls_and_https() {
        let remote = MIN.replace("http://127.0.0.1:9990", "http://10.0.0.1:9999");
        assert!(parse(&remote).is_err(), "plaintext to a remote bridge");
        let https = MIN.replace("http://127.0.0.1:9990", "https://bridge.internal:9999");
        assert!(parse(&https).is_err(), "https without a CA");
        let with_ca = format!("{https}[tls]\nca_file = '/ca.pem'\n");
        assert!(parse(&with_ca).is_ok());
    }

    #[test]
    fn control_dir_only_on_loopback() {
        assert!(parse(&format!("control_dir = '/tmp/c'\n{MIN}")).is_ok());
        let remote = MIN.replace("http://127.0.0.1:9990", "https://b:1")
            + "control_dir = '/tmp/c'\n[tls]\nca_file = '/ca'\n";
        assert!(parse(&remote).is_err());
    }

    #[test]
    fn the_unbounded_disk_hatch_only_on_loopback() {
        let hatch = "[isolate]\nallow_unbounded_disk = true\n";
        assert!(parse(&format!("{MIN}{hatch}")).is_ok());
        let remote = MIN.replace("http://127.0.0.1:9990", "https://b:1") + "[tls]\nca_file = '/ca'\n" + hatch;
        assert!(parse(&remote).is_err(), "a production-shaped bridge with the hatch");
    }

    #[test]
    fn loopback_detection() {
        for url in ["http://127.0.0.1:1", "http://localhost:1", "http://[::1]:1"] {
            assert!(is_loopback_url(url), "{url}");
        }
        for url in [
            "http://10.0.0.1:1",
            "http://127.0.0.1.evil.com:1",
            "https://localhost.evil:1",
            "http://localhost:1@evil.com",
            "http://127.0.0.1@evil.com:1",
            "http://user@127.0.0.1:1",
            "ftp://127.0.0.1:1",
        ] {
            assert!(!is_loopback_url(url), "{url}");
        }
    }

    #[test]
    fn key_is_read_from_the_credentials_directory_and_trimmed() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("judge-key"), "secret\n").unwrap();
        let mut c = parse(MIN).unwrap();
        c.key_file = "judge-key".into();
        assert_eq!(c.read_key_in(Some(dir.path())).unwrap(), "secret");
        std::fs::write(dir.path().join("judge-key"), "  \n").unwrap();
        assert!(c.read_key_in(Some(dir.path())).is_err());
    }

    #[test]
    fn a_disk_quota_headroom_too_small_for_the_largest_legal_output_is_refused() {
        let with = |kib: u64, inodes: u32| {
            format!("{MIN}[isolate]\ndisk_quota = {{ headroom_kib = {kib}, headroom_inodes = {inodes} }}\n")
        };
        assert!(parse(MIN).unwrap().isolate.disk_quota.is_none(), "off by default");
        let ok = parse(&with(MIN_HEADROOM_KIB, 1024)).unwrap().isolate.disk_quota.unwrap();
        assert_eq!((u64::from(ok.headroom_kib), ok.headroom_inodes), (MIN_HEADROOM_KIB, 1024));
        assert!(parse(&with(MIN_HEADROOM_KIB - 1, 1024)).is_err());
        assert!(parse(&with(MIN_HEADROOM_KIB, MIN_HEADROOM_INODES - 1)).is_err());
        assert!(parse(&with(1 << 31, 1024)).is_err(), "isolate takes no number above a C int");
        let old = format!("{MIN}[isolate]\ndisk_quota = {{ kib = 524288, inodes = 1024 }}\n");
        assert!(parse(&old).is_err(), "a total quota is not taken for headroom");
    }

    #[test]
    fn the_example_config_parses() {
        let example = include_str!("../../../packaging/config.example.toml");
        parse(example).unwrap();
    }
}
