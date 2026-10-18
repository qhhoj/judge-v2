#![cfg(feature = "isolate")]
//! Testlib checkers compiled through Artefacts and run in a box: real verdicts, and the box as a jail.
mod common;

use common::{artefacts, cpp_helper, sandbox};
use judge_grader::checkers::custom::{CustomChecker, PartialPoints};
use judge_grader::checkers::select::CustomKind;
use judge_grader::checkers::themis::{ThemisNames, names_for};
use judge_grader::checkers::{CheckInput, CheckOutcome};
use judge_grader::error::GradeError;
use judge_problem::init::FileIo;
use judge_sandbox::IsolateBox;
use std::net::TcpListener;
use std::path::Path;

const CHECKER: &str = r#"#include "testlib.h"
int main(int argc, char* argv[]) {
    registerTestlibCmd(argc, argv);
    int want = ans.readInt(), got = ouf.readInt();
    if (got == want) quitf(_ok, "correct");
    if (got == want + 1) quitp(0.5, "close");
    if (got == want + 2) quitf(_pe, "formatting");
    quitf(_wa, "expected %d, found %d", want, got);
}
"#;

struct Fixture {
    _dirs: Vec<tempfile::TempDir>,
    sandbox: IsolateBox,
    checker: CustomChecker,
    scratch: std::path::PathBuf,
}

fn fixture(offset: u32, kind: CustomKind, source: &str) -> Fixture {
    let dirs: Vec<_> = (0..3).map(|_| tempfile::tempdir().unwrap()).collect();
    let mut sandbox = sandbox(offset, dirs[0].path());
    let program = artefacts(dirs[1].path()).program(&mut sandbox, &cpp_helper(source)).unwrap();
    let scratch = dirs[2].path().to_path_buf();
    Fixture {
        checker: CustomChecker {
            program,
            kind,
            partial: PartialPoints::Absolute,
            themis_names: ThemisNames::default(),
            time_limit_s: 1.0,
        },
        _dirs: dirs,
        sandbox,
        scratch,
    }
}

fn judge(f: &mut Fixture, answer: &str, output: &str) -> Result<CheckOutcome, GradeError> {
    let at = |name: &str| -> std::path::PathBuf { Path::new(&f.scratch).join(name) };
    std::fs::write(at("in"), "").unwrap();
    std::fs::write(at("ans"), answer).unwrap();
    std::fs::write(at("out"), output).unwrap();
    let io = CheckInput { input: &at("in"), output: &at("out"), answer: &at("ans"), points: 1.0 };
    f.checker.check(&mut f.sandbox, &io)
}

#[test]
fn testlib_checker_gives_ac_partial_pe_and_wa() {
    let mut f = fixture(2, CustomKind::Testlib, CHECKER);
    let ac = judge(&mut f, "5\n", "5").unwrap();
    assert!(ac.passed && ac.fraction == 1.0, "{ac:?}");
    let partial = judge(&mut f, "5\n", "6").unwrap();
    assert!(partial.passed && (partial.fraction - 0.5).abs() < 1e-9, "{partial:?}");
    assert_eq!(partial.feedback, "close");
    let pe = judge(&mut f, "5\n", "7").unwrap();
    assert!(!pe.passed);
    assert!(pe.feedback.starts_with("Presentation Error"), "{pe:?}");
    let wa = judge(&mut f, "5\n", "9").unwrap();
    assert!(!wa.passed);
    assert_eq!(wa.feedback, "expected 5, found 9");
    let eof = judge(&mut f, "5\n", "").unwrap();
    assert!(!eof.passed, "a missing token is a wrong answer: {eof:?}");
}

#[test]
fn a_checker_that_calls_fail_or_crashes_is_a_checker_error() {
    let failing = "#include \"testlib.h\"\nint main(int c, char** v){ registerTestlibCmd(c, v); quitf(_fail, \"bad jury data\"); }\n";
    let mut f = fixture(3, CustomKind::Testlib, failing);
    assert!(matches!(judge(&mut f, "", ""), Err(GradeError::Checker(m)) if m.contains("exit code 3")));
    let crashing = "#include \"testlib.h\"\nint main(int c, char** v){ registerTestlibCmd(c, v); *(volatile int*)0 = 1; }\n";
    let mut f = fixture(4, CustomKind::Testlib, crashing);
    assert!(matches!(judge(&mut f, "", ""), Err(GradeError::Checker(m)) if m.contains("SIGSEGV")));
}

