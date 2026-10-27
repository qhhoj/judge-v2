//! Which registered languages this host can run (a broken runtime is dropped at start, so its
//! submissions get `RETRY RUNTIME_MISSING`), and their versions (`Hello.runtimes`, artefact build ids).

use judge_grader::languages::{Language, LanguageRegistry};
use std::collections::HashMap;
use std::io::Read;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

const VERSION_TIMEOUT: Duration = Duration::from_secs(10);

/// Most version output kept per pipe; a version line is far shorter.
const MAX_VERSION_OUTPUT: u64 = 64 << 10;
/// How long to wait for a pipe to close after the child has exited. A background grandchild that
/// inherited the pipe would otherwise hold startup forever.
const PIPE_GRACE: Duration = Duration::from_secs(2);

/// Reads a child's pipe on its own thread, so a chatty command never blocks on a full pipe. The thread
/// is left behind (not joined) if the pipe never closes.
fn drain_pipe(pipe: Option<impl Read + Send + 'static>) -> mpsc::Receiver<Vec<u8>> {
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(pipe) = pipe {
            let _ = pipe.take(MAX_VERSION_OUTPUT).read_to_end(&mut bytes);
        }
        let _ = sender.send(bytes);
    });
    receiver
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
    let (stdout, stderr) = (stdout.recv_timeout(PIPE_GRACE).ok()?, stderr.recv_timeout(PIPE_GRACE).ok()?);
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

/// The first directory the language binds into the box that is not on this host (isolate would refuse it).
fn missing_dir(language: &Language) -> Option<&str> {
    let mut dirs = language.dirs.iter().chain(&language.compile_dirs).map(String::as_str);
    dirs.find(|dir| !Path::new(dir).is_dir())
}

/// The installed languages (run command on this host) that preload but none of whose `preload` paths exists
/// (or whose wildcards matched no file at all):
/// their runs are charged for the page cache of the runtime they are first to read (`judge_sandbox::preload`).
/// A language may list several layouts (PYPY3: portable and apt); one present is enough.
pub fn unpreloaded(registry: &LanguageRegistry) -> Vec<&Language> {
    let installed =
        |language: &&Language| language.run.first().is_some_and(|program| Path::new(program).exists());
    let none_present = |language: &&Language| {
        language.preload_matched_nothing
            || (!language.preload.is_empty() && !language.preload.iter().any(|path| Path::new(path).exists()))
    };
    let languages = registry.keys().into_iter().filter_map(|key| registry.get(key));
    languages.filter(installed).filter(none_present).collect()
}

/// Whether the host can run `language`: a detected version and every bound directory present (logged otherwise).
fn runnable(language: &Language, versions: &HashMap<String, String>) -> bool {
    let lang = language.key.as_str();
    if !versions.contains_key(lang) {
        tracing::warn!(lang, "runtime not found; language not advertised");
        return false;
    }
    if let Some(dir) = missing_dir(language) {
        tracing::warn!(lang, dir, "directory to bind is missing; language not advertised");
        return false;
    }
    true
}

/// Drops (and logs) every language without a detected version or with a missing bound directory.
pub fn available(mut registry: LanguageRegistry, versions: &HashMap<String, String>) -> LanguageRegistry {
    registry.retain(|language| runnable(language, versions));
    for language in unpreloaded(&registry) {
        let (lang, paths) = (language.key.as_str(), language.preload.join(" "));
        tracing::warn!(lang, paths, "no preload path exists; runs may be charged the runtime's page cache");
    }
    registry
}

#[cfg(test)]
mod tests {
    use super::*;

    const LANGS: &str = "[[language]]\nkey='SH'\nname='sh'\nsource='a'\nrun=['x']\nversion=['/bin/sh','-c','echo; echo 5.2; echo extra']\n\
        [[language]]\nkey='FAILS'\nname='f'\nsource='a'\nrun=['x']\nversion=['/bin/sh','-c','echo 1.0; exit 3']\n\
        [[language]]\nkey='NOPE'\nname='n'\nsource='a'\nrun=['x']\nversion=['/nonexistent/bin']\n\
        [[language]]\nkey='CHATTY'\nname='c'\nsource='a'\nrun=['x']\nversion=['/bin/sh','-c','head -c 300000 /dev/zero >&2; echo 9.1']\n\
        [[language]]\nkey='TEXT'\nname='t'\nsource='a'\nkind='text'\nrun=[]\nversion=[]\n\
        [[language]]\nkey='BOUND'\nname='b'\nsource='a'\nrun=['x']\nversion=['/bin/sh','-c','echo 1']\ndirs=['/usr']\ncompile_dirs=['/bin']\n\
        [[language]]\nkey='NO_DIR'\nname='d'\nsource='a'\nrun=['x']\nversion=['/bin/sh','-c','echo 1']\ndirs=['/nonexistent/etc']\n\
        [[language]]\nkey='NO_COMPILE_DIR'\nname='e'\nsource='a'\nrun=['x']\nversion=['/bin/sh','-c','echo 1']\ncompile_dirs=['/nonexistent/etc']\n";

    #[test]
    fn an_installed_language_none_of_whose_preload_paths_exists_is_reported() {
        let text = "[[language]]\nkey='SH'\nname='sh'\nsource='a'\nrun=['/bin/sh']\nversion=[]\npreload=['/nonexistent/lib']\n\
            [[language]]\nkey='WILD'\nname='w'\nsource='a'\nrun=['/bin/sh']\nversion=[]\npreload=['/nonexistent/lib*']\n\
            [[language]]\nkey='ONE'\nname='o'\nsource='a'\nrun=['/bin/sh']\nversion=[]\npreload=['/nonexistent/lib', '/bin']\n\
            [[language]]\nkey='GONE'\nname='g'\nsource='a'\nrun=['/nonexistent/bin']\nversion=[]\npreload=['/nonexistent/lib']\n\
            [[language]]\nkey='GONEWILD'\nname='gw'\nsource='a'\nrun=['/nonexistent/bin']\nversion=[]\npreload=['/nonexistent/lib*']\n";
        let registry = LanguageRegistry::from_toml(text).unwrap();
        let keys: Vec<_> = unpreloaded(&registry).iter().map(|language| language.key.as_str()).collect();
        assert_eq!(
            keys,
            ["SH", "WILD"],
            "one present layout is enough; a language not installed is not reported"
        );
    }

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
        assert_eq!(
            kept.keys(),
            ["BOUND", "CHATTY", "SH", "TEXT"],
            "a missing bound directory drops the language"
        );
    }
}
