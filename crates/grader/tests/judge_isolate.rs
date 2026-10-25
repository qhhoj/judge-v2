#![cfg(feature = "isolate")]
//! Whole submissions through `judge_submission`: real C++ and Python programs, problems in a temp data root.
mod common;

use common::{artefacts, sandbox};
use judge_grader::case_judges::UNREADABLE_ZIP;
use judge_grader::judge::{
    GraderDefaults, JudgeEnv, OUTPUT_REQUIRED, Outcome, RetryReason, Submission, SubmissionSource,
    judge_submission,
};
use judge_grader::report::{CaseReport, Reporter};
use judge_grader::signature::SIGNATURE_LANGUAGES;
use judge_problem::data_version::{FsStamps, WaitPolicy};
use judge_problem::dir_cache::DirCache;
use judge_problem::resolve::{ResolvedProblem, Resolver};
use judge_problem::verdict::Status;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

#[derive(Default)]
struct Recorder {
    log: Option<String>,
    started: Option<(bool, u32)>,
    cases: Vec<CaseReport>,
    /// Runs after each reported case, with the number of cases reported so far.
    after_case: Option<Box<dyn FnMut(usize)>>,
}

impl Reporter for Recorder {
    fn compiled(&mut self, log: &str) {
        self.log = Some(log.into());
    }
    fn started(&mut self, pretested: bool, case_count: u32) {
        self.started = Some((pretested, case_count));
    }
    fn case(&mut self, report: CaseReport) {
        self.cases.push(report);
        if let Some(hook) = &mut self.after_case {
            hook(self.cases.len());
        }
    }
    fn before_case(&mut self) -> bool {
        true
    }
}

impl Recorder {
    fn shape(&self) -> Vec<(Status, f64)> {
        self.cases.iter().map(|c| (c.status, c.points)).collect()
    }
}

/// A temp data root, a resolver over it, and one worker's grading environment.
struct Bench {
    _dirs: Vec<tempfile::TempDir>,
    root: PathBuf,
    resolver: Resolver,
    env: JudgeEnv,
}

fn bench(offset: u32) -> Bench {
    let dirs: Vec<_> = (0..5).map(|_| tempfile::tempdir().unwrap()).collect();
    let root = dirs[0].path().to_path_buf();
    let resolver = Resolver {
        root: root.clone(),
        cache: Arc::new(DirCache::new(dirs[1].path().into(), 1 << 30)),
        wait: WaitPolicy { timeout: Duration::from_millis(100), poll: Duration::from_millis(20) },
        zip: Default::default(),
        stamps: Arc::new(FsStamps),
    };
    let env = JudgeEnv {
        solution: sandbox(offset, dirs[2].path()),
        helper: sandbox(offset + 1, dirs[2].path()),
        artefacts: Arc::new(artefacts(dirs[3].path())),
        stamps: Arc::new(FsStamps),
        scratch: dirs[4].path().into(),
        defaults: GraderDefaults {
            default_output_limit: 1 << 20,
            default_output_prefix: 64,
            jvm_reserve_kib: 131_072,
        },
    };
    Bench { _dirs: dirs, root, resolver, env }
}

impl Bench {
    fn write(&self, code: &str, files: &[(&str, &[u8])]) -> PathBuf {
        let dir = self.root.join(code);
        std::fs::create_dir_all(&dir).unwrap();
        for (name, bytes) in files {
            std::fs::write(dir.join(name), bytes).unwrap();
        }
        dir
    }

    fn resolve(&self, code: &str, assigned: u64) -> ResolvedProblem {
        self.resolver.resolve(code, assigned).unwrap()
    }

    fn judge(&mut self, problem: &ResolvedProblem, submission: &Submission, rec: &mut Recorder) -> Outcome {
        judge_submission(&mut self.env, problem, submission, rec)
    }

    /// Resolves `code` unstamped and grades `source` with the default options.
    fn run(&mut self, code: &str, lang: &str, source: &str) -> (Outcome, Recorder) {
        let problem = self.resolve(code, 0);
        let mut rec = Recorder::default();
        let outcome = self.judge(&problem, &submission(lang, source.as_bytes()), &mut rec);
        (outcome, rec)
    }
}

fn submission(lang: &str, source: &[u8]) -> Submission {
    Submission {
        language_key: lang.into(),
        source: SubmissionSource::Inline(source.to_vec()),
        time_limit_s: 1.0,
        memory_kib: 262_144,
        short_circuit: false,
        pretests_only: false,
    }
}