const PROBE: &str = r#"#include "testlib.h"
#include <arpa/inet.h>
#include <dirent.h>
#include <fcntl.h>
#include <sys/socket.h>
#include <sys/wait.h>
#include <unistd.h>
int main(int argc, char* argv[]) {
    registerTestlibCmd(argc, argv);
    int port = ans.readInt();
    ouf.readInt();
    int s = socket(AF_INET, SOCK_STREAM, 0);
    sockaddr_in a{};
    a.sin_family = AF_INET;
    a.sin_port = htons(port);
    inet_pton(AF_INET, "127.0.0.1", &a.sin_addr);
    bool net = connect(s, (sockaddr*)&a, sizeof a) == 0;
    bool home = opendir("/home") != nullptr;
    bool write_usr = open("/usr/qhhoj_probe", O_WRONLY | O_CREAT, 0644) >= 0;
    pid_t child = fork();
    if (child == 0) _exit(0);
    bool forked = child > 0;
    if (forked) waitpid(child, nullptr, 0);
    if (net || home || write_usr || forked)
        quitf(_fail, "escaped net=%d home=%d usr=%d fork=%d", net, home, write_usr, forked);
    quitf(_ok, "isolated");
}
"#;

#[test]
fn a_checker_cannot_reach_the_host_network_filesystem_or_fork() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let port = listener.local_addr().unwrap().port();
    let mut f = fixture(5, CustomKind::Testlib, PROBE);
    let verdict = judge(&mut f, &format!("{port}\n"), "0").unwrap();
    assert!(verdict.passed, "{verdict:?}");
    assert_eq!(verdict.feedback, "isolated");
    assert!(listener.accept().is_err(), "the checker reached a host listener");
}

#[test]
fn a_default_checker_reports_through_its_exit_code() {
    let source = "#include <cstdio>\n#include <cstring>\nint main(int, char** v) {\n  FILE* f = fopen(v[2], \"r\"); char c = 0; fread(&c, 1, 1, f);\n  if (c == 'a') return 0;\n  if (c == 'p') { puts(\"0.25\"); return 2; }\n  return 1;\n}\n";
    let mut f = fixture(6, CustomKind::Default, source);
    assert!(judge(&mut f, "", "a").unwrap().passed);
    let partial = judge(&mut f, "", "p").unwrap();
    assert!(partial.passed && (partial.fraction - 0.25).abs() < 1e-9);
    assert!(!judge(&mut f, "", "x").unwrap().passed);
}

const POINTS_CHECKER: &str = r#"#include "testlib.h"
int main(int argc, char* argv[]) {
    registerTestlibCmd(argc, argv);
    int got = ouf.readInt();
    if (got == 0) quitf(_ok, "all");
    quitp(got, "scored");
}
"#;

fn judge_worth(f: &mut Fixture, output: &str, points: f64) -> CheckOutcome {
    let at = |name: &str| -> std::path::PathBuf { Path::new(&f.scratch).join(name) };
    for (name, text) in [("in", ""), ("ans", ""), ("out", output)] {
        std::fs::write(at(name), text).unwrap();
    }
    let io = CheckInput { input: &at("in"), output: &at("out"), answer: &at("ans"), points };
    f.checker.check(&mut f.sandbox, &io).unwrap()
}

#[test]
fn testlib_points_are_case_points_or_a_percentage_with_the_flag() {
    let mut f = fixture(7, CustomKind::Testlib, POINTS_CHECKER);
    let three_of_five = judge_worth(&mut f, "3", 5.0);
    assert!(three_of_five.passed && (three_of_five.fraction - 0.6).abs() < 1e-9, "{three_of_five:?}");
    assert_eq!(judge_worth(&mut f, "9", 5.0).fraction, 1.0, "clamped to the case");
    assert_eq!(judge_worth(&mut f, "0", 5.0).fraction, 1.0, "accepted is full points");
    f.checker.partial = PartialPoints::Percentage;
    let half = judge_worth(&mut f, "50", 40.0);
    assert!((half.fraction - 0.5).abs() < 1e-9, "{half:?}");
}

