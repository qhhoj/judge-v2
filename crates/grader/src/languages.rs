//! Languages are data (`languages.toml`); this module only loads and expands them.

use crate::error::GradeError;
use regex::Regex;
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum LanguageKind {
    #[default]
    Program,
    Text,
    OutputZip,
}

/// Where in stderr the name of an uncaught exception is found (see `run_case::exception_reason`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ExceptionStyle {
    /// Text after the marker names it: `Exception in thread "main" java.lang.X: msg`.
    #[default]
    After,
    /// The marker opens a traceback and the last stderr line names it (Python).
    LastLine,
    /// The marker opens a traceback and the line above holds the message (Lua).
    LineAbove,
    /// The marker line ends with the name in parentheses: `main.rb:1:in '<main>': msg (ZeroDivisionError)`.
    Parenthesized,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct CompileLimitOverride {
    pub time_s: f64,
    pub wall_s: f64,
    pub memory_kib: u64,
}

fn one() -> u32 {
    1
}

fn thirty_two() -> u32 {
    32
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Language {
    pub key: String,
    pub name: String,
    pub source: String,
    #[serde(default)]
    pub compile: Option<Vec<String>>,
    #[serde(default)]
    pub compile_limits: Option<CompileLimitOverride>,
    pub run: Vec<String>,
    #[serde(default = "one")]
    pub processes: u32,
    #[serde(default = "thirty_two")]
    pub compile_processes: u32,
    #[serde(default)]
    pub jvm: bool,
    #[serde(default)]
    pub file_only: bool,
    #[serde(default)]
    pub kind: LanguageKind,
    #[serde(default)]
    pub dirs: Vec<String>,
    #[serde(default)]
    pub compile_dirs: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    pub version: Vec<String>,
    /// Compiler and flags the signature grader uses (C and C++ only).
    #[serde(default)]
    pub signature: Option<Vec<String>>,
    /// Flag that puts `/testlib` on the include path for checkers and interactors.
    #[serde(default)]
    pub testlib_flag: Option<String>,
    /// stderr text of an uncaught exception: a non-zero exit carrying it is RTE, not IR (spec §11).
    #[serde(default)]
    pub exception_marker: Option<String>,
    #[serde(default)]
    pub exception_style: ExceptionStyle,
}

#[derive(Deserialize)]
struct LanguageFile {
    language: Vec<Language>,
}

#[derive(Debug, Clone, Default)]
pub struct LanguageRegistry {
    by_key: BTreeMap<String, Language>,
}

impl LanguageRegistry {
    pub fn from_toml(text: &str) -> Result<Self, GradeError> {
        let file: LanguageFile =
            toml::from_str(text).map_err(|e| GradeError::Io(format!("languages.toml: {e}")))?;
        let mut by_key = BTreeMap::new();
        for language in file.language {
            if language.kind == LanguageKind::Program && language.run.is_empty() {
                return Err(GradeError::Io(format!("language {} has no run command", language.key)));
            }
            let key = language.key.clone();
            if by_key.insert(key.clone(), language).is_some() {
                return Err(GradeError::Io(format!("duplicate language key {key}")));
            }
        }
        Ok(LanguageRegistry { by_key })
    }

    pub fn get(&self, key: &str) -> Option<&Language> {
        self.by_key.get(key)
    }

    pub fn keys(&self) -> Vec<&str> {
        self.by_key.keys().map(String::as_str).collect()
    }

    pub fn retain(&mut self, keep: impl Fn(&Language) -> bool) {
        self.by_key.retain(|_, language| keep(language));
    }

    /// Every absolute `argv[0]` the registry runs (compile, run, version, signature).
    pub fn programs(&self) -> BTreeSet<String> {
        let argvs = self.by_key.values().flat_map(|l| {
            [l.compile.as_ref(), Some(&l.run), Some(&l.version), l.signature.as_ref()].into_iter().flatten()
        });
        argvs.filter_map(|argv| argv.first()).filter(|p| p.starts_with('/')).cloned().collect()
    }
}

#[derive(Debug, Clone, Default)]
pub struct Placeholders {
    pub memory_kib: u64,
    pub class: Option<String>,
}

pub fn expand(argv: &[String], values: &Placeholders) -> Vec<String> {
    let class = values.class.as_deref().unwrap_or("Main");
    argv.iter()
        .map(|arg| {
            arg.replace("{memory_kib}", &values.memory_kib.to_string())
                .replace("{memory_mib}", &(values.memory_kib / 1024).to_string())
                .replace("{class}", class)
        })
        .collect()
}

static PUBLIC_CLASS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"public\s+(?:final\s+)?class\s+([A-Za-z_][A-Za-z0-9_]*)").expect("static pattern")
});
static MAIN_CLASS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?s)class\s+([A-Za-z_][A-Za-z0-9_]*)\s*\{[^{}]*static\s+void\s+main")
        .expect("static pattern")
});
static COMMENTS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?s)/\*.*?\*/|//[^\n]*").expect("static pattern"));