const APLUSB_INIT: &str =
    "test_cases:\n- {in: 1.in, out: 1.out, points: 50}\n- {in: 2.in, out: 2.out, points: 50}\n";
const APLUSB: [(&str, &[u8]); 5] = [
    ("init.yml", APLUSB_INIT.as_bytes()),
    ("1.in", b"1 2\n"),
    ("1.out", b"3\n"),
    ("2.in", b"10 20\n"),
    ("2.out", b"30\n"),
];
const CPP_SUM: &str =
    "#include <cstdio>\nint main(){long long a,b;scanf(\"%lld%lld\",&a,&b);printf(\"%lld\\n\",a+b);}";
const PY_SUM: &str = "a, b = map(int, input().split())\nprint(0 if a == 7 else a + b)\n";

#[test]
fn accepted_cpp() {
    let mut b = bench(0);
    b.write("aplusb", &APLUSB);
    let (outcome, rec) = b.run("aplusb", "CPP17", CPP_SUM);
    assert_eq!(outcome, Outcome::Graded);
    assert!(rec.log.is_some());
    assert_eq!(rec.started, Some((false, 2)));
    assert_eq!(rec.shape(), [(Status::Ac, 50.0), (Status::Ac, 50.0)]);
    assert!(rec.cases[0].output_prefix.starts_with('3'));
    assert!(rec.cases.iter().all(|c| c.memory_kib > 0));
}

#[test]
fn scratch_and_unknown_languages_are_refused_without_grading() {
    let mut b = bench(6);
    b.write("aplusb", &APLUSB);
    for lang in ["SCRATCH", "COBOL"] {
        let (outcome, rec) = b.run("aplusb", lang, "x");
        assert_eq!(outcome, Outcome::Retry(RetryReason::RuntimeMissing), "{lang}");
        assert!(rec.started.is_none() && rec.cases.is_empty());
    }
}

#[test]
fn an_unsupported_problem_is_refused_before_compiling() {
    let mut b = bench(8);
    b.write(
        "gen",
        &[("init.yml", b"generator: gen.cpp\ntest_cases:\n- {in: 1.in, out: 1.out, points: 1}\n")],
    );
    let (outcome, rec) = b.run("gen", "CPP17", CPP_SUM);
    assert_eq!(outcome, Outcome::Retry(RetryReason::Unsupported));
    assert!(rec.log.is_none() && rec.cases.is_empty());
}

const BATCHED_INIT: &str = "test_cases:\n\
- points: 40\n  batched:\n  - {in: a1.in, out: a1.out}\n  - {in: a2.in, out: a2.out}\n  - {in: a3.in, out: a3.out}\n\
- points: 30\n  batched:\n  - {in: b1.in, out: b1.out}\n  - {in: b2.in, out: b2.out}\n\
- {in: c.in, out: c.out, points: 30}\n";

/// Inputs `x 1` (answer x + 1); the Python solution answers 0 for `7 1`, the input of case a2.
fn batched_problem(b: &Bench) {
    let mut files: Vec<(String, Vec<u8>)> = vec![("init.yml".into(), BATCHED_INIT.into())];
    for (name, x) in [("a1", 1), ("a2", 7), ("a3", 3), ("b1", 4), ("b2", 5), ("c", 6)] {
        files.push((format!("{name}.in"), format!("{x} 1\n").into()));
        files.push((format!("{name}.out"), format!("{}\n", x + 1).into()));
    }
    let borrowed: Vec<(&str, &[u8])> = files.iter().map(|(n, b)| (n.as_str(), b.as_slice())).collect();
    b.write("batched", &borrowed);
}

#[test]
fn a_failed_batch_case_skips_its_batch_and_later_batches_still_score() {
    let mut b = bench(10);
    batched_problem(&b);
    let (outcome, rec) = b.run("batched", "PY3", PY_SUM);
    assert_eq!(outcome, Outcome::Graded);
    let shape: Vec<_> = rec.cases.iter().map(|c| (c.status, c.points, c.batch)).collect();
    assert_eq!(
        shape,
        [
            (Status::Ac, 40.0, Some(1)),
            (Status::Wa, 0.0, Some(1)),
            (Status::Sc, 0.0, Some(1)),
            (Status::Ac, 30.0, Some(2)),
            (Status::Ac, 30.0, Some(2)),
            (Status::Ac, 30.0, None),
        ]
    );
    let problem = b.resolve("batched", 0);
    let mut short = submission("PY3", PY_SUM.as_bytes());
    short.short_circuit = true;
    let mut rec = Recorder::default();
    b.judge(&problem, &short, &mut rec);
    let statuses: Vec<_> = rec.cases.iter().map(|c| c.status).collect();
    assert_eq!(statuses, [Status::Ac, Status::Wa, Status::Sc, Status::Sc, Status::Sc, Status::Sc]);
}