/// One verdict per output token. `delta` is the pattern production checkers use: `_pc(x - 16)` is
/// `TResult(x)`, so `quitp` takes it as the int x and writes "points x".
const VERDICT_CHECKER: &str = r#"#include "testlib.h"
int main(int argc, char* argv[]) {
    registerTestlibCmd(argc, argv);
    std::string v = ouf.readToken();
    if (v == "pc0") quitf(_pc(0), "none");
    if (v == "pc100") quitf(_pc(100), "half");
    if (v == "pc200") quitf(_pc(200), "all");
    if (v == "quitp") quitp(0.5, "close");
    if (v == "delta") quitp(_pc(5 - 16), "five points");
    if (v == "ok") quitf(_ok, "fine");
    if (v == "wa") quitf(_wa, "wrong");
    if (v == "pe") quitf(_pe, "format");
    quitf(_fail, "jury bug");
}
"#;

#[test]
fn testlib_partially_correct_and_quitp_from_a_real_checker() {
    let mut f = fixture(11, CustomKind::Testlib, VERDICT_CHECKER);
    let pc0 = judge_worth(&mut f, "pc0", 1.0);
    assert!(!pc0.passed && pc0.fraction == 0.0, "_pc(0) earns nothing: {pc0:?}");
    let pc100 = judge_worth(&mut f, "pc100", 1.0);
    assert!(pc100.passed && (pc100.fraction - 0.5).abs() < 1e-9, "{pc100:?}");
    assert_eq!(pc100.feedback, "half");
    let pc200 = judge_worth(&mut f, "pc200", 1.0);
    assert!(pc200.passed && pc200.fraction == 1.0, "{pc200:?}");
    let quitp = judge_worth(&mut f, "quitp", 1.0);
    assert!(quitp.passed && (quitp.fraction - 0.5).abs() < 1e-9, "{quitp:?}");
    assert_eq!(quitp.feedback, "close");
    let delta = judge_worth(&mut f, "delta", 100.0);
    assert!(delta.passed && (delta.fraction - 0.05).abs() < 1e-9, "5 of 100 points: {delta:?}");
}

/// Debug lines on stderr before the verdict, as checkers that trace with `cerr` print them.
const DEBUG_CHECKER: &str = r#"#include "testlib.h"
int main(int argc, char* argv[]) {
    registerTestlibCmd(argc, argv);
    std::string v = ouf.readToken();
    std::cerr << "12 7 3" << std::endl;
    std::cerr << "points 9 (a debug pair)" << std::endl;
    if (v == "quitp") quitp(0.25, "quarter");
    if (v == "pc") quitf(_pc(100), "half");
    quitf(_wa, "wrong");
}
"#;

#[test]
fn testlib_verdicts_after_debug_lines_from_a_real_checker() {
    let mut f = fixture(14, CustomKind::Testlib, DEBUG_CHECKER);
    let quitp = judge_worth(&mut f, "quitp", 1.0);
    assert!(quitp.passed && (quitp.fraction - 0.25).abs() < 1e-9, "{quitp:?}");
    assert_eq!(quitp.feedback, "quarter");
    let pc = judge_worth(&mut f, "pc", 1.0);
    assert!(pc.passed && (pc.fraction - 0.5).abs() < 1e-9, "{pc:?}");
    let wa = judge_worth(&mut f, "wa", 1.0);
    assert!(!wa.passed && wa.feedback == "wrong", "{wa:?}");
}

#[test]
fn testlib_ok_wa_pe_and_fail_from_the_same_checker() {
    let mut f = fixture(12, CustomKind::Testlib, VERDICT_CHECKER);
    let ok = judge_worth(&mut f, "ok", 1.0);
    assert!(ok.passed && ok.fraction == 1.0, "{ok:?}");
    let wa = judge_worth(&mut f, "wa", 1.0);
    assert!(!wa.passed && wa.fraction == 0.0 && wa.feedback == "wrong", "{wa:?}");
    let pe = judge_worth(&mut f, "pe", 1.0);
    assert!(!pe.passed && pe.feedback.starts_with("Presentation Error"), "{pe:?}");
    let fail = judge(&mut f, "", "fail");
    assert!(matches!(&fail, Err(GradeError::Checker(m)) if m.contains("exit code 3")), "{fail:?}");
}

