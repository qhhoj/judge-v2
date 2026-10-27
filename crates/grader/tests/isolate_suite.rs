#![cfg(feature = "isolate")]
//! Every manifest row, judged for real in isolate, plus the sandbox escape checks. Languages without a
//! runtime are skipped unless `JUDGE_SUITE_REQUIRE` (comma separated keys) names them. A hung judge
//! cannot hang the run: every judged submission has a watchdog that ends the process.
mod common;

use common::{box_id, sandbox, testlib_dir};
use judge_grader::artefacts::{ArtefactSetup, Artefacts};
use judge_grader::judge::{JudgeEnv, Outcome, judge_submission};
use judge_grader::languages::{LanguageRegistry, host_languages};
use judge_grader::manifest::{self, Manifest};
use judge_grader::report::{CaseReport, Reporter};
use judge_problem::data_version::{DataError, FsStamps, WaitPolicy};
use judge_problem::dir_cache::DirCache;
use judge_problem::resolve::Resolver;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc;
use std::time::Duration;

const ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
/// Upper bound for one submission (compile + two cases + helpers); Kotlin's compile is the slowest.
const SUBMISSION_LIMIT: Duration = Duration::from_secs(240);
const SOLUTION_BOX: u32 = 40;

fn manifest() -> Manifest {
    Manifest::parse(&std::fs::read_to_string(format!("{ROOT}/tests/programs/manifest.toml")).unwrap())
        .unwrap()
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

/// Ends the process if dropped late: a stuck sandbox must fail the run, not hang it.
struct Watchdog(mpsc::Sender<()>);

impl Watchdog {
    fn start(what: String, limit: Duration) -> Watchdog {
        let (done, wait) = mpsc::channel();
        std::thread::spawn(move || {
            if wait.recv_timeout(limit) == Err(mpsc::RecvTimeoutError::Timeout) {
                eprintln!("isolate_suite: {what} still running after {limit:?}; aborting the run");
                std::process::exit(101);
            }
        });
        Watchdog(done)
    }
}

impl Drop for Watchdog {
    fn drop(&mut self) {
        let _ = self.0.send(());
    }
}

fn has_runtime(registry: &LanguageRegistry, key: &str) -> bool {
    let Some(language) = registry.get(key) else { return false };
    let Some((program, args)) = language.version.split_first() else { return true };
    let output = std::process::Command::new(program).args(args).output();
    output.is_ok_and(|o| o.status.success())
}

fn write_problem(root: &Path, code: &str, cases: &[(&str, &str)]) {
    manifest::write_problem(&root.join(code), cases).unwrap();
}

/// A problem with extra files and its own init.yml.
fn write_raw(root: &Path, code: &str, init: &str, files: &[(&str, String)]) {
    let dir = root.join(code);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("init.yml"), init).unwrap();
    for (name, text) in files {
        std::fs::write(dir.join(name), text).unwrap();
    }
}

struct Suite {
    env: JudgeEnv,
    resolver: Resolver,
    registry: LanguageRegistry,
    dirs: Vec<tempfile::TempDir>,
}

impl Suite {
    fn data_root(&self) -> &Path {
        self.dirs[0].path()
    }
}

/// The shipped file with every runtime behind `/etc/alternatives` resolved (the box has no `/etc`).
fn host_registry() -> LanguageRegistry {
    let text = std::fs::read_to_string(format!("{ROOT}/languages.toml")).unwrap();
    let (host, rewrites) = host_languages(&text, &LanguageRegistry::from_toml(&text).unwrap());
    for (from, to) in &rewrites {
        eprintln!("host rewrite {from} -> {to}");
    }
    LanguageRegistry::from_toml(&host).unwrap()
}