#[test]
fn a_pretest_run_grades_only_the_pretests_and_a_full_run_stops_at_a_failed_pretest() {
    let mut b = bench(12);
    let init = "pretest_test_cases:\n- {in: p.in, out: p.out, points: 10}\ntest_cases:\n- {in: 1.in, out: 1.out, points: 50}\n- {in: 2.in, out: 2.out, points: 50}\n";
    let mut files = APLUSB.to_vec();
    files[0] = ("init.yml", init.as_bytes());
    files.extend([("p.in", &b"7 1\n"[..]), ("p.out", b"8\n")]);
    b.write("pre", &files);
    let problem = b.resolve("pre", 0);
    let mut pretests = submission("PY3", PY_SUM.as_bytes());
    pretests.pretests_only = true;
    let mut rec = Recorder::default();
    assert_eq!(b.judge(&problem, &pretests, &mut rec), Outcome::Graded);
    assert_eq!(rec.started, Some((true, 1)));
    assert_eq!(rec.shape(), [(Status::Wa, 0.0)], "the pretest input 7 1 is answered 0");
    // DMOJ's full run: the pretest first (worth 0 there), and failing it skips the tests.
    let (_, rec) = b.run("pre", "PY3", PY_SUM);
    assert_eq!(rec.started, Some((false, 3)));
    assert_eq!(rec.shape(), [(Status::Wa, 0.0), (Status::Sc, 0.0), (Status::Sc, 0.0)]);
}

const INTERACTOR: &str = r#"#include "testlib.h"
int main(int argc, char* argv[]) {
    registerInteraction(argc, argv);
    int secret = inf.readInt(), asked = 0;
    while (true) {
        int guess = ouf.readInt();
        if (++asked > 40) quitf(_wa, "too many guesses");
        if (guess == secret) { std::cout << "=" << std::endl; quitf(_ok, "found in %d", asked); }
        std::cout << (guess < secret ? "<" : ">") << std::endl;
    }
}
"#;

const BINARY_SEARCH: &str = r#"#include <iostream>
#include <string>
int main() {
    int lo = 1, hi = 1000;
    while (true) {
        int m = (lo + hi) / 2;
        std::cout << m << std::endl;
        std::string r; std::cin >> r;
        if (r == "=") return 0;
        if (r == "<") lo = m + 1; else hi = m - 1;
    }
}
"#;

#[test]
fn interactive_problems_are_judged_by_the_interactor() {
    let mut b = bench(14);
    let init = "checker: {name: bridged, args: {files: missing.cpp, lang: CPP17, type: testlib}}\n\
                interactive: {files: inter.cpp, type: testlib, lang: CPP17}\n\
                test_cases:\n- {in: 1.in, out: 1.out, points: 60, checker: identical}\n- {in: 2.in, out: 2.out, points: 40}\n";
    let files: [(&str, &[u8]); 6] = [
        ("init.yml", init.as_bytes()),
        ("inter.cpp", INTERACTOR.as_bytes()),
        ("1.in", b"377\n"),
        ("1.out", b""),
        ("2.in", b"1\n"),
        ("2.out", b""),
    ];
    b.write("guess", &files);
    let (outcome, rec) = b.run("guess", "CPP17", BINARY_SEARCH);
    assert_eq!(outcome, Outcome::Graded, "the named checkers are never compiled or compared");
    assert_eq!(rec.shape(), [(Status::Ac, 60.0), (Status::Ac, 40.0)]);
    let always_one = "#include <iostream>\n#include <string>\nint main(){ std::string r; while (true) { std::cout << 1 << std::endl; if (!(std::cin >> r)) return 0; } }\n";
    let (_, rec) = b.run("guess", "CPP17", always_one);
    assert_eq!(rec.shape(), [(Status::Wa, 0.0), (Status::Ac, 40.0)]);
    assert_eq!(rec.cases[0].feedback, "too many guesses");
    let never_reads = "#include <iostream>\nint main(){ while (true) std::cout << 1 << std::endl; }\n";
    let (_, rec) = b.run("guess", "CPP17", never_reads);
    assert_eq!(rec.cases[0].status, Status::Wa, "SIGPIPE after the interactor quit: {:?}", rec.cases[0]);
    assert_eq!(rec.cases[0].feedback, "too many guesses");
    let (outcome, _) = b.run("guess", "TEXT", "1\n");
    assert!(matches!(outcome, Outcome::CompileError(_)), "TEXT cannot play: {outcome:?}");
}