/// Old checkers include the Themis/CMS testlib variant and score with `quitf(_points, "%f ...")` or `quitp`.
const THEMIS_CMS_CHECKER: &str = r#"#include "testlib_themis_cms.h"
int main(int argc, char* argv[]) {
#ifdef THEMIS
    registerTestlibThemis("a.inp", "a.out");
#else
    registerTestlibCmd(argc, argv);
#endif
    std::string v = ouf.readToken();
    if (v == "ok") quitf(_ok, "fine");
    if (v == "points") quitf(_points, "%f \n partial", 0.25);
    if (v == "quitp") quitp(30, "thirty");
    quitf(_wa, "wrong");
}
"#;

#[test]
fn a_checker_including_testlib_themis_cms_compiles_and_grades() {
    let mut f = fixture(13, CustomKind::Testlib, THEMIS_CMS_CHECKER);
    let ok = judge_worth(&mut f, "ok", 1.0);
    assert!(ok.passed && ok.fraction == 1.0, "{ok:?}");
    let wa = judge_worth(&mut f, "x", 1.0);
    assert!(!wa.passed && wa.fraction == 0.0 && wa.feedback == "wrong", "{wa:?}");
    let points = judge_worth(&mut f, "points", 1.0);
    assert!(points.passed && (points.fraction - 0.25).abs() < 1e-9, "{points:?}");
    f.checker.partial = PartialPoints::Percentage;
    let quitp = judge_worth(&mut f, "quitp", 9.0);
    assert!(quitp.passed && (quitp.fraction - 0.3).abs() < 1e-9, "{quitp:?}");
}

/// Reads both directories with getline (CRLF-safe) and opens upper-case names; compares with the answer file.
const THEMIS_ANSWER_FILE: &str = r#"#include <fstream>
#include <iostream>
#include <string>
int main() {
    std::string test, work;
    std::getline(std::cin, test);
    std::getline(std::cin, work);
    for (std::string* s : {&test, &work}) if (!s->empty() && s->back() == '\r') s->pop_back();
    std::ifstream ans(test + "/Sum.OUT"), out(work + "/Sum.OUT"), in(test + "/Sum.INP");
    long long a, b, want, got;
    if (!(in >> a >> b) || !(ans >> want)) return 3;
    if (!(out >> got)) { std::cout << "Khong doc duoc output\n0\n"; return 0; }
    if (got == want) std::cout << "Dung\n1.0\n";
    else if (got == want + 1) std::cout << "Gan dung\n0.5\n";
    else std::cout << "Sai\n0\n";
}
"#;

/// Reads the directories with scanf, joins with "%s/%s", opens lower-case names and recomputes the answer.
const THEMIS_RECOMPUTE: &str = r#"#include <cstdio>
int main() {
    char test[512], work[512], path[1100];
    if (scanf("%511s %511s", test, work) != 2) return 3;
    long long a, b, got;
    snprintf(path, sizeof path, "%s/%s", test, "sum.inp");
    FILE* in = fopen(path, "r");
    if (!in || fscanf(in, "%lld %lld", &a, &b) != 2) return 3;
    snprintf(path, sizeof path, "%s/%s", work, "sum.out");
    FILE* out = fopen(path, "r");
    if (!out || fscanf(out, "%lld", &got) != 1) { printf("Thieu output\n0\n"); return 0; }
    printf(got == a + b ? "1\n" : "Sai\n0\n");
}
"#;

fn themis(offset: u32, source: &str, file_io: Option<FileIo>) -> Fixture {
    let mut f = fixture(offset, CustomKind::Themis, source);
    f.checker.themis_names = names_for(file_io.as_ref(), source.as_bytes());
    f
}

fn judge_case(f: &mut Fixture, input: &str, answer: &str, output: &str) -> CheckOutcome {
    let at = |name: &str| -> std::path::PathBuf { Path::new(&f.scratch).join(name) };
    for (name, text) in [("in", input), ("ans", answer), ("out", output)] {
        std::fs::write(at(name), text).unwrap();
    }
    let io = CheckInput { input: &at("in"), output: &at("out"), answer: &at("ans"), points: 1.0 };
    f.checker.check(&mut f.sandbox, &io).unwrap()
}

