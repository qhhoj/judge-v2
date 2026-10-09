//! Which registered languages this host can run (a broken runtime is dropped at start, so its
//! submissions get `RETRY RUNTIME_MISSING`), and their versions (`Hello.runtimes`, artefact build ids).

use judge_grader::languages::LanguageRegistry;
use std::collections::HashMap;
use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const VERSION_TIMEOUT: Duration = Duration::from_secs(10);

/// Reads a child's pipe to the end on its own thread, so a chatty command never blocks on a full pipe.
fn drain_pipe(pipe: Option<impl Read + Send + 'static>) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(mut pipe) = pipe {
            let _ = pipe.read_to_end(&mut bytes);
        }
        bytes
    })
}

/// Whether `child` exited successfully within the version timeout (killed otherwise).
fn exited_ok(child: &mut Child) -> bool {
    let deadline = Instant::now() + VERSION_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if Instant::now() <= deadline => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
    }
}

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
    let (stdout, stderr) = (drain_pipe(child.stdout.take()), drain_pipe(child.stderr.take()));
    let ok = exited_ok(&mut child);
    let (stdout, stderr) = (stdout.join().ok()?, stderr.join().ok()?);
    if !ok {
        return None;
    }
    // javac -version and some others print to stderr.
    let text = if stdout.is_empty() { stderr } else { stdout };
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
        [[language]]\nkey='CHATTY'\nname='c'\nsource='a'\nrun=['x']\nversion=['/bin/sh','-c','head -c 300000 /dev/zero >&2; echo 9.1']\n\
        [[language]]\nkey='TEXT'\nname='t'\nsource='a'\nkind='text'\nrun=[]\nversion=[]\n";

    #[test]
    fn versions_are_first_lines_and_missing_runtimes_drop_out() {
        let registry = LanguageRegistry::from_toml(LANGS).unwrap();
        let versions = detect_versions(&registry);
        assert_eq!(versions.get("SH").map(String::as_str), Some("5.2"));
        assert_eq!(versions.get("TEXT").map(String::as_str), Some("builtin"));
        assert!(!versions.contains_key("NOPE"), "missing binary");
        assert!(!versions.contains_key("FAILS"), "non-zero exit");
        assert_eq!(
            versions.get("CHATTY").map(String::as_str),
            Some("9.1"),
            "a full stderr pipe does not block"
        );
        let kept = available(registry, &versions);
        assert_eq!(kept.keys(), ["CHATTY", "SH", "TEXT"]);
    }
}