/// The guessing game, but the interactor only reports the guess count on `tout`; the answer file holds the
/// budget the problem's checker enforces.
const COUNTING_INTERACTOR: &str = r#"#include "testlib.h"
int main(int argc, char* argv[]) {
    registerInteraction(argc, argv);
    int secret = inf.readInt(), asked = 0;
    while (true) {
        int guess = ouf.readInt();
        if (++asked > 40) quitf(_wa, "too many guesses");
        if (guess == secret) {
            std::cout << "=" << std::endl;
            tout << asked << std::endl;
            quitf(_ok, "found in %d", asked);
        }
        std::cout << (guess < secret ? "<" : ">") << std::endl;
    }
}
"#;

/// Includes the package's own testlib.h and a header shipped beside it, not the judge's vendored copy.
const BUDGET_CHECKER: &str = r#"#include "testlib.h"
#include "budget.h"
#ifndef PACKAGE_TESTLIB
#error "compiled against the vendored testlib, not the package's"
#endif
int main(int argc, char* argv[]) {
    registerTestlibCmd(argc, argv);
    int budget = ans.readInt(), asked = ouf.readInt();
    if (!within(asked, budget)) quitf(_wa, "%d guesses, budget %d", asked, budget);
    quitf(_ok, "%d guesses", asked);
}
"#;

/// Secret 377 (10 guesses by binary search, budget 10) and secret 1 (9 guesses, budget 3). `checker` is the
/// `interactive` entry's tail; the top-level `standard` checker, which the backend always writes, must be ignored.
fn counting_problem(b: &Bench, code: &str, checker: &str) {
    counting_problem_burning(b, code, checker, false);
}

/// `counting_problem`; with `burn`, the interactor and the checker each use at least `BURN_S` of CPU.
fn counting_problem_burning(b: &Bench, code: &str, checker: &str, burn: bool) {
    let burning = |source: &str| if burn { burn_cpu(source) } else { source.to_string() };
    let init = format!(
        "checker: standard\ninteractive: {{files: count.cpp, type: testlib, lang: CPP17{checker}}}\n\
         test_cases:\n- {{in: 1.in, out: 1.out, points: 60}}\n- {{in: 2.in, out: 2.out, points: 40}}\n"
    );
    let vendored = std::fs::read(common::testlib_dir().join("testlib.h")).unwrap();
    let package_testlib = [b"#define PACKAGE_TESTLIB 1\n".as_slice(), &vendored].concat();
    let (interactor, budget_checker) = (burning(COUNTING_INTERACTOR), burning(BUDGET_CHECKER));
    let files: [(&str, &[u8]); 9] = [
        ("init.yml", init.as_bytes()),
        ("count.cpp", interactor.as_bytes()),
        ("check.cpp", budget_checker.as_bytes()),
        ("testlib.h", &package_testlib),
        ("budget.h", b"inline bool within(int asked, int budget) { return asked <= budget; }\n"),
        ("1.in", b"377\n"),
        ("1.out", b"10\n"),
        ("2.in", b"1\n"),
        ("2.out", b"3\n"),
    ];
    b.write(code, &files);
}

#[test]
fn an_interactive_checker_reads_tout_once_the_interactor_accepts() {
    let mut b = bench(84);
    let checker = ", checker: {name: bridged, args: {files: [check.cpp, testlib.h, budget.h], lang: CPP17, type: testlib}}";
    counting_problem(&b, "counted", checker);
    let (outcome, rec) = b.run("counted", "CPP17", BINARY_SEARCH);
    assert_eq!(outcome, Outcome::Graded);
    assert_eq!(rec.shape(), [(Status::Ac, 60.0), (Status::Wa, 0.0)], "the interactor accepted both");
    assert_eq!(rec.cases[0].feedback, "10 guesses");
    assert_eq!(rec.cases[1].feedback, "9 guesses, budget 3");
    let always_one = "#include <iostream>\n#include <string>\nint main(){ std::string r; while (true) { std::cout << 1 << std::endl; if (!(std::cin >> r)) return 0; } }\n";
    let (_, rec) = b.run("counted", "CPP17", always_one);
    assert_eq!(rec.cases[0].status, Status::Wa);
    assert_eq!(rec.cases[0].feedback, "too many guesses", "a rejecting interactor is not overruled");
}