#[test]
fn a_themis_checker_finds_the_case_files_by_its_own_names_and_scores_from_stdout() {
    let io = FileIo { input: "sum.inp".into(), output: "sum.out".into() };
    let mut f = themis(8, THEMIS_ANSWER_FILE, Some(io));
    let ac = judge_case(&mut f, "2 3\n", "5\n", "5\n");
    assert!(ac.passed && ac.fraction == 1.0 && ac.feedback == "Dung", "{ac:?}");
    let half = judge_case(&mut f, "2 3\n", "5\n", "6\n");
    assert!(half.passed && (half.fraction - 0.5).abs() < 1e-9, "{half:?}");
    let wa = judge_case(&mut f, "2 3\n", "5\n", "9\n");
    assert!(!wa.passed && wa.fraction == 0.0 && wa.feedback == "Sai", "{wa:?}");
    let empty = judge_case(&mut f, "2 3\n", "5\n", "");
    assert!(!empty.passed && empty.feedback == "Khong doc duoc output", "{empty:?}");
}

#[test]
fn a_themis_checker_without_file_io_is_laid_out_by_its_source_literals() {
    let mut f = themis(9, THEMIS_RECOMPUTE, None);
    assert!(judge_case(&mut f, "2 3\n", "", "5\n").passed);
    let wa = judge_case(&mut f, "2 3\n", "", "4\n");
    assert!(!wa.passed && wa.feedback == "Sai", "{wa:?}");
    assert_eq!(judge_case(&mut f, "2 3\n", "", "").fraction, 0.0, "no output is score 0");
}

/// Joins the directories and names without a separator and reads the answer and the output as `.ans`.
const THEMIS_NO_SEPARATOR_ANS: &str = r#"#include <fstream>
#include <iostream>
#include <string>
int main() {
    std::string test, work;
    std::getline(std::cin, test);
    std::getline(std::cin, work);
    std::ifstream in(test + "polygon.inp"), ans(test + "polygon.ans"), out(work + "polygon.ans");
    long long a, b, want, got;
    if (!(in >> a >> b) || !(ans >> want)) return 3;
    if (!(out >> got)) { std::cout << "Thieu output\n0\n"; return 0; }
    std::cout << (got == want ? "1" : "Sai\n0") << "\n";
}
"#;

/// Names its files `name ".INP"` / `name ".OUT"` with `#define name "/Path"`: no whole-name literal.
const THEMIS_DEFINED_NAME: &str = r#"#include <fstream>
#include <iostream>
#include <string>
#define name "/Path"
int main() {
    std::string test, work;
    std::getline(std::cin, test);
    std::getline(std::cin, work);
    std::ifstream in(test + name ".INP"), ans(test + name ".OUT"), out(work + name ".OUT");
    long long a, b, want, got;
    if (!(in >> a >> b) || !(ans >> want)) return 3;
    if (!(out >> got)) { std::cout << "Thieu output\n0\n"; return 0; }
    std::cout << (got == want ? "1" : "Sai\n0") << "\n";
}
"#;

fn judge_named(f: &mut Fixture, case_files: (&str, &str), answer: &str, output: &str) -> CheckOutcome {
    let at = |name: &str| -> std::path::PathBuf { Path::new(&f.scratch).join(name) };
    for (name, text) in [("in", "2 3\n"), ("ans", answer), ("out", output)] {
        std::fs::write(at(name), text).unwrap();
    }
    let io = CheckInput { input: &at("in"), output: &at("out"), answer: &at("ans"), points: 1.0 };
    f.checker.check_case(&mut f.sandbox, &io, case_files).unwrap()
}

#[test]
fn a_themis_checker_finds_the_case_files_under_their_own_names_without_a_separator() {
    let mut f = themis(15, THEMIS_NO_SEPARATOR_ANS, None);
    let case = ("01/polygon.inp", "01/polygon.ans");
    let ac = judge_named(&mut f, case, "5\n", "5\n");
    assert!(ac.passed && ac.fraction == 1.0, "{ac:?}");
    let wa = judge_named(&mut f, case, "5\n", "4\n");
    assert!(!wa.passed && wa.feedback == "Sai", "{wa:?}");
}

