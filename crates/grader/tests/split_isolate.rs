#![cfg(feature = "isolate")]
//! Split signature end to end through `judge_submission`: the jury and the contestant in two boxes joined by
//! pipes. Proves an honest solution gets the same verdict and points as the one-process path on the same data
//! (over callbacks, refreshed `const&` returns of distinct objects and every grammar type, under C++03 and
//! C++17), that the contestant box holds only the contestant's program, that every verdict goes to the side
//! whose CPU or memory caused it, and that the jury build is never cached when it fails. Box offsets 0-9 (two
//! boxes per `bench`; the tests run one at a time). Run under `flock -o` (CONTRIBUTING.md); a channel run
//! refuses to start while the test process holds an inheritable fd.
mod common;

use common::{artefacts, language_or_skip, registry, sandbox};
use judge_grader::judge::{
    GraderDefaults, JudgeEnv, Outcome, Submission, SubmissionSource, judge_submission,
};
use judge_grader::report::{CaseReport, Reporter};
use judge_problem::data_version::{FsStamps, WaitPolicy};
use judge_problem::dir_cache::DirCache;
use judge_problem::resolve::{ResolvedProblem, Resolver};
use judge_problem::verdict::Status;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// Header: the contestant implements `play` and `report`; the grader implements the `ask` callbacks, `peek`,
/// which returns a `const&` refreshed between calls, and `part`, which returns a different object per half.
/// Every grammar type appears in a position.
const HEADER: &str = "#include <string>\n#include <vector>\n\
long long play(int n, const std::vector<long long>& hint);\n\
void report(const std::string& tag);\n\
long long ask(int i);\n\
bool more();\n\
const std::vector<long long>& peek();\n\
const std::vector<long long>& part(int half);\n";

/// Grader: reads n and the secret, lets the contestant query it through the callbacks, and prints 1 when the
/// contestant returns the sum and reported the right tag. `burn` spends a given amount of the grader's own CPU
/// (process CPU time, so the amount does not depend on the host's speed or load).
const GRADER: &str = "#include <cstdio>\n#include <ctime>\n#include <string>\n#include <vector>\n#include \"sol.h\"\n\
static void burn(double s){ std::clock_t end = std::clock() + (std::clock_t)(s * CLOCKS_PER_SEC); while (std::clock() < end) {} }\n\
static std::vector<long long> secret;\nstatic std::string seen;\n\
std::vector<long long> g_peek;\nstatic std::vector<long long> halves[2];\n\
long long ask(int i){ g_peek.push_back(secret[i]); return secret[i]; }\n\
bool more(){ return true; }\n\
const std::vector<long long>& peek(){ return g_peek; }\n\
const std::vector<long long>& part(int half){ return halves[half]; }\n\
void report(const std::string& tag){ seen = tag; }\n\
int main(){\n  int n; if(scanf(\"%d\", &n)!=1) return 2;\n  secret.resize(n);\n\
  long long total=0;\n  for(int i=0;i<n;i++){ scanf(\"%lld\", &secret[i]); total+=secret[i]; }\n\
  g_peek = secret;\n  halves[0].assign(1, 10);\n  halves[1].assign(2, 20);\n\
  std::vector<long long> hint(1, total);\n  long long got = play(n, hint);\n\
  printf(\"%d\\n\", (got==total && seen==\"ok\") ? 1 : 0);\n}\n";

/// Honest contestant: sums through the callbacks and reports the tag. It holds `peek`'s reference across the
/// next calls (it must see them), and the references `part` returns for both halves at once (each must keep
/// its own object, as in one process).
const HONEST: &str = "#include <string>\n#include <vector>\n#include \"sol.h\"\n\
long long play(int n, const std::vector<long long>& hint){\n  const std::vector<long long>& p = peek();\n\
  const std::vector<long long>& a = part(0);\n  const std::vector<long long>& b = part(1);\n\
  bool halves = a.size()==1 && a[0]==10 && b.size()==2 && b[1]==20 && &part(0)==&a;\n\
  long long s=0;\n  for(int i=0;i<n;i++) s+=ask(i);\n\
  bool ok = halves && more() && p.size()==(size_t)(2*n) && hint.size()==1;\n\
  report(\"ok\");\n  return ok ? s : -1;\n}\n";