/// The class a Java source declares: the first `public class`, else the first `class` with `main`.
/// Comments are removed first, so a commented-out declaration is not picked.
pub fn java_main_class(source: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(source);
    let code = COMMENTS.replace_all(&text, " ");
    [&PUBLIC_CLASS, &MAIN_CLASS].into_iter().find_map(|re| re.captures(&code).map(|c| c[1].to_string()))
}

impl Language {
    pub fn source_name(&self, source: &[u8]) -> String {
        if !self.source.contains("{class}") {
            return self.source.clone();
        }
        let class = java_main_class(source).unwrap_or_else(|| "Main".into());
        self.source.replace("{class}", &class)
    }
}

/// isolate 2.6's default mounts (no `/etc`): a program reached through anything else fails in the box.
const BOX_MOUNTS: [&str; 4] = ["/usr", "/bin", "/lib", "/lib64"];

fn in_box_mounts(path: &Path) -> bool {
    BOX_MOUNTS.iter().any(|mount| path.starts_with(mount))
}

/// Follows `program`'s symlink chain; `Some(real path)` when a hop leaves the box mounts (preflight I18).
pub fn host_target(program: &Path) -> Option<PathBuf> {
    let mut path = program.to_path_buf();
    let mut left_box = false;
    for _ in 0..16 {
        let Ok(next) = std::fs::read_link(&path) else { break };
        path = if next.is_absolute() { next } else { path.parent()?.join(next) };
        left_box |= !in_box_mounts(&path);
    }
    left_box.then(|| std::fs::canonicalize(&path).unwrap_or(path))
}

