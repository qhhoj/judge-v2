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
    /// CPU seconds.
    pub time_s: f64,
    /// Wall-clock seconds.
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
    /// The language key the bridge sends in `Assign.language_key` (`CPP17`, `PY3`).
    pub key: String,
    /// Display name, reported in `Hello.runtimes`.
    pub name: String,
    /// File name the source is written to in the box; `{class}` is the Java public class.
    pub source: String,
    /// Compile command, run in the solution box with cwd `/box`. None: nothing to compile.
    #[serde(default)]
    pub compile: Option<Vec<String>>,
    /// Replaces the default compile limits (CPU 10 s, wall 20 s, 1 GiB) for this language.
    #[serde(default)]
    pub compile_limits: Option<CompileLimitOverride>,
    /// Run command, one run per case.
    pub run: Vec<String>,
    /// Most processes (threads included) a run may start; 64 for the JVM, whose runtime starts helper threads.
    #[serde(default = "one")]
    pub processes: u32,
    /// Most processes the compile step may start.
    #[serde(default = "thirty_two")]
    pub compile_processes: u32,
    /// A JVM language: its cgroup gets `jvm_reserve_kib` above the memory limit, its stderr is kept so an
    /// `OutOfMemoryError` becomes MLE, and a checker written in it keeps a 128 MiB reserve outside its heap.
    #[serde(default)]
    pub jvm: bool,
    /// The submission is a file (a zip) fetched with `FetchSource`, not source text sent in `Assign`.
    #[serde(default)]
    pub file_only: bool,
    /// How cases are graded: a compiled/interpreted `program` (default), `text` (the source is every case's
    /// output) or `output_zip` (a zip of per-case outputs).
    #[serde(default)]
    pub kind: LanguageKind,
    /// Extra read-only directories mounted into every run (`--dir`): runtime trees such as the JDK or PyPy.
    #[serde(default)]
    pub dirs: Vec<String>,
    /// Extra read-only directories mounted into the compile step only.
    #[serde(default)]
    pub compile_dirs: Vec<String>,
    /// Runtime files and directories the judge reads into the page cache before each compile and run, so the
    /// kernel charges their pages to the judge and not to the box (`judge_sandbox::preload`).
    #[serde(default)]
    pub preload: Vec<String>,
    /// Set at load: `preload` had entries for this host's layout and none of them expanded to a file.
    #[serde(skip)]
    pub preload_matched_nothing: bool,
    /// Environment variables set for runs, on top of `PATH` and `HOME`.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Command whose first output line is the compiler or runtime version, reported in `Hello.runtimes`.
    pub version: Vec<String>,
    /// Compiler and flags the signature grader uses (C and C++ only).
    #[serde(default)]
    pub signature: Option<Vec<String>>,
    /// Flag that puts `/testlib` on the include path for checkers and interactors.
    #[serde(default)]
    pub testlib_flag: Option<String>,
    /// stderr text of an uncaught exception: a non-zero exit carrying it is RTE, not IR (docs/grading.md#uncaught-exceptions).
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

/// Placeholder in `dirs` and `compile_dirs` for the installed default JDK's `/etc` directory.
pub const JDK_ETC_PLACEHOLDER: &str = "{jdk_etc}";
/// The distro's default-jdk link (Debian/Ubuntu): its target names the JDK major.
pub const DEFAULT_JAVA: &str = "/usr/lib/jvm/default-java";

/// Placeholder in `preload` for the runtime's install directory: the one above the directory that holds the
/// real file of the run command (`runtime_home`).
pub const RUNTIME_PLACEHOLDER: &str = "{runtime}";

static HOST_JDK_ETC: LazyLock<Option<String>> = LazyLock::new(|| jdk_etc_dir(Path::new(DEFAULT_JAVA)));