#[test]
fn an_interactive_problem_without_its_own_checker_is_judged_by_the_interactor_alone() {
    let mut b = bench(86);
    counting_problem(&b, "uncounted", "");
    let (outcome, rec) = b.run("uncounted", "CPP17", BINARY_SEARCH);
    assert_eq!(outcome, Outcome::Graded);
    assert_eq!(rec.shape(), [(Status::Ac, 60.0), (Status::Ac, 40.0)], "no checker reads the budget");
    assert_eq!(rec.cases[1].feedback, "found in 9");
}

const SIGNATURE_INIT: &str = "test_cases:\n- {in: 1.in, out: 1.out, points: 1}\nsignature_grader: {entry: grader.cpp, header: sol.h}\n";
const SIGNATURE_GRADER: &str = "#include <cstdio>\n#include \"sol.h\"\nint main(){long long a,b;std::scanf(\"%lld %lld\",&a,&b);std::printf(\"%lld\\n\",add(a,b));}\n";

#[test]
fn signature_problems_link_c_plus_plus_and_refuse_python() {
    let mut b = bench(16);
    let files: [(&str, &[u8]); 5] = [
        ("init.yml", SIGNATURE_INIT.as_bytes()),
        ("sol.h", b"long long add(long long a, long long b);\n"),
        ("grader.cpp", SIGNATURE_GRADER.as_bytes()),
        ("1.in", b"1 2\n"),
        ("1.out", b"3\n"),
    ];
    b.write("sig", &files);
    let (outcome, rec) = b.run("sig", "CPP17", "long long add(long long a, long long b) { return a + b; }\n");
    assert_eq!(outcome, Outcome::Graded);
    assert_eq!(rec.shape(), [(Status::Ac, 1.0)]);
    let (outcome, rec) = b.run("sig", "PY3", "def add(a, b):\n    return a + b\n");
    assert_eq!(outcome, Outcome::CompileError(SIGNATURE_LANGUAGES.into()));
    assert!(rec.cases.is_empty() && rec.log.is_none());
}

const CHECKER: &str = r#"#include "testlib.h"
int main(int argc, char* argv[]) {
    registerTestlibCmd(argc, argv);
    int want = ans.readInt(), got = ouf.readInt();
    if (got == want) quitf(_ok, "correct");
    if (got == want + 1) quitp(20, "close");
    quitf(_wa, "expected %d, found %d", want, got);
}
"#;

/// testlib `points N` is N points of the case (the old judge's reading), or N% with the percentage flag.
#[test]
fn a_bridged_checker_grades_partial_points_as_case_points_or_a_percentage() {
    let mut b = bench(18);
    let off_by_one = "a, b = map(int, input().split())\nprint(a + b + (1 if a == 10 else 0))\n";
    let modes = [("points", "", 20.0), ("percent", ", treat_checker_points_as_percentage: true", 10.0)];
    for (code, flag, partial) in modes {
        let init = format!(
            "checker: {{name: bridged, args: {{files: check.cpp, lang: CPP17, type: testlib{flag}}}}}\n\
             test_cases:\n- {{in: 1.in, out: 1.out, points: 50}}\n- {{in: 2.in, out: 2.out, points: 50}}\n"
        );
        let mut files = APLUSB.to_vec();
        files[0] = ("init.yml", init.as_bytes());
        files.push(("check.cpp", CHECKER.as_bytes()));
        b.write(code, &files);
        let (outcome, rec) = b.run(code, "PY3", off_by_one);
        assert_eq!(outcome, Outcome::Graded);
        assert_eq!(rec.shape(), [(Status::Ac, 50.0), (Status::Ac, partial)], "{code}");
    }
}