/// The host copy of `languages.toml`: every unreachable `argv[0]` replaced by its real path.
pub fn host_languages(text: &str, registry: &LanguageRegistry) -> (String, Vec<(String, String)>) {
    let rewrites: Vec<(String, String)> = registry
        .programs()
        .into_iter()
        .filter_map(|program| {
            host_target(Path::new(&program)).map(|real| (program, real.display().to_string()))
        })
        .collect();
    let mut host = text.to_string();
    for (from, to) in &rewrites {
        host = host.replace(&format!("\"{from}\""), &format!("\"{to}\""));
    }
    (host, rewrites)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHIPPED: &str = include_str!("../../../languages.toml");
    const MINIMAL: &str = "[[language]]\nkey='A'\nname='a'\nsource='a'\nrun=['x']\nversion=[]\n";

    fn shipped() -> LanguageRegistry {
        LanguageRegistry::from_toml(SHIPPED).unwrap()
    }

    #[test]
    fn shipped_registry_has_the_measured_day_one_set() {
        let registry = shipped();
        for key in [
            "C", "C11", "CLANG", "CPP03", "CPP11", "CPP14", "CPP17", "CPP20", "CPP23", "CLANGX", "PY3",
            "PYPY3", "PAS", "JAVA", "JAVA8", "KOTLIN", "RUBY", "D", "LUA", "ADA", "BF", "GAS32", "GAS64",
            "TEXT", "OUTPUT",
        ] {
            assert!(registry.get(key).is_some(), "{key} missing");
        }
        assert!(registry.get("PY2").is_none());
        assert!(registry.get("SCRATCH").is_none(), "SCRATCH is dropped");
    }

    #[test]
    fn python_and_java_uncaught_exceptions_can_be_recognised() {
        let registry = shipped();
        for key in ["PY3", "PYPY3", "JAVA", "JAVA8", "KOTLIN"] {
            assert!(registry.get(key).unwrap().exception_marker.is_some(), "{key} has no exception marker");
        }
        assert!(registry.get("RUBY").unwrap().exception_marker.is_some());
        assert!(registry.get("CPP17").unwrap().exception_marker.is_none());
    }

    #[test]
    fn duplicate_keys_and_empty_program_runs_are_refused() {
        assert!(LanguageRegistry::from_toml(&MINIMAL.repeat(2)).is_err());
        let no_run = "[[language]]\nkey='A'\nname='a'\nsource='a'\nrun=[]\nversion=[]\n";
        assert!(LanguageRegistry::from_toml(no_run).is_err());
        assert!(LanguageRegistry::from_toml(&format!("{MINIMAL}colour='red'\n")).is_err(), "unknown key");
    }

    #[test]
    fn defaults_apply() {
        let r = LanguageRegistry::from_toml(MINIMAL).unwrap();
        let a = r.get("A").unwrap();
        let shape = (a.processes, a.compile_processes, a.jvm, a.file_only, a.kind);
        assert_eq!(shape, (1, 32, false, false, LanguageKind::Program));
    }

    #[test]
    fn retain_drops_languages() {
        let mut r = shipped();
        r.retain(|l| l.key != "PAS");
        assert!(r.get("PAS").is_none() && r.get("C").is_some());
        assert!(!r.keys().contains(&"PAS"));
    }

    #[test]
    fn placeholders_expand() {
        let argv: Vec<String> =
            ["java", "-Xmx{memory_mib}m", "{class}", "{memory_kib}"].map(String::from).to_vec();
        let out = expand(&argv, &Placeholders { memory_kib: 262_144, class: Some("Sol".into()) });
        assert_eq!(out, ["java", "-Xmx256m", "Sol", "262144"]);
    }

    #[test]
    fn java_class_detection() {
        let public = b"import x;\npublic class Solution {\n public static void main(String[] a){}}";
        assert_eq!(java_main_class(public).as_deref(), Some("Solution"));
        let package_private = b"class A {}\nclass B { public static void main(String[] a){} }";
        assert_eq!(java_main_class(package_private).as_deref(), Some("B"));
        assert_eq!(java_main_class(b"int x;"), None);
        let commented = b"// public class Old {}\n/* public class Older {} */\npublic class Real {}";
        assert_eq!(java_main_class(commented).as_deref(), Some("Real"));
        let r = shipped();
        assert_eq!(r.get("JAVA").unwrap().source_name(b"public class Q { }"), "Q.java");
        assert_eq!(r.get("JAVA").unwrap().source_name(b"nothing"), "Main.java");
        assert_eq!(r.get("CPP17").unwrap().source_name(b"int main(){}"), "main.cpp");
    }

    #[test]
    fn shipped_runtimes_never_go_through_etc() {
        for program in shipped().programs() {
            assert!(!program.starts_with("/etc"), "{program}");
        }
        assert!(!SHIPPED.contains("/etc/fpc.cfg"), "isolate does not mount /etc");
    }

    #[test]
    fn host_target_flags_chains_that_leave_the_box_mounts() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("alt/java-real");
        std::fs::create_dir_all(real.parent().unwrap()).unwrap();
        std::fs::write(&real, "").unwrap();
        let link = dir.path().join("java");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert_eq!(host_target(&link), Some(std::fs::canonicalize(&real).unwrap()));
        let inside = dir.path().join("env");
        std::os::unix::fs::symlink("/usr/bin/env", &inside).unwrap();
        assert_eq!(host_target(&inside), None, "a chain that ends inside /usr is reachable in the box");
        assert_eq!(host_target(&real), None, "a regular file is not a chain");
    }

    #[test]
    fn host_languages_rewrites_only_unreachable_programs() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("alt/ruby-real");
        std::fs::create_dir_all(real.parent().unwrap()).unwrap();
        std::fs::write(&real, "").unwrap();
        let link = dir.path().join("ruby");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let text = format!(
            "[[language]]\nkey='RUBY'\nname='r'\nsource='main.rb'\nrun=[\"{l}\", \"main.rb\"]\nversion=[\"{l}\", \"-v\"]\n\
             [[language]]\nkey='C'\nname='c'\nsource='main.c'\nrun=[\"./main\"]\nversion=[\"/usr/bin/env\"]\n",
            l = link.display()
        );
        let registry = LanguageRegistry::from_toml(&text).unwrap();
        let (host, rewrites) = host_languages(&text, &registry);
        let real = std::fs::canonicalize(&real).unwrap().display().to_string();
        assert_eq!(rewrites, vec![(link.display().to_string(), real.clone())]);
        let rewritten = LanguageRegistry::from_toml(&host).unwrap();
        assert_eq!(rewritten.get("RUBY").unwrap().run[0], real);
        assert_eq!(rewritten.get("RUBY").unwrap().version[0], real);
        assert_eq!(rewritten.get("C").unwrap().version[0], "/usr/bin/env");
    }
}