/// `/etc/java-NN-openjdk` for the JDK `default_java` resolves to (`java-NN-openjdk-<arch>`): where the
/// JDK's conf/ and lib/security/ symlinks point. None when the link is missing or names no OpenJDK.
pub fn jdk_etc_dir(default_java: &Path) -> Option<String> {
    let jdk = std::fs::canonicalize(default_java).ok()?;
    let name = jdk.file_name()?.to_str()?;
    let mut parts = name.split('-');
    let (java, major, openjdk) = (parts.next()?, parts.next()?, parts.next()?);
    let is_major = !major.is_empty() && major.bytes().all(|b| b.is_ascii_digit());
    (java == "java" && is_major && openjdk == "openjdk").then(|| format!("/etc/java-{major}-openjdk"))
}

/// The directory above the one holding the real file `program` resolves to: `/usr/lib/jvm/java-NN-openjdk-<arch>`
/// for the JDK's `bin/java`, `/opt/pypy` for a portable PyPy's `bin/pypy3.9`. None when `program` is missing or
/// that directory is a system prefix (a runtime installed straight into `/usr/bin`) or one level below `/` (`/opt`,
/// `/srv`, `/home` for a binary in `/opt/bin`): never preload a whole tree of the host.
pub fn runtime_home(program: &str) -> Option<String> {
    install_home(&std::fs::canonicalize(program).ok()?)
}

/// The directory above `real_program`'s, unless it is a system prefix or has fewer than two components.
fn install_home(real_program: &Path) -> Option<String> {
    let home = real_program.parent()?.parent()?;
    let shallow = home.components().count() < 3; // the root and two names: `/opt/pypy`
    let system = shallow || home == Path::new("/usr/local");
    (!system).then(|| home.to_string_lossy().into_owned())
}

/// `path` itself, or, when its last component holds `*` between literal text (`libruby*.so*`), the sorted
/// entries of its directory whose name matches (each `*` is any text). A component with no literal text (`*`,
/// a `*` after a `/`) is kept as written, to be refused.
pub fn expand_wildcard(path: &str) -> Vec<String> {
    let Some((dir, pattern)) = path.rsplit_once('/').filter(|(_, pattern)| pattern.contains('*')) else {
        return vec![path.to_string()];
    };
    if pattern.chars().all(|c| c == '*') {
        return vec![path.to_string()];
    }
    let mut found: Vec<String> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| matches_wildcard(pattern, &entry.file_name().to_string_lossy()))
        .map(|entry| entry.path().to_string_lossy().into_owned())
        .collect();
    found.sort();
    found
}

/// Whether `name` matches `pattern`, where each `*` stands for any text (possibly none).
fn matches_wildcard(pattern: &str, name: &str) -> bool {
    match pattern.split_once('*') {
        None => pattern == name,
        Some((head, rest)) => name.strip_prefix(head).is_some_and(|tail| {
            (0..=tail.len())
                .filter(|&at| tail.is_char_boundary(at))
                .any(|at| matches_wildcard(rest, &tail[at..]))
        }),
    }
}

impl Language {
    /// Replaces `{runtime}` in `preload` with `runtime_home` of the run command; without one, the entries that
    /// name it are dropped (that layout is not this host's) and the absolute ones stay. An entry whose last
    /// component holds `*` (`libruby*.so*`, so no version is written) becomes the entries of its directory
    /// that match it; one matching nothing is dropped.
    pub fn resolve_runtime(&mut self) {
        match self.run.first().and_then(|program| runtime_home(program)) {
            Some(home) => {
                self.preload.iter_mut().for_each(|path| *path = path.replace(RUNTIME_PLACEHOLDER, &home))
            }
            None => self.preload.retain(|path| !path.contains(RUNTIME_PLACEHOLDER)),
        }
        let written = self.preload.len();
        self.preload = self.preload.iter().flat_map(|path| expand_wildcard(path)).collect();
        self.preload_matched_nothing = written > 0 && self.preload.is_empty();
    }

