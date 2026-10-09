//! `/etc/qhhoj-judge/config.toml` (spec §7 "Config").

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
}

impl Default for LimitDefaults {
    fn default() -> Self {
        LimitDefaults {
            default_output_limit: 25_165_824,
            default_output_prefix: 64,
            jvm_reserve_kib: 131_072,
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

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct IsolatePaths {
    pub bin: PathBuf,
    pub taskset: PathBuf,
}

impl Default for IsolatePaths {
    fn default() -> Self {
        IsolatePaths { bin: "/usr/local/bin/isolate".into(), taskset: "/usr/bin/taskset".into() }
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
    /// Dev and e2e only: hold files `hold-ack`, `hold-begin`, `hold-end` (Review Focus 6).
    #[serde(default)]
    pub control_dir: Option<PathBuf>,
    #[serde(default)]
    pub limits: LimitDefaults,
    #[serde(default)]
    pub reconnect: Reconnect,
    #[serde(default)]
    pub isolate: IsolatePaths,
}

/// Parsed with `http::Uri`; a URL with userinfo (`http://localhost:1@evil.com`) is never loopback (preflight M8).
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
    Ok(())
}

pub fn parse(text: &str) -> Result<Config, ConfigError> {
    let config: Config = toml::from_str(text).map_err(|e| invalid(e.to_string()))?;
    validate(&config)?;
    Ok(config)
}

pub fn load(path: &Path) -> Result<Config, ConfigError> {
    parse(&std::fs::read_to_string(path)?)
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
    fn minimal_loopback_config_gets_defaults() {
        let c = parse(MIN).unwrap();
        assert_eq!(
            (c.cache_max_bytes, c.box_base, c.drain_timeout_s, c.data_wait_ms),
            (20 << 30, 0, 120, 10_000)
        );
        assert_eq!(c.limits.default_output_limit, 25_165_824);
        assert_eq!(c.limits.jvm_reserve_kib, 131_072);
        assert_eq!((c.reconnect.initial_ms, c.reconnect.max_ms), (500, 30_000));
        assert_eq!(c.metrics_listen, "127.0.0.1:9464");
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
    fn unknown_keys_are_refused() {
        assert!(parse(&format!("{MIN}slot = 3\n")).is_err());
    }

    #[test]
    fn the_example_config_parses() {
        let example = include_str!("../../../packaging/config.example.toml");
        let config = parse(example).unwrap();
        assert_eq!(config.tls.unwrap().server_name.as_deref(), Some("bridge.qhhoj.internal"));
    }
}