/// `CHECKER`, but an answer off by one earns `quitp(0)`.
const ZERO_POINT_CHECKER: &str = r#"#include "testlib.h"
int main(int argc, char* argv[]) {
    registerTestlibCmd(argc, argv);
    int want = ans.readInt(), got = ouf.readInt();
    if (got == want) quitf(_ok, "correct");
    if (got == want + 1) quitp(0, "close but worth nothing");
    quitf(_wa, "expected %d, found %d", want, got);
}
"#;

#[test]
fn zero_checker_points_are_wa_and_a_full_run_stops_at_such_a_pretest() {
    let mut b = bench(88);
    let init = "checker: {name: bridged, args: {files: check.cpp, lang: CPP17, type: testlib}}\n\
                pretest_test_cases:\n- {in: p.in, out: p.out, points: 10}\n\
                test_cases:\n- {in: 1.in, out: 1.out, points: 50}\n- {in: 2.in, out: 2.out, points: 50}\n";
    let mut files = APLUSB.to_vec();
    files[0] = ("init.yml", init.as_bytes());
    files.extend([
        ("p.in", &b"10 1\n"[..]),
        ("p.out", b"11\n"),
        ("check.cpp", ZERO_POINT_CHECKER.as_bytes()),
    ]);
    b.write("zero", &files);
    let off_by_one = "a, b = map(int, input().split())\nprint(a + b + (1 if a == 10 else 0))\n";
    let (outcome, rec) = b.run("zero", "PY3", off_by_one);
    assert_eq!(outcome, Outcome::Graded);
    assert_eq!(rec.shape(), [(Status::Wa, 0.0), (Status::Sc, 0.0), (Status::Sc, 0.0)]);
    let problem = b.resolve("zero", 0);
    let mut rec = Recorder::default();
    let mut pretests = submission("PY3", off_by_one.as_bytes());
    pretests.pretests_only = true;
    assert_eq!(b.judge(&problem, &pretests, &mut rec), Outcome::Graded);
    assert_eq!(rec.shape(), [(Status::Wa, 0.0)], "points 0 on a 10-point case");
}

/// The guessing game, but finding the secret earns `quitp(50)`.
const HALF_POINTS_INTERACTOR: &str = r#"#include "testlib.h"
int main(int argc, char* argv[]) {
    registerInteraction(argc, argv);
    int secret = inf.readInt(), asked = 0;
    while (true) {
        int guess = ouf.readInt();
        if (++asked > 40) quitf(_wa, "too many guesses");
        if (guess == secret) { std::cout << "=" << std::endl; quitp(50, "found in %d", asked); }
        std::cout << (guess < secret ? "<" : ">") << std::endl;
    }
}
"#;

/// An interactor's `points N` is N points of the case, or N% with `treat_checker_points_as_percentage`.
#[test]
fn an_interactor_grades_partial_points_as_case_points_or_a_percentage() {
    let mut b = bench(90);
    let modes = [
        ("halfpoints", "", 100, 50.0),
        ("halfpercent", ", treat_checker_points_as_percentage: true", 40, 20.0),
    ];
    for (code, flag, case_points, points) in modes {
        let init = format!(
            "interactive: {{files: inter.cpp, type: testlib, lang: CPP17{flag}}}\n\
             test_cases:\n- {{in: 1.in, out: 1.out, points: {case_points}}}\n"
        );
        let files: [(&str, &[u8]); 4] = [
            ("init.yml", init.as_bytes()),
            ("inter.cpp", HALF_POINTS_INTERACTOR.as_bytes()),
            ("1.in", b"377\n"),
            ("1.out", b""),
        ];
        b.write(code, &files);
        let (outcome, rec) = b.run(code, "CPP17", BINARY_SEARCH);
        assert_eq!(outcome, Outcome::Graded, "{code}");
        assert_eq!(rec.shape(), [(Status::Ac, points)], "{code}");
    }
}

#[test]
fn text_language_answers_every_case_with_the_source() {
    let mut b = bench(20);
    b.write("aplusb", &APLUSB);
    let (outcome, rec) = b.run("aplusb", "TEXT", "3\n");
    assert_eq!(outcome, Outcome::Graded);
    assert!(rec.log.is_none(), "nothing compiles");
    assert_eq!(rec.cases.iter().map(|c| c.status).collect::<Vec<_>>(), [Status::Ac, Status::Wa]);
}

fn zip_of(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(&mut bytes));
    for (name, data) in entries {
        writer.start_file(*name, zip::write::SimpleFileOptions::default()).unwrap();
        writer.write_all(data).unwrap();
    }
    writer.finish().unwrap();
    bytes
}