/// Sums the case file `input` instead of calling `ask`.
const READS_INPUT: &str = "#include <cstdio>\n#include <string>\n#include \"sol.h\"\n\
long long play(int n, const std::vector<long long>&){\n  FILE* f=fopen(\"input\",\"r\");\n\
  if(!f) return -1;\n  int m; fscanf(f,\"%d\",&m);\n  long long s=0,x;\n  for(int i=0;i<n;i++){ fscanf(f,\"%lld\",&x); s+=x; }\n\
  fclose(f);\n  report(\"ok\");\n  return s;\n}\n";

/// Uses a variable the header does not declare.
const UNDECLARED: &str = "#include <vector>\n#include \"sol.h\"\nextern std::vector<long long> g_peek;\n\
long long play(int n, const std::vector<long long>& hint){ long long s=0; for(size_t i=0;i<g_peek.size();i++) s+=g_peek[i]; report(\"ok\"); (void)hint; return s; }\n";

#[derive(Default)]
struct Recorder {
    cases: Vec<CaseReport>,
    log: Option<String>,
}

impl Reporter for Recorder {
    fn compiled(&mut self, log: &str) {
        self.log = Some(log.into());
    }
    fn started(&mut self, _pretested: bool, _case_count: u32) {}
    fn case(&mut self, report: CaseReport) {
        self.cases.push(report);
    }
    fn before_case(&mut self) -> bool {
        true
    }
}

struct Bench {
    _dirs: Vec<tempfile::TempDir>,
    root: PathBuf,
    /// The artefact cache's directory.
    cache: PathBuf,
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
    let cache = dirs[3].path().to_path_buf();
    Bench { _dirs: dirs, root, cache, resolver, env }
}

fn write_grader(b: &Bench, code: &str, grader: &str, split: bool) {
    let init = format!(
        "test_cases:\n- {{in: 1.in, out: 1.out, points: 60}}\n- {{in: 2.in, out: 2.out, points: 40}}\n\
         signature_grader: {{entry: grader.cpp, header: sol.h, split: {split}}}\n"
    );
    let dir = b.root.join(code);
    std::fs::create_dir_all(&dir).unwrap();
    for (name, text) in [
        ("init.yml", init.as_str()),
        ("sol.h", HEADER),
        ("grader.cpp", grader),
        ("1.in", "3\n5 7 11\n"),
        ("1.out", "1\n"),
        ("2.in", "1\n42\n"),
        ("2.out", "1\n"),
    ] {
        std::fs::write(dir.join(name), text).unwrap();
    }
}

fn submission(lang: &str, source: &str, time_limit_s: f64, memory_kib: u64) -> Submission {
    Submission {
        language_key: lang.into(),
        source: SubmissionSource::Inline(source.as_bytes().to_vec()),
        time_limit_s,
        memory_kib,
        short_circuit: false,
        pretests_only: false,
    }
}

fn shape(rec: &Recorder) -> Vec<(Status, f64)> {
    rec.cases.iter().map(|c| (c.status, c.points)).collect()
}

/// Grades `source` in `lang` against `code` (written with the default grader and `split`); None after `skip:`
/// when the key is missing.
fn judge(
    b: &mut Bench,
    code: &str,
    (lang, split): (&str, bool),
    source: &str,
) -> Option<(Outcome, Recorder)> {
    judge_with(b, (code, GRADER), (lang, split), source)
}

