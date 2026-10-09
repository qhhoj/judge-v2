#![cfg(feature = "isolate")]
//! Every manifest row, judged for real in isolate, plus the sandbox escape checks. Languages without a
//! runtime are skipped unless `JUDGE_SUITE_REQUIRE` (comma separated keys) names them. A hung judge
//! cannot hang the run: every judged submission has a watchdog that ends the process.
mod common;

use common::{box_id, sandbox, testlib_dir};
use judge_grader::artefacts::{ArtefactSetup, Artefacts};
use judge_grader::judge::{
    GraderDefaults, JudgeEnv, Outcome, Submission, SubmissionSource, judge_submission,
};
use judge_grader::languages::{LanguageRegistry, host_languages};
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

#[derive(serde::Deserialize, Clone)]
struct Row {
    lang: String,
    file: String,
    expect: String,
}

#[derive(serde::Deserialize)]
struct SameAs {
    lang: String,
    rows_of: String,
    #[serde(default)]
    except: Vec<String>,
}

#[derive(serde::Deserialize)]
struct Manifest {
    rows: Vec<Row>,
    #[serde(default)]
    same_as: Vec<SameAs>,
}

impl Manifest {
    fn load() -> Manifest {
        let text = std::fs::read_to_string(format!("{ROOT}/tests/programs/manifest.toml")).unwrap();
        toml::from_str(&text).unwrap()
    }

    /// The rows plus, per `same_as`, the named key's rows run under another key.
    fn expanded(&self) -> Vec<Row> {
        let mut rows = self.rows.clone();
        for alias in &self.same_as {
            let copies =
                self.rows.iter().filter(|r| r.lang == alias.rows_of && !alias.except.contains(&r.file));
            rows.extend(copies.map(|r| Row { lang: alias.lang.clone(), ..r.clone() }).collect::<Vec<_>>());
        }
        rows
    }
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
    let dir = root.join(code);
    std::fs::create_dir_all(&dir).unwrap();
    let mut init = String::from("test_cases:\n");
    for (i, (input, output)) in cases.iter().enumerate() {
        std::fs::write(dir.join(format!("{i}.in")), input).unwrap();
        std::fs::write(dir.join(format!("{i}.out")), output).unwrap();
        init.push_str(&format!("- {{in: {i}.in, out: {i}.out, points: 1}}\n"));
    }
    std::fs::write(dir.join("init.yml"), init).unwrap();
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

/// The shipped file with every runtime behind `/etc/alternatives` resolved (preflight I18).
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
    write_problem(dirs[0].path(), "aplusb", &[("1 2\n", "3\n"), ("4 5\n", "9\n")]);
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
        defaults: GraderDefaults {
            default_output_limit: 64 << 10,
            default_output_prefix: 64,
            jvm_reserve_kib: 131_072,
        },
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

/// The outcome and every case's status code.
fn judge(s: &mut Suite, code: &str, lang: &str, source: Vec<u8>) -> (Outcome, Vec<String>) {
    let _watchdog = Watchdog::start(format!("{lang} on {code}"), SUBMISSION_LIMIT);
    let jvm = s.registry.get(lang).is_some_and(|l| l.jvm);
    let submission = Submission {
        language_key: lang.into(),
        source: SubmissionSource::Inline(source),
        time_limit_s: 1.0,
        memory_kib: if jvm { 262_144 } else { 65_536 },
        short_circuit: false,
        pretests_only: false,
    };
    let problem = s.resolver.resolve(code, 0).unwrap();
    let mut cases = Cases::default();
    let outcome = judge_submission(&mut s.env, &problem, &submission, &mut cases);
    (outcome, cases.0.iter().map(|c| c.status.code().to_string()).collect())
}

/// "CE" for a compile error, the first case's status code when graded, else the outcome's debug text.
fn summary((outcome, cases): (Outcome, Vec<String>)) -> String {
    match outcome {
        Outcome::CompileError(_) => "CE".into(),
        Outcome::Graded => cases[0].clone(),
        other => format!("{other:?}"),
    }
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
    for row in &Manifest::load().expanded() {
        if !has_runtime(&s.registry, &row.lang) {
            if required.contains(&row.lang) {
                failures.push(format!("{} required but absent", row.lang));
            }
            skipped.insert(row.lang.clone());
            continue;
        }
        let got = verdict(&mut s, "aplusb", &row.lang, &row.file);
        judged += 1;
        if got != row.expect {
            failures.push(format!("{} {}: expected {}, got {got}", row.lang, row.file, row.expect));
        }
    }
    failures.dedup();
    eprintln!("judged {judged} rows; skipped (no runtime): {skipped:?}");
    assert!(judged > 0, "no language has a runtime on this host");
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// isolate 2.6 installs a seccomp filter that kills every non-x86-64 system call (`int 0x80` included),
/// so a 32-bit program dies of SIGSYS whatever it does. GAS32 therefore has no manifest rows; this test
/// pins the limitation so a change of isolate's filter is noticed.
#[test]
fn gas32_programs_are_killed_by_isolates_architecture_filter() {
    let mut s = suite();
    if !has_runtime(&s.registry, "GAS32") {
        return eprintln!("skip GAS32 (no runtime)");
    }
    assert_eq!(verdict(&mut s, "aplusb", "GAS32", "GAS32/ac.s"), "RTE");
}

#[test]
fn every_required_language_is_in_the_manifest() {
    let manifest = Manifest::load().expanded();
    for key in required_keys() {
        assert!(manifest.iter().any(|row| row.lang == key), "{key} is required but has no manifest row");
    }
}

#[test]
fn a_fork_bomb_does_not_hurt_the_next_submission() {
    let mut s = suite();
    assert_eq!(verdict(&mut s, "secure", "CPP17", "CPP17/sec_fork.cpp"), "AC", "fork must fail");
    assert_eq!(verdict(&mut s, "aplusb", "CPP17", "CPP17/ac.cpp"), "AC");
}

#[test]
fn data_and_other_boxes_are_unreadable() {
    let mut s = suite();
    let other_box = format!("/var/local/lib/isolate/{}/box", box_id(SOLUTION_BOX + 1));
    let own_box = format!("/var/local/lib/isolate/{}/box", box_id(SOLUTION_BOX));
    let paths =
        format!("{}/aplusb/0.out\n{other_box}/main.cpp\n{own_box}/main.cpp\n", s.data_root().display());
    write_problem(s.data_root(), "files", &[(&paths, "ok\n")]);
    assert_eq!(verdict(&mut s, "files", "CPP17", "CPP17/sec_files.cpp"), "AC");
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