    /// The first `preload` path that is not absolute, is a whole system tree (`/`, `/usr`, `/usr/local`),
    /// climbs with `..` (`/usr/lib/..` is `/usr`) or still holds a `*` (a wildcard with no literal text,
    /// `/usr/lib/*`, or one before the last component).
    fn refused_preload(&self) -> Option<&str> {
        let system = |path: &Path| ["/", "/usr", "/usr/local"].iter().any(|root| path == Path::new(root));
        let climbs = |path: &Path| path.components().any(|part| part == std::path::Component::ParentDir);
        let refused = |path: &&String| {
            let path_ref = Path::new(path);
            !path_ref.is_absolute() || system(path_ref) || climbs(path_ref) || path.contains('*')
        };
        self.preload.iter().find(refused).map(String::as_str)
    }

    /// Replaces `{jdk_etc}` in the bound directories; without a JDK it stays, and the daemon drops the
    /// language because that directory does not exist.
    fn resolve_dirs(&mut self, jdk_etc: Option<&str>) {
        let Some(jdk_etc) = jdk_etc else { return };
        for dir in self.dirs.iter_mut().chain(self.compile_dirs.iter_mut()) {
            *dir = dir.replace(JDK_ETC_PLACEHOLDER, jdk_etc);
        }
    }
}

impl LanguageRegistry {
    /// Parses `languages.toml`, resolving `{jdk_etc}` against this host's default JDK.
    pub fn from_toml(text: &str) -> Result<Self, GradeError> {
        Self::from_toml_with(text, HOST_JDK_ETC.as_deref())
    }

    fn from_toml_with(text: &str, jdk_etc: Option<&str>) -> Result<Self, GradeError> {
        let file: LanguageFile =
            toml::from_str(text).map_err(|e| GradeError::Io(format!("languages.toml: {e}")))?;
        let mut by_key = BTreeMap::new();
        for mut language in file.language {
            language.resolve_dirs(jdk_etc);
            language.resolve_runtime();
            if let Some(path) = language.refused_preload() {
                return Err(GradeError::Io(format!(
                    "language {}: preload {path:?} is not a runtime",
                    language.key
                )));
            }
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

/// Comments, text blocks, strings and char literals: blanked before scanning, so their braces and words never count.
static NOT_CODE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?s)/\*.*?\*/|//[^\n]*|""".*?"""|"(?:\\.|[^"\\\n])*"|'(?:\\.|[^'\\\n])*'"#)
        .expect("static pattern")
});
static TYPE_DECL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b((?:(?:public|final|abstract|strictfp|sealed)\s+)*)(?:class|interface|enum|record)\s+([\p{L}_$][\p{L}\p{N}_$]*)")
        .expect("static pattern")
});
static MAIN_METHOD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\bstatic\s+(?:(?:public|final|synchronized|strictfp)\s+)*void\s+main\s*\(")
        .expect("static pattern")
});

struct TopLevelType {
    at: usize,
    public: bool,
    name: String,
}

/// `depths[i]`: how many braces are open before byte `i`.
fn brace_depths(code: &str) -> Vec<i64> {
    let mut depth = 0;
    let mut depths = Vec::with_capacity(code.len() + 1);
    for byte in code.bytes() {
        depths.push(depth);
        depth += match byte {
            b'{' => 1,
            b'}' => -1,
            _ => 0,
        };
    }
    depths.push(depth);
    depths
}

/// The types declared outside every brace, in source order.
fn top_level_types(code: &str, depths: &[i64]) -> Vec<TopLevelType> {
    TYPE_DECL
        .captures_iter(code)
        .filter_map(|c| {
            let at = c.get(0)?.start();
            (depths[at] == 0).then(|| TopLevelType {
                at,
                public: c[1].contains("public"),
                name: c[2].to_string(),
            })
        })
        .collect()
}

