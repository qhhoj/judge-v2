#![cfg(feature = "isolate")]
//! Testlib checkers compiled through Artefacts and run in a box: real verdicts, and the box as a jail.
mod common;

use common::{artefacts, cpp_helper, sandbox};
use judge_grader::checkers::custom::{CustomChecker, PartialPoints};
use judge_grader::checkers::select::CustomKind;
use judge_grader::checkers::{CheckInput, CheckOutcome};
use judge_grader::error::GradeError;
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
        checker: CustomChecker { program, kind, partial: PartialPoints::Absolute, time_limit_s: 1.0 },
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