fn suite() -> Suite {
    let dirs: Vec<_> = (0..4).map(|_| tempfile::tempdir().unwrap()).collect();
    manifest::write_problems(dirs[0].path(), &PathBuf::from(ROOT).join("tests/programs")).unwrap();
    write_problem(dirs[0].path(), "secure", &[("", "ok\n")]);
    let registry = host_registry();
    let cache = Arc::new(DirCache::new(dirs[1].path().into(), 4 << 30));
    let build_ids = registry.keys().into_iter().map(|k| (k.to_string(), "suite".to_string())).collect();
    let setup = ArtefactSetup {
        cache: cache.clone(),
        registry: Arc::new(registry.clone()),
        build_ids,
        testlib_dir: testlib_dir(),
    };
    let env = JudgeEnv {
        solution: sandbox(SOLUTION_BOX, dirs[2].path()),
        helper: sandbox(SOLUTION_BOX + 1, dirs[2].path()),
        artefacts: Arc::new(Artefacts::new(setup)),
        stamps: Arc::new(FsStamps),
        scratch: dirs[3].path().into(),
        defaults: manifest::DEFAULTS,
    };
    let wait = WaitPolicy { timeout: Duration::from_millis(100), poll: Duration::from_millis(20) };
    let resolver = Resolver {
        root: dirs[0].path().into(),
        cache,
        wait,
        zip: Default::default(),
        stamps: Arc::new(FsStamps),
    };
    Suite { env, resolver, registry, dirs }
}

/// The outcome and every reported case.
fn judge_cases(s: &mut Suite, code: &str, lang: &str, source: Vec<u8>) -> (Outcome, Vec<CaseReport>) {
    let _watchdog = Watchdog::start(format!("{lang} on {code}"), SUBMISSION_LIMIT);
    let jvm = s.registry.get(lang).is_some_and(|l| l.jvm);
    let submission = manifest::submission(lang, jvm, source);
    let problem = s.resolver.resolve(code, 0).unwrap();
    let mut cases = Cases::default();
    let outcome = judge_submission(&mut s.env, &problem, &submission, &mut cases);
    (outcome, cases.0)
}

/// The outcome and every case's status code.
fn judge(s: &mut Suite, code: &str, lang: &str, source: Vec<u8>) -> (Outcome, Vec<String>) {
    let (outcome, cases) = judge_cases(s, code, lang, source);
    (outcome, cases.iter().map(|c| c.status.code().to_string()).collect())
}

/// "CE" for a compile error, the first case's status code when graded, else the outcome's debug text.
fn summary((outcome, cases): (Outcome, Vec<String>)) -> String {
    let codes: Vec<&str> = cases.iter().map(String::as_str).collect();
    manifest::statuses(&outcome, &codes).swap_remove(0)
}

fn program(file: &str) -> Vec<u8> {
    std::fs::read(PathBuf::from(ROOT).join("tests/programs").join(file)).unwrap()
}

fn verdict(s: &mut Suite, code: &str, lang: &str, file: &str) -> String {
    summary(judge(s, code, lang, program(file)))
}

fn required_keys() -> Vec<String> {
    let list = std::env::var("JUDGE_SUITE_REQUIRE").unwrap_or_default();
    list.split(',').filter(|s| !s.is_empty()).map(String::from).collect()
}