/// The same with `grader` as the problem's entry.
fn judge_with(
    b: &mut Bench,
    (code, grader): (&str, &str),
    (lang, split): (&str, bool),
    source: &str,
) -> Option<(Outcome, Recorder)> {
    language_or_skip(&registry(), lang)?;
    if b.root.join(code).join("init.yml").exists() {
        std::fs::remove_dir_all(b.root.join(code)).unwrap();
    }
    write_grader(b, code, grader, split);
    let problem: ResolvedProblem = b.resolver.resolve(code, 0).unwrap();
    let mut rec = Recorder::default();
    let outcome = judge_submission(&mut b.env, &problem, &submission(lang, source, 1.0, 262_144), &mut rec);
    Some((outcome, rec))
}

const PASS: [(Status, f64); 2] = [(Status::Ac, 60.0), (Status::Ac, 40.0)];

#[test]
fn an_honest_solution_gets_the_same_verdict_and_points_split_or_not() {
    let mut b = bench(0);
    for key in ["CPP03", "CPP17"] {
        let Some((one_process, one_rec)) = judge(&mut b, "p", (key, false), HONEST) else { continue };
        let (split, split_rec) = judge(&mut b, "p", (key, true), HONEST).unwrap();
        assert_eq!(one_process, Outcome::Graded, "{key}");
        assert_eq!(split, Outcome::Graded, "{key}");
        assert_eq!(shape(&one_rec), PASS, "{key}");
        assert_eq!(shape(&split_rec), shape(&one_rec), "{key}: split matches one-process on the same data");
    }
}

/// The contestant box holds only the contestant's program: a solution that reads the case file finds none, and
/// each case is graded WA.
#[test]
fn split_contestant_box_holds_only_its_program() {
    let mut b = bench(2);
    let Some((split, rec)) = judge(&mut b, "p", ("CPP17", true), READS_INPUT) else { return };
    assert_eq!(split, Outcome::Graded);
    assert_eq!(shape(&rec), [(Status::Wa, 0.0), (Status::Wa, 0.0)]);
}

/// A name the header does not declare is not linked into the contestant's program: a compile error.
#[test]
fn a_solution_that_uses_a_name_outside_the_header_is_a_compile_error() {
    let mut b = bench(4);
    let Some((split, _)) = judge(&mut b, "p", ("CPP17", true), UNDECLARED) else { return };
    assert!(matches!(split, Outcome::CompileError(_)), "{split:?}");
}

/// The grader burns far more CPU and memory than the contestant's limits outside any callback; a correct
/// contestant still gets AC (its reported time stays tiny while the jury's CPU is large), so neither is charged
/// to it. A contestant that itself spins is TLE and one that itself allocates is MLE: each failure goes to the
/// box that caused it.
#[test]
fn time_and_memory_are_charged_to_the_box_that_spent_them() {
    let mut b = bench(6);
    let heavy_grader = GRADER.replace(
        "int n; if(scanf(\"%d\", &n)!=1) return 2;",
        "int n; if(scanf(\"%d\", &n)!=1) return 2;\n  { burn(0.3); std::vector<char> hog(200<<20, 1); if(hog[0]!=1) return 3; }",
    );
    let code = "heavy";
    std::fs::create_dir_all(b.root.join(code)).unwrap();
    if language_or_skip(&registry(), "CPP17").is_none() {
        return;
    }
    write_grader(&b, code, &heavy_grader, true);
    let honest = judge_written(&mut b, code, HONEST, 65_536);
    assert_eq!(honest.0, Outcome::Graded);
    assert_eq!(shape(&honest.1), PASS, "jury CPU and memory are not the contestant's");
    assert!(
        honest.1.cases.iter().all(|c| c.time_s < 0.3
            && c.jury_time_s > 0.25
            && c.jury_time_s > c.time_s * 3.0
            && c.memory_kib < 60_000),
        "the contestant stays small while the jury's CPU is large: {:?}",
        honest.1.cases
    );

    let spinner = "#include \"sol.h\"\nlong long play(int n, const std::vector<long long>&){ volatile long long z=0; for(;;) z++; return n+z; }\n";
    assert_eq!(status_of(judge_written(&mut b, code, spinner, 262_144)), Status::Tle);
    let hog = "#include <vector>\n#include \"sol.h\"\nlong long play(int n, const std::vector<long long>&){ std::vector<char> big(400<<20, 7); for(size_t i=0;i<big.size();i+=4096) big[i]++; return n + big[0]; }\n";
    assert_eq!(status_of(judge_written(&mut b, code, hog, 65_536)), Status::Mle);
}