/// The class a Java source declares: the top-level `public` type, else the top-level type whose body
/// declares `static void main`. Comments and literals are blanked first, so a commented-out declaration
/// is not picked; members of nested types are skipped by brace depth.
pub fn java_main_class(source: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(source);
    let code = NOT_CODE.replace_all(&text, " ");
    let depths = brace_depths(&code);
    let types = top_level_types(&code, &depths);
    if let Some(public) = types.iter().find(|t| t.public) {
        return Some(public.name.clone());
    }
    let mut mains = MAIN_METHOD.find_iter(&code).map(|m| m.start()).filter(|&at| depths[at] == 1);
    mains.find_map(|at| types.iter().rev().find(|t| t.at < at).map(|t| t.name.clone()))
}

static PACKAGE_DECL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\bpackage\s+[\p{L}_$][\p{L}\p{N}_$.\s]*;").expect("static pattern"));
/// Linux NAME_MAX: the longest file name the box can hold, in bytes.
const MAX_FILE_NAME_BYTES: usize = 255;
const PACKAGE_REFUSAL: &str = "the main class must be in the default package: remove the `package` line";
const LONG_NAME_REFUSAL: &str = "the class name is too long for its source file name (255 bytes at most)";

/// True when the source declares a package outside every brace (comments and literals blanked first).
fn declares_package(source: &[u8]) -> bool {
    let text = String::from_utf8_lossy(source);
    let code = NOT_CODE.replace_all(&text, " ");
    let depths = brace_depths(&code);
    PACKAGE_DECL.find_iter(&code).any(|m| depths[m.start()] == 0)
}

impl Language {
    pub fn source_name(&self, source: &[u8]) -> String {
        if !self.source.contains("{class}") {
            return self.source.clone();
        }
        let class = java_main_class(source).unwrap_or_else(|| "Main".into());
        self.source.replace("{class}", &class)
    }

    /// The compile-error log for a source named after its class that the box cannot hold or run as
    /// `java {class}`: a `package` line (the class is run from the default package) or a class name too
    /// long for a file name.
    pub fn class_source_refusal(&self, source: &[u8]) -> Option<String> {
        if !self.source.contains("{class}") {
            return None;
        }
        if declares_package(source) {
            return Some(PACKAGE_REFUSAL.into());
        }
        (self.source_name(source).len() > MAX_FILE_NAME_BYTES).then(|| LONG_NAME_REFUSAL.into())
    }
}

/// isolate 2.6's default mounts (no `/etc`): a program reached through anything else fails in the box.
const BOX_MOUNTS: [&str; 4] = ["/usr", "/bin", "/lib", "/lib64"];

fn in_box_mounts(path: &Path) -> bool {
    BOX_MOUNTS.iter().any(|mount| path.starts_with(mount))
}

