//! The binary's contract: exit codes and output. Every child process runs under a deadline and is killed
//! when it passes, so a hung binary fails the test instead of hanging the suite.
use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

struct Finished {
    code: Option<i32>,
    #[allow(dead_code)] // read by the isolate tests only
    stdout: String,
    stderr: String,
}

fn drain(mut pipe: impl Read + Send + 'static) -> std::thread::JoinHandle<String> {
    std::thread::spawn(move || {
        let mut text = String::new();
        let _ = pipe.read_to_string(&mut text);
        text
    })
}

/// Runs the binary; kills it and panics with its output so far when `limit` passes.
fn run_within(args: &[&str], limit: Duration) -> Finished {
    let mut child = Command::new(env!("CARGO_BIN_EXE_qhhoj-judge"))
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let (stdout, stderr) = (drain(child.stdout.take().unwrap()), drain(child.stderr.take().unwrap()));
    let deadline = Instant::now() + limit;
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() > deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("qhhoj-judge {args:?} still running after {limit:?}: {}", stdout.join().unwrap());
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    Finished { code: status.code(), stdout: stdout.join().unwrap(), stderr: stderr.join().unwrap() }
}

fn bin(args: &[&str]) -> Finished {
    run_within(args, Duration::from_secs(20))
}

#[test]
fn a_bad_or_missing_config_exits_2_with_the_reason() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("c.toml");
    std::fs::write(&config, "name = ''\n").unwrap();
    let out = bin(&["--config", config.to_str().unwrap()]);
    assert_eq!(out.code, Some(2), "{}", out.stderr);
    assert!(out.stderr.contains("config"), "{}", out.stderr);
    for args in [
        &["--config", "/nonexistent/qhhoj/config.toml"][..],
        &["selftest", "--config", "/nonexistent/qhhoj/config.toml"],
    ] {
        let out = bin(args);
        assert_eq!(out.code, Some(2), "{args:?}: {}", out.stderr);
        assert!(!out.stderr.is_empty(), "{args:?}");
    }
}

#[cfg(feature = "isolate")]
mod with_isolate {
    use super::{Finished, run_within};
    use std::time::Duration;

    const ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
    const LIMIT: Duration = Duration::from_secs(180);

    fn box_base(offset: u32) -> String {
        let base: u32 = std::env::var("JUDGE_TEST_BOX_BASE").ok().and_then(|v| v.parse().ok()).unwrap_or(500);
        (base + offset).to_string()
    }

    /// A data root holding `aplusb` (`1 2` -> `3`) and a source file `sol` with the given text.
    fn fixture(source_name: &str, source: &str) -> (tempfile::TempDir, String, String) {
        let dir = tempfile::tempdir().unwrap();
        let problem = dir.path().join("aplusb");
        std::fs::create_dir_all(&problem).unwrap();
        std::fs::write(problem.join("init.yml"), "test_cases:\n- {in: 1.in, out: 1.out, points: 100}\n")
            .unwrap();
        std::fs::write(problem.join("1.in"), "1 2\n").unwrap();
        std::fs::write(problem.join("1.out"), "3\n").unwrap();
        let file = dir.path().join(source_name);
        std::fs::write(&file, source).unwrap();
        (dir, problem.display().to_string(), file.display().to_string())
    }

    fn run_source(lang: &str, source_name: &str, source: &str, offset: u32) -> Finished {
        let (_dir, problem, file) = fixture(source_name, source);
        let languages = format!("{ROOT}/languages.toml");
        let testlib = format!("{ROOT}/assets/testlib");
        let base = box_base(offset);
        let args =
            ["run", "--problem", &problem, "--lang", lang, "--languages", &languages, "--testlib", &testlib];
        run_within(&[&args[..], &["--box-base", &base, &file]].concat(), LIMIT)
    }

    #[test]
    fn run_judges_a_local_problem() {
        let out = run_source("PY3", "sol.py", "a, b = map(int, input().split())\nprint(a + b)\n", 20);
        assert_eq!(out.code, Some(0), "{}{}", out.stdout, out.stderr);
        assert!(out.stdout.contains("#1 AC"), "{}", out.stdout);
        assert!(out.stdout.contains("graded"), "{}", out.stdout);
    }

    #[test]
    fn run_prints_a_wrong_answer_and_still_exits_0() {
        let out = run_source("PY3", "sol.py", "print(0)\n", 22);
        assert_eq!(out.code, Some(0), "{}{}", out.stdout, out.stderr);
        assert!(out.stdout.contains("#1 WA"), "{}", out.stdout);
    }

    #[test]
    fn run_exits_1_when_the_problem_cannot_be_judged() {
        let (_dir, _problem, file) = fixture("sol.py", "print(3)\n");
        let languages = format!("{ROOT}/languages.toml");
        let testlib = format!("{ROOT}/assets/testlib");
        let base = box_base(30);
        let args = ["run", "--problem", "/nonexistent/aplusb", "--lang", "PY3", "--languages", &languages];
        let out =
            run_within(&[&args[..], &["--testlib", &testlib, "--box-base", &base, &file]].concat(), LIMIT);
        assert_eq!(out.code, Some(1), "{}{}", out.stdout, out.stderr);
        assert!(!out.stderr.is_empty());
    }

    #[test]
    fn run_exits_1_on_a_compile_error() {
        let out = run_source("C", "sol.c", "int main(void) { return missing_symbol; }\n", 24);
        assert_eq!(out.code, Some(1), "{}{}", out.stdout, out.stderr);
        assert!(out.stdout.contains("compile error"), "{}", out.stdout);
    }

    /// A one-row manifest: PY3 a+b expected to get `expect`.
    fn selftest_expecting(expect: &str) -> Finished {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("PY3")).unwrap();
        std::fs::copy(format!("{ROOT}/tests/programs/PY3/ac.py"), dir.path().join("PY3/ac.py")).unwrap();
        let row = format!("rows = [{{ lang = \"PY3\", file = \"PY3/ac.py\", expect = \"{expect}\" }}]\n");
        std::fs::write(dir.path().join("manifest.toml"), row).unwrap();
        let (languages, testlib) = (format!("{ROOT}/languages.toml"), format!("{ROOT}/assets/testlib"));
        let programs = dir.path().to_str().unwrap();
        let args = ["selftest", "--config", "/nonexistent", "--languages", &languages, "--testlib", &testlib];
        let base = box_base(26);
        run_within(&[&args[..], &["--programs", programs, "--box-base", &base]].concat(), LIMIT)
    }

    #[test]
    fn selftest_exits_0_when_verdicts_match_the_manifest_and_1_on_a_mismatch() {
        let matching = selftest_expecting("AC");
        assert_eq!(matching.code, Some(0), "{}{}", matching.stdout, matching.stderr);
        let mismatching = selftest_expecting("WA");
        assert_eq!(mismatching.code, Some(1), "{}{}", mismatching.stdout, mismatching.stderr);
        assert!(mismatching.stdout.contains("MISMATCH"), "{}", mismatching.stdout);
    }
}

#[test]
fn generate_with_an_unreadable_plan_exits_1_naming_the_plan_before_touching_isolate() {
    let out = bin(&["generate", "--bundle", "/b.zip", "--plan", "/nonexistent/plan.json", "--out", "/o.zip"]);
    assert_eq!(out.code, Some(1), "{}", out.stderr);
    assert!(out.stderr.contains("plan /nonexistent/plan.json"), "{}", out.stderr);
}