/// CPU the grader spends in callbacks is the contestant's, as in one process: `ask` burns 0.5 s of CPU, so the
/// three calls of case 1 are over the 1 s TL and the one call of case 2 is not. The contestant box's own CPU
/// stays near zero, so only the charge makes case 1 TLE.
#[test]
fn cpu_the_grader_spends_in_callbacks_is_charged_to_the_contestant() {
    let mut b = bench(8);
    let slow_ask = GRADER.replace("long long ask(int i){", "long long ask(int i){ burn(0.5);");
    let Some((split, rec)) = judge_with(&mut b, ("slow", &slow_ask), ("CPP17", true), HONEST) else { return };
    assert_eq!(split, Outcome::Graded);
    assert_eq!(
        rec.cases.iter().map(|c| c.status).collect::<Vec<_>>(),
        [Status::Tle, Status::Ac],
        "{:?}",
        rec.cases
    );
    assert!(rec.cases[1].time_s > 0.1, "case 2 reports the callback's CPU: {:?}", rec.cases[1]);
}

/// A wall timeout goes to the side whose CPU ran out the clock, never to host load: the grader's own work
/// before the first call (4.5 s of CPU against a 4 s contestant wall, TL 1 s) is a problem fault (IE), while a
/// contestant that sleeps is TLE.
#[test]
fn a_wall_timeout_is_the_contestants_only_when_the_jury_did_not_use_the_time() {
    let mut b = bench(0);
    let busy_grader =
        GRADER.replace("long long got = play(n, hint);", "burn(4.5);\n  long long got = play(n, hint);");
    let Some((busy, _)) = judge_with(&mut b, ("busy", &busy_grader), ("CPP17", true), HONEST) else { return };
    assert!(matches!(&busy, Outcome::InternalError(text) if text.contains("timed out waiting")), "{busy:?}");
    let sleeper = "#include <unistd.h>\n#include \"sol.h\"\nlong long play(int n, const std::vector<long long>&){ sleep(30); return n; }\n";
    let (idle, rec) = judge(&mut b, "p", ("CPP17", true), sleeper).unwrap();
    assert_eq!(idle, Outcome::Graded);
    assert_eq!(rec.cases.first().map(|c| c.status), Some(Status::Tle), "{:?}", rec.cases);
}

/// The re-check of a `const&` object the contestant holds runs before every frame the grader sends, also before
/// the grader's own calls into the contestant: it is charged to the contestant, so a run that it slows past the TL
/// is TLE, never IE. A 1M-int object held across 200k grader calls (TL 1 s).
#[test]
fn the_recheck_of_a_held_const_reference_is_charged_to_the_contestant() {
    let mut b = bench(8);
    if language_or_skip(&registry(), "CPP17").is_none() {
        return;
    }
    let header = "#include <vector>\nconst std::vector<int>& arr();\nint query(int i);\n";
    let grader = "#include <cstdio>\n#include <vector>\n#include \"sol.h\"\nstatic std::vector<int> a;\n\
const std::vector<int>& arr(){ return a; }\n\
int main(){ a.assign(1000000, 0); for(int i=0;i<(int)a.size();i++) a[i]=i%7;\n\
  long long s=0, want=0; for(int i=0;i<200000;i++){ s+=query(i); want+=i%7; }\n  printf(\"%d\\n\", s==want ? 1 : 0); }\n";
    let holder = "#include \"sol.h\"\nstatic const std::vector<int>* held = 0;\n\
int query(int i){ if(!held) held=&arr(); return (*held)[i]; }\n";
    write_grader(&b, "held", grader, true);
    std::fs::write(b.root.join("held").join("sol.h"), header).unwrap();
    let (outcome, rec) = judge_written(&mut b, "held", holder, 262_144);
    assert_eq!(outcome, Outcome::Graded, "{outcome:?}");
    assert!(rec.cases.iter().all(|c| c.status == Status::Tle && c.time_s > 1.0), "{:?}", rec.cases);
}

