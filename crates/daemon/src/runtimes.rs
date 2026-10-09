//! Which registered languages this host can run (a broken runtime is dropped at start, so its
//! submissions get `RETRY RUNTIME_MISSING`), and their versions (`Hello.runtimes`, artefact build ids).

use judge_grader::languages::LanguageRegistry;
use std::collections::HashMap;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const VERSION_TIMEOUT: Duration = Duration::from_secs(10);

/// The first non-empty output line of a successful `argv`, run outside the sandbox (10 s at most).
fn first_line(argv: &[String]) -> Option<String> {
    let (program, args) = argv.split_first()?;
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + VERSION_TIMEOUT;
    while child.try_wait().ok()?.is_none() {
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let output = child.wait_with_output().ok()?;
    if !output.status.success() {
        return None;
    }
    // javac -version and some others print to stderr.
    let text = if output.stdout.is_empty() { output.stderr } else { output.stdout };
    String::from_utf8_lossy(&text).lines().map(str::trim).find(|l| !l.is_empty()).map(str::to_string)
}

/// Version per language key; an empty `version` command is `builtin`; a failing one leaves the key out.
pub fn detect_versions(registry: &LanguageRegistry) -> HashMap<String, String> {
    let detect = |key: &str| {
        let language = registry.get(key)?;
        let version = if language.version.is_empty() {
            Some("builtin".to_string())
        } else {
            first_line(&language.version)
        };
        version.map(|v| (key.to_string(), v))
    };
    registry.keys().into_iter().filter_map(detect).collect()
}

/// Drops (and logs) every language without a detected version.
pub fn available(mut registry: LanguageRegistry, versions: &HashMap<String, String>) -> LanguageRegistry {
    for key in registry.keys().into_iter().filter(|key| !versions.contains_key(*key)) {
        tracing::warn!(lang = key, "runtime not found; language not advertised");
    }
    registry.retain(|language| versions.contains_key(&language.key));
    registry
}

#[cfg(test)]
mod tests {
    use super::*;

    const LANGS: &str = "[[language]]\nkey='SH'\nname='sh'\nsource='a'\nrun=['x']\nversion=['/bin/sh','-c','echo; echo 5.2; echo extra']\n\
        [[language]]\nkey='FAILS'\nname='f'\nsource='a'\nrun=['x']\nversion=['/bin/sh','-c','echo 1.0; exit 3']\n\
        [[language]]\nkey='NOPE'\nname='n'\nsource='a'\nrun=['x']\nversion=['/nonexistent/bin']\n\
        [[language]]\nkey='TEXT'\nname='t'\nsource='a'\nkind='text'\nrun=[]\nversion=[]\n";

    #[test]
    fn versions_are_first_lines_and_missing_runtimes_drop_out() {
        let registry = LanguageRegistry::from_toml(LANGS).unwrap();
        let versions = detect_versions(&registry);
        assert_eq!(versions.get("SH").map(String::as_str), Some("5.2"));
        assert_eq!(versions.get("TEXT").map(String::as_str), Some("builtin"));
        assert!(!versions.contains_key("NOPE"), "missing binary");
        assert!(!versions.contains_key("FAILS"), "non-zero exit");
        let kept = available(registry, &versions);
        assert_eq!(kept.keys(), ["SH", "TEXT"]);
    }
}