#[test]
fn every_manifest_row_gets_its_verdict() {
    let required = required_keys();
    let mut s = suite();
    let mut failures = Vec::new();
    let (mut skipped, mut judged) = (std::collections::BTreeSet::new(), 0);
    for row in &manifest().expanded() {
        if !has_runtime(&s.registry, &row.lang) {
            if required.contains(&row.lang) {
                failures.push(format!("{} required but absent", row.lang));
            }
            skipped.insert(row.lang.clone());
            continue;
        }
        let (outcome, cases) = judge_cases(&mut s, row.problem(), &row.lang, program(&row.file));
        let codes: Vec<&str> = cases.iter().map(|c| c.status.code()).collect();
        let got = manifest::statuses(&outcome, &codes);
        let points: Vec<f64> = cases.iter().map(|c| c.points).collect();
        judged += 1;
        if !row.matches(&got, &points) {
            failures.push(format!(
                "{} {}: expected {:?}, got {got:?} {points:?}",
                row.lang,
                row.file,
                row.expected()
            ));
        }
    }
    failures.dedup();
    eprintln!("judged {judged} rows; skipped (no runtime): {skipped:?}");
    assert!(judged > 0, "no language has a runtime on this host");
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn every_required_language_is_in_the_manifest() {
    let rows = manifest().expanded();
    for key in required_keys() {
        assert!(rows.iter().any(|row| row.lang == key), "{key} is required but has no manifest row");
    }
}

#[test]
fn a_pypy_program_gets_the_same_gc_nursery_on_every_host() {
    let mut s = suite();
    if !has_runtime(&s.registry, "PYPY3") {
        assert!(!required_keys().contains(&"PYPY3".to_string()), "PYPY3 required but absent");
        return eprintln!("skipped: no PYPY3 runtime");
    }
    write_problem(s.data_root(), "nursery", &[("", "4194304\n")]);
    let result = judge(&mut s, "nursery", "PYPY3", program("PYPY3/gc_nursery.py"));
    assert_eq!(result, (Outcome::Graded, vec!["AC".to_string()]), "the nursery is not 4 MiB in the box");
}

#[test]
fn a_fork_bomb_does_not_hurt_the_next_submission() {
    let mut s = suite();
    assert_eq!(verdict(&mut s, "secure", "CPP17", "CPP17/sec_fork.cpp"), "AC", "fork must fail");
    assert_eq!(verdict(&mut s, "aplusb", "CPP17", "CPP17/ac.cpp"), "AC");
}

/// Host paths that exist now and would be readable if the sandbox leaked: the problem data and the
/// directories of both judge boxes (the solution runs in one, the helper lives in the other).
fn leak_targets(s: &Suite) -> Vec<PathBuf> {
    let box_dir = |offset| PathBuf::from(format!("/var/local/lib/isolate/{}/box", box_id(offset)));
    let targets = vec![
        s.data_root().join("aplusb/0.out"),
        s.data_root().join("aplusb"),
        box_dir(SOLUTION_BOX),
        box_dir(SOLUTION_BOX + 1),
    ];
    for path in &targets {
        assert!(path.exists(), "the control needs {} to exist on the host", path.display());
    }
    targets
}

fn probe_input(mode: &str, targets: &[PathBuf]) -> String {
    let paths: Vec<_> = targets.iter().map(|p| p.display().to_string()).collect();
    format!("{mode} {}\n{}\n", std::process::id(), paths.join("\n"))
}

/// Runs the probe outside any sandbox: the attacks it makes must reach their targets there.
fn run_probe_on_host(input: &str) -> String {
    use std::io::Write;
    let dir = tempfile::tempdir().unwrap();
    let binary = dir.path().join("probe");
    let source = PathBuf::from(ROOT).join("tests/programs/CPP17/sec_probe.cpp");
    let built =
        std::process::Command::new("g++").args(["-O1", "-pthread", "-o"]).arg(&binary).arg(source).status();
    assert!(built.unwrap().success(), "the probe must compile");
    let mut child = std::process::Command::new(&binary)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
    let _watchdog = Watchdog::start("host probe".into(), Duration::from_secs(60));
    let out = child.wait_with_output().unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn the_sandbox_blocks_host_access_and_privilege_that_work_outside_it() {
    let mut s = suite();
    let targets = leak_targets(&s);
    assert_eq!(
        run_probe_on_host(&probe_input("host", &targets)),
        "ok",
        "control: the attacks reach their targets outside the sandbox"
    );
    write_problem(s.data_root(), "probe", &[(&probe_input("box", &targets), "ok\n")]);
    let (outcome, cases) = judge_cases(&mut s, "probe", "CPP17", program("CPP17/sec_probe.cpp"));
    assert_eq!(outcome, Outcome::Graded);
    let seen = cases.first().map(|c| format!("{} {}", c.status.code(), c.output_prefix));
    assert_eq!(seen.as_deref(), Some("AC ok\n"), "attacks that got through are listed after the status");
}

/// A host TCP listener and an abstract AF_UNIX listener; non-blocking so "accepted nothing" is checkable.
struct Listeners {
    tcp: std::net::TcpListener,
    unix: std::os::unix::net::UnixListener,
    port: u16,
    name: String,
}

fn listeners(tag: &str) -> Listeners {
    use std::os::linux::net::SocketAddrExt;
    let tcp = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    tcp.set_nonblocking(true).unwrap();
    let name = format!("qhhoj-suite-{tag}-{}", std::process::id());
    let address = std::os::unix::net::SocketAddr::from_abstract_name(name.as_bytes()).unwrap();
    let unix = std::os::unix::net::UnixListener::bind_addr(&address).unwrap();
    unix.set_nonblocking(true).unwrap();
    let port = tcp.local_addr().unwrap().port();
    Listeners { tcp, unix, port, name }
}

impl Listeners {
    fn accepted_nothing(&self) -> bool {
        let would_block = |e: std::io::Error| e.kind() == std::io::ErrorKind::WouldBlock;
        self.tcp.accept().err().is_some_and(would_block) && self.unix.accept().err().is_some_and(would_block)
    }
}

#[test]
fn network_and_abstract_unix_sockets_are_unreachable() {
    let mut s = suite();
    let l = listeners("net");
    write_problem(s.data_root(), "net", &[(&format!("{} {}\n", l.port, l.name), "ok\n")]);
    assert_eq!(verdict(&mut s, "net", "CPP17", "CPP17/sec_net.cpp"), "AC");
    assert!(l.accepted_nothing(), "a sandboxed program reached a host listener");
}

#[test]
fn problem_data_cannot_be_included_at_compile_time() {
    let mut s = suite();
    let secret = s.data_root().join("secure/secret.h");
    std::fs::write(&secret, "#include <cstdio>\nint main() { std::puts(\"ok\"); }\n").unwrap();
    let source = format!("#include \"{}\"\n", secret.display());
    assert_eq!(
        summary(judge(&mut s, "secure", "CPP17", source.into_bytes())),
        "CE",
        "the compiler read problem data"
    );
}

#[test]
fn tmp_and_box_files_do_not_survive_between_cases() {
    let mut s = suite();
    write_problem(s.data_root(), "tmpcheck", &[("1\n", "ok\n"), ("2\n", "ok\n")]);
    let result = judge(&mut s, "tmpcheck", "CPP17", program("CPP17/sec_tmp.cpp"));
    assert_eq!(result, (Outcome::Graded, vec!["AC".to_string(), "AC".to_string()]));
}

/// Tries the host listener and two data paths; "contained" only when all fail.
const HELPER_PROBE: &str = r#"#include "testlib.h"
#include <arpa/inet.h>
#include <cstdio>
#include <sys/socket.h>
#include <unistd.h>
static bool leaks() {
    int s = socket(AF_INET, SOCK_STREAM, 0);
    sockaddr_in a{}; a.sin_family = AF_INET; a.sin_port = htons(PORT); inet_pton(AF_INET, "127.0.0.1", &a.sin_addr);
    if (s >= 0 && connect(s, (sockaddr*)&a, sizeof a) == 0) return true;
    for (const char* path : {DATA, "/srv/qhhoj/problem-data"}) if (std::FILE* f = std::fopen(path, "r")) { std::fclose(f); return true; }
    return false;
}
"#;

fn probe(l: &Listeners, s: &Suite) -> String {
    let data = s.data_root().join("aplusb/0.out");
    HELPER_PROBE.replace("PORT", &l.port.to_string()).replace("DATA", &format!("\"{}\"", data.display()))
}

const ONE_CASE: &str = "test_cases:\n- {in: 0.in, out: 0.out, points: 1}\n";
const HI: &[u8] = b"#include <cstdio>\nint main() { std::puts(\"hi\"); }\n";

fn one_case_files(extra: (&str, String)) -> Vec<(&str, String)> {
    vec![("0.in", "x\n".to_string()), ("0.out", "x\n".to_string()), extra]
}

#[test]
fn custom_checkers_and_interactors_cannot_reach_network_or_data() {
    let mut s = suite();
    let l = listeners("helpers");
    let checker = probe(&l, &s)
        + "int main(int argc, char* argv[]) { registerTestlibCmd(argc, argv);\n    ouf.readWord();\n    if (leaks()) quitf(_wa, \"checker escaped\"); quitf(_ok, \"contained\"); }\n";
    let interactor = probe(&l, &s)
        + "int main(int argc, char* argv[]) { registerInteraction(argc, argv);\n    if (leaks()) quitf(_wa, \"interactor escaped\"); ouf.readWord(); quitf(_ok, \"contained\"); }\n";
    let with_checker = format!(
        "{ONE_CASE}checker: {{name: bridged, args: {{type: testlib, lang: CPP17, files: chk.cpp}}}}\n"
    );
    let with_interactor =
        format!("{ONE_CASE}interactive: {{files: inter.cpp, type: testlib, lang: CPP17}}\n");
    write_raw(s.data_root(), "chkprobe", &with_checker, &one_case_files(("chk.cpp", checker)));
    write_raw(s.data_root(), "interprobe", &with_interactor, &one_case_files(("inter.cpp", interactor)));
    assert_eq!(summary(judge(&mut s, "chkprobe", "CPP17", HI.to_vec())), "AC", "custom checker");
    assert_eq!(summary(judge(&mut s, "interprobe", "CPP17", HI.to_vec())), "AC", "interactor");
    assert!(l.accepted_nothing(), "a checker or interactor reached a host listener");
}

#[test]
fn interactor_crashes_are_internal_errors() {
    let mut s = suite();
    let crashes = [
        ("inter_exit3", "int main() { return 3; }\n"),
        ("inter_segv", "#include <csignal>\nint main() { std::raise(SIGSEGV); }\n"),
    ];
    for (code, source) in crashes {
        let init = format!("{ONE_CASE}interactive: {{files: inter.cpp, type: testlib, lang: CPP17}}\n");
        write_raw(s.data_root(), code, &init, &one_case_files(("inter.cpp", source.to_string())));
        let got = summary(judge(&mut s, code, "CPP17", b"int main() { return 0; }\n".to_vec()));
        assert!(got.starts_with("InternalError"), "{code}: {got}");
    }
}

#[test]
fn file_io_names_that_leave_the_box_never_reach_the_grader() {
    let s = suite();
    let init = format!("{ONE_CASE}file_io: {{input: ../x, output: /etc/passwd}}\n");
    write_raw(s.data_root(), "fileio_escape", &init, &one_case_files(("unused", String::new())));
    // The daemon turns a data error other than Unavailable into Final{INTERNAL_ERROR}.
    assert!(matches!(s.resolver.resolve("fileio_escape", 0), Err(DataError::Init(_))));
}

#[test]
fn a_case_file_symlinked_out_of_the_data_root_is_an_internal_error() {
    let mut s = suite();
    write_raw(s.data_root(), "linked", ONE_CASE, &[("0.out", "x\n".into())]);
    std::os::unix::fs::symlink("/etc/hostname", s.data_root().join("linked/0.in")).unwrap();
    let got = summary(judge(&mut s, "linked", "CPP17", program("CPP17/ac.cpp")));
    assert!(got.starts_with("InternalError"), "{got}");
}