/// A frame from the contestant that breaks the channel protocol ends the jury: the case is WA.
#[test]
fn a_frame_outside_the_protocol_is_wa() {
    let mut b = bench(2);
    let source = "#include <unistd.h>\n#include \"sol.h\"\nlong long play(int n, const std::vector<long long>&){\n\
  unsigned char frame[8] = {0xff, 0xff, 0xff, 0x7f, 1, 0, 0, 0};\n  if (write(4, frame, 8) != 8) return -2;\n\
  return n + ask(0);\n}\n";
    let Some((split, rec)) = judge(&mut b, "p", ("CPP17", true), source) else { return };
    assert_eq!(split, Outcome::Graded);
    assert_eq!(shape(&rec), [(Status::Wa, 0.0), (Status::Wa, 0.0)]);
    assert!(rec.cases.iter().all(|c| c.feedback == "protocol violation"), "{:?}", rec.cases);
}

/// `exit` inside a call ends both halves as one process: the grader's buffered output is flushed and checked;
/// `_exit` flushes nothing. Split and one-process give the same verdicts.
#[test]
fn an_exit_inside_a_call_flushes_the_graders_output_as_in_one_process() {
    let mut b = bench(4);
    let early = GRADER
        .replace("long long got = play(n, hint);", "printf(\"1\\n\");\n  long long got = play(n, hint);");
    for (end, statuses) in [("exit(0)", [Status::Ac, Status::Ac]), ("_exit(0)", [Status::Wa, Status::Wa])] {
        let source = format!(
            "#include <cstdlib>\n#include <unistd.h>\n#include \"sol.h\"\nlong long play(int n, const std::vector<long long>&){{ {end}; return n; }}\n"
        );
        for split in [false, true] {
            let Some((outcome, rec)) = judge_with(&mut b, ("early", &early), ("CPP17", split), &source)
            else {
                return;
            };
            assert_eq!(outcome, Outcome::Graded, "{end} split {split}");
            assert_eq!(
                rec.cases.iter().map(|c| c.status).collect::<Vec<_>>(),
                statuses,
                "{end} split {split}"
            );
        }
    }
}

/// A grader that does not compile is a problem fault for each submission, and the failed build is not kept: the
/// next submission builds again (a judge-side failure must never stick to the problem).
#[test]
fn a_jury_build_that_fails_is_an_internal_error_and_never_cached() {
    let mut b = bench(6);
    let broken = GRADER.replace("int main(){", "int main(){ this does not compile;");
    for _ in 0..2 {
        let Some((split, _)) = judge_with(&mut b, ("broken", &broken), ("CPP17", true), HONEST) else {
            return;
        };
        assert!(
            matches!(&split, Outcome::InternalError(text) if text.contains("does not compile")),
            "{split:?}"
        );
        assert!(!b.cache.join("split-jury").exists(), "a failed jury build left a cache entry");
    }
}

fn judge_written(b: &mut Bench, code: &str, source: &str, memory_kib: u64) -> (Outcome, Recorder) {
    let problem = b.resolver.resolve(code, 0).unwrap();
    let mut rec = Recorder::default();
    let outcome =
        judge_submission(&mut b.env, &problem, &submission("CPP17", source, 1.0, memory_kib), &mut rec);
    (outcome, rec)
}

fn status_of((outcome, rec): (Outcome, Recorder)) -> Status {
    assert_eq!(outcome, Outcome::Graded, "{outcome:?}");
    rec.cases.first().map(|c| c.status).expect("at least one case")
}