/// Follows `program`'s symlink chain; `Some(real path)` when a hop leaves the box mounts: the box has no `/etc`, so `/etc/alternatives` links do not resolve inside it.
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
    fn duplicate_keys_and_empty_program_runs_are_refused() {
        assert!(LanguageRegistry::from_toml(&MINIMAL.repeat(2)).is_err());
        let no_run = "[[language]]\nkey='A'\nname='a'\nsource='a'\nrun=[]\nversion=[]\n";
        assert!(LanguageRegistry::from_toml(no_run).is_err());
        assert!(LanguageRegistry::from_toml(&format!("{MINIMAL}colour='red'\n")).is_err(), "unknown key");
    }

    #[test]
    fn runtime_resolves_to_the_install_above_the_real_run_binary_but_never_to_a_system_prefix() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("opt/pypy");
        std::fs::create_dir_all(home.join("bin")).unwrap();
        std::fs::write(home.join("bin/pypy3.9"), "").unwrap();
        let link = root.path().join("pypy3");
        std::os::unix::fs::symlink(home.join("bin/pypy3.9"), &link).unwrap();
        let text = format!("{MINIMAL}preload=['{{runtime}}/lib']\n")
            .replace("run=['x']", &format!("run=['{}']", link.display()));
        let language = LanguageRegistry::from_toml(&text).unwrap().get("A").unwrap().clone();
        assert_eq!(language.preload, [format!("{}/lib", home.display())]);
        assert_eq!(
            install_home(Path::new("/usr/bin/pypy3.11")),
            None,
            "Debian's pypy3: /usr is not a runtime"
        );
        assert_eq!(install_home(Path::new("/opt/bin/pypy3")), None, "a binary in /opt/bin: not all of /opt");
        assert_eq!(install_home(Path::new("/opt/pypy/bin/pypy3.9")).as_deref(), Some("/opt/pypy"));
        let apt = MINIMAL.replace("run=['x']", "run=['/nonexistent/pypy3']");
        let apt = format!("{apt}preload=['{{runtime}}/lib', '/usr/lib/pypy3.9']\n");
        let language = LanguageRegistry::from_toml(&apt).unwrap().get("A").unwrap().clone();
        assert_eq!(language.preload, ["/usr/lib/pypy3.9"], "no {{runtime}} here: its layout is dropped");
    }

    #[test]
    fn preload_wildcards_match_by_name_and_an_entry_that_climbs_or_is_a_system_tree_is_refused() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("lib");
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["libpy3.14.so.1", "libpy3.14.a", "libother.so", "py3.14"] {
            std::fs::write(dir.join(name), "").unwrap();
        }
        let wild =
            format!("{MINIMAL}preload=['{0}/libpy3.*.so*', '{0}/py3*', '{0}/nothing*']\n", dir.display());
        let language = LanguageRegistry::from_toml(&wild).unwrap().get("A").unwrap().clone();
        assert!(!language.preload_matched_nothing, "some entries matched");
        let none = format!("{MINIMAL}preload=['{0}/nothing*']\n", dir.display());
        let none = LanguageRegistry::from_toml(&none).unwrap().get("A").unwrap().clone();
        assert!(
            none.preload_matched_nothing && none.preload.is_empty(),
            "a wildcard matching nothing is noticed"
        );
        let found: Vec<_> = language.preload.iter().map(|path| path.rsplit('/').next().unwrap()).collect();
        assert_eq!(found, ["libpy3.14.so.1", "py3.14"], "a wildcard matches by name, not libpy3.14.a");
        for refused in
            ["'/usr/'", "'/'", "'lib'", "'/usr/lib/*'", "'/usr/*/lib'", "'/usr/lib/..'", "'/usr/lib/../..'"]
        {
            let text = format!("{MINIMAL}preload=[{refused}]\n");
            assert!(LanguageRegistry::from_toml(&text).is_err(), "preload {refused}");
        }
    }

    #[test]
    fn jdk_etc_resolves_at_load_and_stays_without_a_jdk() {
        let text = format!("{MINIMAL}dirs=['{{jdk_etc}}']\ncompile_dirs=['/x', '{{jdk_etc}}/security']\n");
        let resolved = LanguageRegistry::from_toml_with(&text, Some("/etc/java-17-openjdk")).unwrap();
        let language = resolved.get("A").unwrap();
        assert_eq!(language.dirs, ["/etc/java-17-openjdk"]);
        assert_eq!(language.compile_dirs, ["/x", "/etc/java-17-openjdk/security"]);
        let unresolved = LanguageRegistry::from_toml_with(&text, None).unwrap();
        assert_eq!(unresolved.get("A").unwrap().dirs, ["{jdk_etc}"]);
    }

    #[test]
    fn jdk_etc_dir_follows_the_default_java_chain_to_its_major() {
        let jvm = tempfile::tempdir().unwrap();
        let link =
            |target: &str, name: &str| std::os::unix::fs::symlink(target, jvm.path().join(name)).unwrap();
        std::fs::create_dir(jvm.path().join("java-17-openjdk-amd64")).unwrap();
        link("java-17-openjdk-amd64", "java-1.17.0-openjdk-amd64");
        link("java-1.17.0-openjdk-amd64", "default-java");
        assert_eq!(jdk_etc_dir(&jvm.path().join("default-java")).as_deref(), Some("/etc/java-17-openjdk"));
        std::fs::create_dir(jvm.path().join("temurin-21-jdk")).unwrap();
        link("temurin-21-jdk", "other-java");
        assert_eq!(jdk_etc_dir(&jvm.path().join("other-java")), None, "not a distro OpenJDK");
        assert_eq!(jdk_etc_dir(&jvm.path().join("missing")), None);
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
    }

    #[test]
    fn java_class_detection_skips_literals_and_nested_types() {
        let in_literals = b"class Main {\n  static String s = \"public class Fake {\";\n  static char c = '{';\n  public static void main(String[] a) {}\n}";
        assert_eq!(java_main_class(in_literals).as_deref(), Some("Main"));
        let nested_main = b"class A { static class B { public static void main(String[] a) {} } }";
        assert_eq!(java_main_class(nested_main), None, "only a top-level type's main runs as `java X`");
        assert_eq!(java_main_class(b"interface I { static void main(String[] a) {} }").as_deref(), Some("I"));
        let modifiers_after_static =
            b"class S { static int f() { return 1; }\n static public void main(String[] a) {} }";
        assert_eq!(java_main_class(modifiers_after_static).as_deref(), Some("S"));
        assert_eq!(java_main_class("public class Bài {}".as_bytes()).as_deref(), Some("Bài"));
    }

    #[test]
    fn java_source_names() {
        let r = shipped();
        assert_eq!(r.get("JAVA").unwrap().source_name(b"public class Q { }"), "Q.java");
        assert_eq!(r.get("JAVA").unwrap().source_name(b"nothing"), "Main.java");
        assert_eq!(r.get("CPP17").unwrap().source_name(b"int main(){}"), "main.cpp");
    }

    #[test]
    fn java_sources_with_a_package_line_are_refused() {
        let java = shipped().get("JAVA").unwrap().clone();
        for source in ["package solution;\npublic class Main {}", "/* x */ package a.b . c ;\nclass Main {}"]
        {
            let refusal = java.class_source_refusal(source.as_bytes());
            assert!(refusal.is_some_and(|log| log.contains("package")), "{source:?}");
        }
        for source in [
            "// package solution;\npublic class Main {}",
            "public class Main { String s = \"package x;\"; }",
            "public class Main { int my_package; }",
        ] {
            assert_eq!(java.class_source_refusal(source.as_bytes()), None, "{source:?}");
        }
    }

    #[test]
    fn java_class_names_are_refused_only_past_the_file_name_limit() {
        let r = shipped();
        let java = r.get("JAVA").unwrap();
        let class_of = |name: &str| format!("public class {name} {{}}");
        let longest = class_of(&"A".repeat(250)); // A…A.java is 255 bytes
        assert_eq!(java.class_source_refusal(longest.as_bytes()), None);
        let too_long = class_of(&"A".repeat(251));
        assert!(java.class_source_refusal(too_long.as_bytes()).is_some_and(|log| log.contains("too long")));
        let multibyte = class_of(&"à".repeat(126)); // 252 bytes of name
        assert!(java.class_source_refusal(multibyte.as_bytes()).is_some());
        assert!(r.get("JAVA8").unwrap().class_source_refusal(too_long.as_bytes()).is_some());
    }

    #[test]
    fn sources_not_named_after_their_class_are_never_refused() {
        let r = shipped();
        let kotlin = b"package solution\nfun main() {}";
        assert_eq!(r.get("KOTLIN").unwrap().class_source_refusal(kotlin), None);
        assert_eq!(r.get("CPP17").unwrap().class_source_refusal(b"int package; int main(){}"), None);
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