#[test]
fn output_only_zips_are_graded_per_entry_and_a_broken_zip_is_wa() {
    let mut b = bench(22);
    let problem = output_only_problem(&b);
    let zip = b.root.join("submission.zip");
    std::fs::write(&zip, zip_of(&[("1.out", b"3\n")])).unwrap();
    let mut from_file = submission("OUTPUT", b"");
    from_file.source = SubmissionSource::File(zip);
    let mut rec = Recorder::default();
    assert_eq!(b.judge(&problem, &from_file, &mut rec), Outcome::Graded);
    assert_eq!(rec.shape(), [(Status::Ac, 50.0), (Status::Wa, 0.0)], "a missing entry is an empty answer");
    let mut rec = Recorder::default();
    assert_eq!(b.judge(&problem, &submission("OUTPUT", b"PK\x05\x06 broken"), &mut rec), Outcome::Graded);
    assert_eq!(rec.shape(), [(Status::Wa, 0.0), (Status::Wa, 0.0)]);
    assert!(rec.cases.iter().all(|c| c.feedback == UNREADABLE_ZIP));
}

fn output_only_problem(b: &Bench) -> ResolvedProblem {
    let mut files = APLUSB.to_vec();
    let init = format!("output_only: true\n{APLUSB_INIT}");
    files[0] = ("init.yml", init.as_bytes());
    b.write("outputs", &files);
    b.resolve("outputs", 0)
}

#[test]
fn a_bad_zip_entry_is_wa_and_a_program_is_refused_on_output_only_problems() {
    let mut b = bench(28);
    let problem = output_only_problem(&b);
    let mut bad_entry = stored_zip(&[("1.out", b"3\n"), ("2.out", b"30\n")]);
    let name = position(&bad_entry, b"2.out", 0);
    let data = position(&bad_entry, b"30\n", name);
    bad_entry[data] = b'4'; // the entry's checksum no longer matches
    let mut rec = Recorder::default();
    assert_eq!(b.judge(&problem, &submission("OUTPUT", &bad_entry), &mut rec), Outcome::Graded);
    assert_eq!(rec.shape(), [(Status::Ac, 50.0), (Status::Wa, 0.0)]);
    assert_eq!(rec.cases[1].feedback, UNREADABLE_ZIP, "a bad entry is the submission's fault, not IE");
    let (outcome, rec) = b.run("outputs", "CPP17", CPP_SUM);
    assert_eq!(outcome, Outcome::CompileError(OUTPUT_REQUIRED.into()), "a program is not graded as text");
    assert!(rec.cases.is_empty());
}

fn stored_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(&mut bytes));
    let stored = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    for (name, data) in entries {
        writer.start_file(*name, stored).unwrap();
        writer.write_all(data).unwrap();
    }
    writer.finish().unwrap();
    bytes
}

fn position(haystack: &[u8], needle: &[u8], from: usize) -> usize {
    from + haystack[from..].windows(needle.len()).position(|w| w == needle).unwrap()
}

fn restamp(dir: &Path, version: u64) {
    let temp = dir.join(".data-version.tmp");
    std::fs::write(&temp, format!("{version}\n")).unwrap();
    std::fs::rename(temp, dir.join(".data-version")).unwrap();
}

#[test]
fn a_stamp_that_moves_while_loose_files_are_graded_is_retry_data_unavailable() {
    let mut b = bench(24);
    let dir = b.write("aplusb", &APLUSB);
    restamp(&dir, 1);
    let problem = b.resolve("aplusb", 1);
    assert_eq!(problem.data_version_used, 1);
    let mut rec = Recorder::default();
    let sum = submission("CPP17", CPP_SUM.as_bytes());
    assert_eq!(b.judge(&problem, &sum, &mut rec), Outcome::Graded, "an unmoved stamp keeps the result");
    let moved = dir.clone();
    let mut rec = Recorder {
        after_case: Some(Box::new(move |n| {
            if n == 1 {
                restamp(&moved, 2)
            }
        })),
        ..Default::default()
    };
    assert_eq!(b.judge(&problem, &sum, &mut rec), Outcome::Retry(RetryReason::DataUnavailable));
    assert_eq!(rec.cases.len(), 2, "grading finished before the stamp was read again");
}