#[test]
fn a_themis_checker_with_a_defined_name_and_no_file_io_finds_the_case_files() {
    let mut f = themis(16, THEMIS_DEFINED_NAME, None);
    let case = ("Test01/Path.inp", "Test01/Path.out");
    let ac = judge_named(&mut f, case, "5\n", "5\n");
    assert!(ac.passed && ac.fraction == 1.0, "{ac:?}");
    let missing = judge_named(&mut f, case, "5\n", "");
    assert!(!missing.passed && missing.feedback == "Thieu output", "{missing:?}");
}

/// A default-kind Java checker (backend maps `.java` checkers to JAVA8): exit 0 when the tokens match.
const JAVA_CHECKER: &str = r#"import java.nio.file.*;
public class Checker {
    public static void main(String[] args) throws Exception {
        String got = new String(Files.readAllBytes(Paths.get(args[1]))).trim();
        String want = new String(Files.readAllBytes(Paths.get(args[2]))).trim();
        if (got.equals(want)) System.exit(0);
        System.err.println("expected " + want + ", found " + got);
        System.exit(1);
    }
}
"#;

#[test]
fn java_checker_runs_and_accepts_a_correct_output() {
    use judge_grader::artefacts::HelperSource;
    use judge_grader::program::SourceFile;
    let registry = common::registry();
    if common::language_or_skip(&registry, "JAVA8").is_none() {
        return;
    }
    let dirs: Vec<_> = (0..3).map(|_| tempfile::tempdir().unwrap()).collect();
    let mut sandbox = sandbox(40, dirs[0].path());
    let file = SourceFile { name: "Checker.java".into(), bytes: JAVA_CHECKER.as_bytes().to_vec() };
    let source = HelperSource { lang: "JAVA8".into(), files: vec![file] };
    let program = artefacts(dirs[1].path()).program(&mut sandbox, &source).unwrap();
    let mut f = Fixture {
        checker: CustomChecker {
            program,
            kind: CustomKind::Default,
            partial: PartialPoints::Absolute,
            themis_names: ThemisNames::default(),
            time_limit_s: 1.0,
        },
        scratch: dirs[2].path().to_path_buf(),
        _dirs: dirs,
        sandbox,
    };
    let ac = judge(&mut f, "5\n", "5\n");
    assert!(matches!(&ac, Ok(o) if o.passed), "correct output must be AC: {ac:?}");
    let wa = judge(&mut f, "5\n", "6\n");
    assert!(matches!(&wa, Ok(o) if !o.passed), "wrong output is WA: {wa:?}");
}

/// Runs only the JVM (no javac needed): the checker argv's heap flag must be a real size the JVM accepts in
/// the 1 GiB checker box, so a JVM startup error never reaches `interpret` as a checker "exit 1" (WA).
#[test]
fn a_java_checker_jvm_starts_with_a_valid_heap_flag() {
    let registry = common::registry();
    let java = registry.get("JAVA8").unwrap();
    if !std::path::Path::new(&java.run[0]).exists() {
        eprintln!("skip: no JVM");
        return;
    }
    let dirs: Vec<_> = (0..3).map(|_| tempfile::tempdir().unwrap()).collect();
    let sandbox = sandbox(41, dirs[0].path());
    let mut program = judge_grader::compile::program_for(java, dirs[1].path(), b"public class Checker {}");
    // A host without the distro JDK's /etc dir still starts the JVM far enough to read its flags.
    program.dirs.retain(|d| std::path::Path::new(d.rsplit('=').next().unwrap()).exists());
    let mut f = Fixture {
        checker: CustomChecker {
            program,
            kind: CustomKind::Default,
            partial: PartialPoints::Absolute,
            themis_names: ThemisNames::default(),
            time_limit_s: 1.0,
        },
        scratch: dirs[2].path().to_path_buf(),
        _dirs: dirs,
        sandbox,
    };
    let result = judge(&mut f, "5\n", "5\n");
    // No class was compiled, so a JVM that accepted its flags gets as far as looking up the main class.
    let started = matches!(&result, Ok(o) if !o.passed && o.extended.contains("main class Checker"));
    assert!(started, "the JVM must start with its heap flag and fail only on the missing class: {result:?}");
}