#[test]
fn an_archive_problem_whose_stamp_moves_while_graded_is_retry_data_unavailable() {
    let mut b = bench(26);
    let archive = zip_of(&APLUSB[1..]);
    let dir = b.write(
        "zipped",
        &[("init.yml", format!("archive: tests.zip\n{APLUSB_INIT}").as_bytes()), ("tests.zip", &archive)],
    );
    restamp(&dir, 1);
    let problem = b.resolve("zipped", 1);
    let sum = submission("CPP17", CPP_SUM.as_bytes());
    assert_eq!(
        b.judge(&problem, &sum, &mut Recorder::default()),
        Outcome::Graded,
        "an unmoved stamp keeps the result"
    );
    let mut rec = Recorder { after_case: Some(Box::new(move |_| restamp(&dir, 2))), ..Default::default() };
    let outcome = b.judge(&problem, &sum, &mut rec);
    // Its checker, interactor and signature sources come from the data root, not the cache entry.
    assert_eq!(outcome, Outcome::Retry(RetryReason::DataUnavailable), "not kept as version 1");
    assert_eq!(rec.shape(), [(Status::Ac, 50.0), (Status::Ac, 50.0)]);
}

/// CPU every burning jury program uses at least, in seconds (`clock()` counts the process's own CPU).
const BURN_S: f64 = 0.2;

/// A testlib jury program that first spins until it has used `BURN_S` of CPU.
fn burn_cpu(source: &str) -> String {
    let spin = "static void burn() { while (clock() < CLOCKS_PER_SEC / 5); }\nint main(";
    source.replacen("int main(", spin, 1).replacen("{\n", "{\n    burn();\n", 1)
}

#[test]
fn a_bridged_checker_reports_its_cpu_as_jury_time_and_a_builtin_checker_reports_none() {
    let mut b = bench(92);
    let init = "checker: {name: bridged, args: {files: check.cpp, lang: CPP17, type: testlib}}\n\
                test_cases:\n- {in: 1.in, out: 1.out, points: 50}\n- {in: 2.in, out: 2.out, points: 50}\n";
    let mut files = APLUSB.to_vec();
    files[0] = ("init.yml", init.as_bytes());
    let burning = burn_cpu(CHECKER);
    files.push(("check.cpp", burning.as_bytes()));
    b.write("burningchecker", &files);
    b.write("builtinchecker", &APLUSB);
    let (_, custom) = b.run("burningchecker", "CPP17", CPP_SUM);
    let (_, builtin) = b.run("builtinchecker", "CPP17", CPP_SUM);
    assert_eq!(custom.shape(), builtin.shape(), "the verdicts do not change");
    assert!(custom.cases.iter().all(|c| c.jury_time_s >= BURN_S), "{:?}", custom.cases);
    assert!(builtin.cases.iter().all(|c| c.jury_time_s == 0.0), "{:?}", builtin.cases);
}

#[test]
fn an_interactive_case_reports_the_interactor_cpu_and_adds_the_checker_cpu() {
    let mut b = bench(94);
    let checker = ", checker: {name: bridged, args: {files: [check.cpp, testlib.h, budget.h], lang: CPP17, type: testlib}}";
    counting_problem_burning(&b, "burnalone", "", true);
    counting_problem_burning(&b, "burnchecked", checker, true);
    let (_, alone) = b.run("burnalone", "CPP17", BINARY_SEARCH);
    let (_, checked) = b.run("burnchecked", "CPP17", BINARY_SEARCH);
    let interactor_only = alone.cases[0].jury_time_s;
    assert!(interactor_only >= BURN_S, "the interactor's CPU is counted: {interactor_only}");
    let both = checked.cases[0].jury_time_s;
    assert!(both >= 2.0 * BURN_S, "interactor and checker are summed: {both}");
    assert_eq!(checked.cases[1].status, Status::Wa, "the budget checker rejected case 2");
    assert!(checked.cases[1].jury_time_s >= 2.0 * BURN_S, "a checker-rejected case still counts both");
}

#[test]
fn an_interactor_that_decides_alone_still_reports_its_cpu_when_the_solution_fails() {
    let mut b = bench(96);
    counting_problem_burning(&b, "burnfail", "", true);
    let never_reads = "#include <iostream>\nint main(){ while (true) std::cout << 1 << std::endl; }\n";
    let (_, rec) = b.run("burnfail", "CPP17", never_reads);
    assert_eq!(rec.cases[0].status, Status::Wa);
    assert!(rec.cases[0].jury_time_s >= BURN_S, "{:?}", rec.cases[0]);
}
