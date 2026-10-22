#![cfg(feature = "isolate")]
mod common;

use common::{box_id, language_or_skip, registry, sandbox};
use judge_grader::compile::{CompileJob, build, program_for};
use judge_grader::file_io::SolutionFileIo;
use judge_grader::languages::Language;
use judge_grader::program::Program;
use judge_grader::run_case::{CaseIo, CaseLimits, CaseRun, Runnable, run_case};
use judge_problem::init::FileIo;
use judge_sandbox::classify::RunVerdict;
use judge_sandbox::{DiskQuota, IsolateBox, SandboxConfig, disk_quota_works};
use std::fs::File;
use std::path::PathBuf;

struct Fixture {
    _dirs: Vec<tempfile::TempDir>,
    sandbox: IsolateBox,
    program: Program,
    scratch: PathBuf,
}

fn fixture_with(offset: u32, language: &Language, source: &str) -> Fixture {
    let dirs: Vec<_> = (0..3).map(|_| tempfile::tempdir().unwrap()).collect();
    let mut sandbox = sandbox(offset, dirs[0].path());
    let job = CompileJob::for_language(language, source.as_bytes());
    build(&mut sandbox, &job, dirs[1].path()).unwrap();
    let program = program_for(language, dirs[1].path(), source.as_bytes());
    let scratch = dirs[2].path().to_path_buf();
    Fixture { _dirs: dirs, sandbox, program, scratch }
}

fn fixture(offset: u32, key: &str, source: &str) -> Option<Fixture> {
    let language = language_or_skip(&registry(), key)?;
    Some(fixture_with(offset, &language, source))
}

const LIMITS: CaseLimits = CaseLimits { time_limit_s: 1.0, memory_kib: 65_536, jvm_reserve_kib: 131_072 };

fn input(f: &Fixture, text: &str) -> File {
    let path = f.scratch.join("in");
    std::fs::write(&path, text).unwrap();
    File::open(path).unwrap()
}

fn go(f: &mut Fixture, io: &CaseIo, name: &str) -> (CaseRun, PathBuf) {
    let out = f.scratch.join(name);
    let runnable = Runnable { program: &f.program, limits: &LIMITS };
    (run_case(&mut f.sandbox, &runnable, io, &out).unwrap(), out)
}

fn plain(file: &File, cap: u64) -> CaseIo<'_> {
    CaseIo { input: file, output_cap: cap, file_io: None }
}

#[test]
fn file_io_reads_and_writes_named_files() {
    let src = "a, b = map(int, open('sum.inp').read().split())\nopen('sum.out', 'w').write(str(a + b))\n";
    let Some(mut f) = fixture(1, "PY3", src) else { return };
    let file_io = SolutionFileIo::new(&FileIo { input: "sum.inp".into(), output: "sum.out".into() }, b"");
    let file = input(&f, "4 5");
    let io = CaseIo { input: &file, output_cap: 1 << 20, file_io: Some(&file_io) };
    let (run, out) = go(&mut f, &io, "out");
    assert_eq!(run.verdict, RunVerdict::Ok);
    assert_eq!(std::fs::read_to_string(&out).unwrap(), "9");
}

/// The old judge matched file I/O names ignoring letter case (teachers and contestants use Windows).
fn file_io_case(
    offset: u32,
    src: &str,
    names: (&str, &str),
    (text, cap): (&str, u64),
) -> Option<(RunVerdict, String)> {
    let mut f = fixture(offset, "PY3", src)?;
    let file_io =
        SolutionFileIo::new(&FileIo { input: names.0.into(), output: names.1.into() }, src.as_bytes());
    let file = input(&f, text);
    let io = CaseIo { input: &file, output_cap: cap, file_io: Some(&file_io) };
    let (run, out) = go(&mut f, &io, "out");
    Some((run.verdict, std::fs::read_to_string(&out).unwrap()))
}

fn file_io_run(offset: u32, src: &str, names: (&str, &str), text: &str) -> Option<String> {
    let (verdict, output) = file_io_case(offset, src, names, (text, 1 << 20))?;
    assert_eq!(verdict, RunVerdict::Ok);
    Some(output)
}

#[test]
fn file_io_names_match_in_any_letter_case() {
    let upper =
        "a, b = map(int, open('NUMBER.INP').read().split())\nopen('NUMBER.OUT', 'w').write(str(a + b))\n";
    let Some(output) = file_io_run(18, upper, ("Number.inp", "Number.out"), "4 5") else { return };
    assert_eq!(output, "9");
    let camel =
        "name = 'PathLib'\nx = int(open(name + '.Inp').read())\nopen('PATHLIB.out', 'w').write(str(2 * x))\n";
    assert_eq!(file_io_run(18, camel, ("pathlib.inp", "pathlib.out"), "21").unwrap(), "42");
    let odd = "x = int(open('nUmBeR.iNp').read())\nopen('nUmBeR.oUt', 'w').write(str(x + 1))\n";
    assert_eq!(file_io_run(18, odd, ("number.inp", "number.out"), "1").unwrap(), "2");
}

/// Two outputs differing only in case: the configured spelling wins, else the first name in byte order.
#[test]
fn of_several_output_spellings_the_configured_one_then_the_first_by_name_is_read() {
    let both = "open('NUMBER.OUT', 'w').write('upper')\nopen('Number.out', 'w').write('config')\n";
    let Some(output) = file_io_run(20, both, ("Number.inp", "Number.out"), "") else { return };
    assert_eq!(output, "config");
    let neither = "open('number.out', 'w').write('lower')\nopen('NUMBER.OUT', 'w').write('upper')\n";
    assert_eq!(file_io_run(20, neither, ("Number.inp", "Number.out"), "").unwrap(), "upper");
}

/// The input's extra names are hard links: they use no file quota and are not output, so an input many
/// times the output cap is still a normal run.
#[test]
fn input_aliases_do_not_count_against_the_output_cap() {
    let src = "data = open('NUMBER.INP').read()\nopen('number.out', 'w').write(str(len(data)))\n";
    let text = "7".repeat(1 << 20);
    let Some((verdict, output)) = file_io_case(21, src, ("Number.inp", "Number.out"), (&text, 100)) else {
        return;
    };
    assert_eq!((verdict, output), (RunVerdict::Ok, (1 << 20).to_string()));
}

#[test]
fn a_file_io_program_that_uses_stdin_and_stdout_has_an_empty_output() {
    let src = "import sys\nprint(sys.stdin.read())\n";
    let Some(output) = file_io_run(19, src, ("sum.inp", "sum.out"), "7") else { return };
    assert_eq!(output, "");
}

#[test]
fn a_file_io_name_that_leaves_the_box_never_runs() {
    let Some(mut f) = fixture(2, "PY3", "print(1)\n") else { return };
    let file_io = SolutionFileIo::new(&FileIo { input: "../x".into(), output: "o".into() }, b"");
    let file = input(&f, "");
    let io = CaseIo { input: &file, output_cap: 100, file_io: Some(&file_io) };
    let runnable = Runnable { program: &f.program, limits: &LIMITS };
    assert!(run_case(&mut f.sandbox, &runnable, &io, &f.scratch.join("o")).is_err());
}

#[test]
fn a_program_that_makes_the_output_a_symlink_has_an_empty_output() {
    let src = "import os\nos.symlink('/etc/hostname', 'output')\n";
    let Some(mut f) = fixture(3, "PY3", src) else { return };
    let file = input(&f, "");
    let (run, out) = go(&mut f, &plain(&file, 1 << 20), "out");
    assert_eq!(run.output_len, 0);
    assert_eq!(std::fs::read(&out).unwrap(), b"");
}

#[test]
fn output_of_exactly_the_cap_is_not_ole() {
    let Some(mut f) = fixture(5, "PY3", "print('x' * 99)\n") else { return };
    let file = input(&f, "");
    let (run, _) = go(&mut f, &plain(&file, 100), "out");
    assert_eq!((run.verdict, run.output_len), (RunVerdict::Ok, 100));
}

#[test]
fn second_case_after_mle_is_clean() {
    let src = "import sys\nif sys.stdin.read().strip() == 'big':\n    a = bytearray(200_000_000)\nprint(1)\n";
    let Some(mut f) = fixture(6, "PY3", src) else { return };
    let big = input(&f, "big");
    let (first, _) = go(&mut f, &plain(&big, 1 << 20), "o1");
    assert_eq!(first.verdict, RunVerdict::Mle);
    let small = input(&f, "small");
    let (second, _) = go(&mut f, &plain(&small, 1 << 20), "o2");
    assert_eq!(second.verdict, RunVerdict::Ok);
    assert!(second.memory_kib < first.memory_kib);
}

#[test]
fn a_native_crash_is_rte_by_signal() {
    let Some(mut f) = fixture(10, "CPP17", "int main(){ int *p = 0; return *p; }") else { return };
    let file = input(&f, "");
    let (run, _) = go(&mut f, &plain(&file, 1 << 20), "out");
    assert_eq!(run.verdict, RunVerdict::Rte { reason: "SIGSEGV".into() });
}

/// JAVA with the JRE-only layout of this host: javac is reached through the compiler module.
fn java_language() -> Option<Language> {
    let java = "/usr/lib/jvm/default-java/bin/java";
    if !std::path::Path::new(java).exists() {
        eprintln!("skip: JAVA (no JVM on this host)");
        return None;
    }
    let mut language = registry().get("JAVA").unwrap().clone();
    let javac = [java, "-m", "jdk.compiler/com.sun.tools.javac.Main", "-encoding", "UTF-8", "{class}.java"];
    language.compile = Some(javac.map(String::from).to_vec());
    Some(language)
}

#[test]
fn java_uncaught_exception_is_rte_and_out_of_memory_is_mle() {
    let Some(language) = java_language() else { return };
    let boom =
        "public class Main { public static void main(String[] a) { int x = 0; System.out.println(1 / x); } }";
    let mut f = fixture_with(11, &language, boom);
    let file = input(&f, "");
    let (run, _) = go(&mut f, &plain(&file, 1 << 20), "out");
    assert_eq!(run.verdict, RunVerdict::Rte { reason: "ArithmeticException".into() });
    let hog = "public class Main { public static void main(String[] a) { long[] x = new long[200_000_000]; System.out.println(x.length); } }";
    let mut g = fixture_with(12, &language, hog);
    let (run, _) = go(&mut g, &plain(&file, 1 << 20), "out");
    assert_eq!(run.verdict, RunVerdict::Mle);
}

#[test]
fn the_input_comes_from_the_opened_handle_not_from_its_path() {
    let Some(mut f) = fixture(13, "PY3", "print(input())\n") else { return };
    let file = input(&f, "original\n");
    let path = f.scratch.join("in");
    std::fs::remove_file(&path).unwrap();
    std::fs::write(&path, "swapped\n").unwrap();
    let (run, out) = go(&mut f, &plain(&file, 1 << 20), "out");
    assert_eq!(run.verdict, RunVerdict::Ok);
    assert_eq!(std::fs::read_to_string(&out).unwrap(), "original\n");
}

#[test]
fn a_chatty_stderr_does_not_change_a_correct_verdict() {
    let src = "import sys\nfor i in range(20000): print('debug line ' * 10, file=sys.stderr)\nprint(1)\n";
    let Some(mut f) = fixture(14, "PY3", src) else { return };
    let file = input(&f, "");
    let (run, out) = go(&mut f, &plain(&file, 8192), "out");
    assert_eq!(run.verdict, RunVerdict::Ok, "stderr is bigger than the output cap");
    assert_eq!(std::fs::read_to_string(&out).unwrap(), "1\n");
}

#[test]
fn stderr_text_and_volume_before_the_traceback_do_not_rename_the_exception() {
    let src = "import sys\nprint('dbg', file=sys.stderr)\nfor i in range(3000): print('x' * 100, file=sys.stderr)\nprint(1 // 0)\n";
    let Some(mut f) = fixture(15, "PY3", src) else { return };
    let file = input(&f, "");
    let (run, _) = go(&mut f, &plain(&file, 1 << 20), "out");
    assert_eq!(run.verdict, RunVerdict::Rte { reason: "ZeroDivisionError".into() });
}

#[test]
fn a_python_stdout_flood_is_still_ole_with_stderr_kept() {
    let Some(mut f) = fixture(16, "PY3", "while True: print('x' * 1000)\n") else { return };
    let file = input(&f, "");
    let (run, _) = go(&mut f, &plain(&file, 8192), "out");
    assert_eq!(run.verdict, RunVerdict::Ole);
}

#[test]
fn an_uncaught_ruby_exception_is_rte() {
    let Some(mut f) = fixture(17, "RUBY", "puts 1 / 0\n") else { return };
    let file = input(&f, "");
    let (run, _) = go(&mut f, &plain(&file, 1 << 20), "out");
    assert_eq!(run.verdict, RunVerdict::Rte { reason: "ZeroDivisionError".into() });
}

#[test]
fn reported_memory_is_the_program_not_the_page_cache_of_its_output() {
    // ~1 MiB of live memory, 20 MiB written to stdout (under the 24 MiB default cap).
    let src = "#include <cstdio>\n#include <cstring>\nstatic char line[1 << 20];\nint main(){ memset(line, 'x', sizeof line - 1); for (int i = 0; i < 20; i++) fputs(line, stdout); }\n";
    let Some(mut f) = fixture(40, "CPP17", src) else { return };
    let file = input(&f, "");
    let (run, _) = go(&mut f, &plain(&file, 24 << 20), "out");
    assert_eq!(run.verdict, RunVerdict::Ok);
    assert!(run.memory_kib < 12 * 1024, "memory {} KiB includes the 20 MiB output", run.memory_kib);
}

fn go_limited(f: &mut Fixture, io: &CaseIo, memory_kib: u64) -> CaseRun {
    let limits = CaseLimits { memory_kib, ..LIMITS };
    let runnable = Runnable { program: &f.program, limits: &limits };
    run_case(&mut f.sandbox, &runnable, io, &f.scratch.join("out")).unwrap()
}

/// `n` MiB written to stdout in 64 KiB writes, or `n` MiB allocated and touched with `alloc`.
const WRITE_OR_ALLOC: &str = "#include <cstdio>\n#include <cstdlib>\n#include <cstring>\nstatic char block[1 << 16];\n\
int main(){ char how[8]; int n; if (scanf(\"%7s %d\", how, &n) != 2) return 2;\n\
if (how[0] == 'a') { char *p = (char*)malloc((size_t)n << 20); memset(p, 1, (size_t)n << 20); printf(\"%d\", p[n]); return 0; }\n\
memset(block, 'x', sizeof block); for (long i = 0; i < (long)n << 4; i++) fwrite(block, 1, sizeof block, stdout); }\n";

#[test]
fn a_small_memory_limit_with_a_large_output_is_not_mle() {
    // The page cache of the output is charged to the cgroup; without room for it this run was OOM-killed
    // now and then (writeback timing), so it runs a few times.
    let Some(mut f) = fixture(41, "CPP17", WRITE_OR_ALLOC) else { return };
    for _ in 0..5 {
        let file = input(&f, "write 48");
        let run = go_limited(&mut f, &plain(&file, 64 << 20), 8192);
        assert_eq!(run.verdict, RunVerdict::Ok, "48 MiB written under an 8 MiB limit");
        assert_eq!(run.output_len, 48 << 20);
        assert!(run.memory_kib < 8192, "memory {} KiB", run.memory_kib);
    }
}

#[test]
fn memory_over_the_limit_is_mle_with_or_without_an_oom_kill() {
    let Some(mut f) = fixture(42, "CPP17", WRITE_OR_ALLOC) else { return };
    let over = input(&f, "alloc 24");
    let run = go_limited(&mut f, &plain(&over, 32 << 20), 8192);
    assert_eq!(
        run.verdict,
        RunVerdict::Mle,
        "24 MiB touched under an 8 MiB limit, inside the page-cache room"
    );
    assert!(run.memory_kib > 8192, "an MLE is not shown under the limit: {} KiB", run.memory_kib);
    let far_over = input(&f, "alloc 512");
    assert_eq!(go_limited(&mut f, &plain(&far_over, 32 << 20), 8192).verdict, RunVerdict::Mle);
    let under = input(&f, "alloc 4");
    assert_eq!(go_limited(&mut f, &plain(&under, 32 << 20), 16_384).verdict, RunVerdict::Ok);
}

/// Maps its stdin (a regular file) and reads one byte of every page.
const MAP_INPUT: &str = "#include <cstdio>\n#include <sys/mman.h>\n#include <sys/stat.h>\n\
int main(){ struct stat st; if (fstat(0, &st) != 0) return 2;\n\
const char *p = (const char*)mmap(nullptr, st.st_size, PROT_READ, MAP_PRIVATE, 0, 0); if (p == MAP_FAILED) return 3;\n\
long sum = 0; for (long i = 0; i < st.st_size; i += 4096) sum += p[i]; printf(\"%ld\", sum); }\n";

#[test]
fn mapped_files_already_in_page_cache_are_not_the_programs_memory() {
    // The resident pages of a mapped, already cached file count in the peak RSS but are not charged to the
    // run: a runtime's shared objects (~37 MiB of PyPy's) and, here, a 32 MiB input.
    let Some(mut f) = fixture(43, "CPP17", MAP_INPUT) else { return };
    let file = input(&f, &"x".repeat(32 << 20));
    let run = go_limited(&mut f, &plain(&file, 1 << 20), 8192);
    assert_eq!(run.verdict, RunVerdict::Ok, "32 MiB of mapped input under an 8 MiB limit");
    assert!(run.memory_kib < 8192, "memory {} KiB", run.memory_kib);
}

/// 60 MiB of heap, then 22 MiB written to a `memfd` or `/dev/shm` file and read back with pread, so the
/// file's pages are charged to the run but never mapped: they are not in the peak RSS. Prints `refused`
/// when the file cannot be made.
const STASH: &str = "#include <cstdio>\n#include <cstdlib>\n#include <cstring>\n#include <fcntl.h>\n\
#include <sys/mman.h>\n#include <unistd.h>\nstatic char block[1 << 20];\nchar *volatile heap;\n\
int main(){ char how[8]; if (scanf(\"%7s\", how) != 1) return 2;\n\
int fd = how[0] == 'm' ? memfd_create(\"stash\", 0) : open(\"/dev/shm/stash\", O_RDWR | O_CREAT | O_TRUNC, 0600);\n\
if (fd < 0) { puts(\"refused\"); return 0; }\n\
heap = (char*)malloc(60 << 20); memset(heap, 1, 60 << 20); long sum = 0;\n\
for (int i = 0; i < 22; i++) { memset(block, i, sizeof block); if (write(fd, block, sizeof block) != sizeof block) return 4; }\n\
for (int i = 0; i < 22; i++) { if (pread(fd, block, sizeof block, (off_t)i << 20) != sizeof block) return 5; sum += block[7]; }\n\
printf(\"%ld %d\", sum, heap[(60 << 20) - 1]); }\n";

#[test]
fn file_pages_the_program_holds_unmapped_count_against_the_memory_limit() {
    // 82 MiB charged under a 64 MiB limit, inside the cgroup's 24 MiB page-cache room: no OOM kill.
    let Some(mut f) = fixture(47, "CPP17", STASH) else { return };
    let file = input(&f, "memfd");
    let run = go_limited(&mut f, &plain(&file, 24 << 20), 65_536);
    assert_eq!(run.verdict, RunVerdict::Mle, "memory {} KiB", run.memory_kib);
    assert!(run.memory_kib > 80 << 10, "memory {} KiB leaves out the memfd", run.memory_kib);
    let file = input(&f, "shm");
    let run = go_limited(&mut f, &plain(&file, 24 << 20), 65_536);
    assert_eq!(run.verdict, RunVerdict::Ok);
    assert_eq!(std::fs::read_to_string(f.scratch.join("out")).unwrap(), "refused\n", "/dev/shm is writable");
}

/// Drops its stdin's page cache, then reads it all: the input's pages are charged to the run.
const READ_EVICTED: &str = "#include <cstdio>\n#include <fcntl.h>\n#include <unistd.h>\n\
static char block[1 << 16];\n\
int main(){ fdatasync(0); posix_fadvise(0, 0, 0, POSIX_FADV_DONTNEED); long n = 0, r;\n\
while ((r = read(0, block, sizeof block)) > 0) n += r; printf(\"%ld\", n); }\n";

#[test]
fn the_page_cache_of_an_input_the_program_reads_is_not_its_memory() {
    let Some(mut f) = fixture(48, "CPP17", READ_EVICTED) else { return };
    let file = input(&f, &"x".repeat(40 << 20));
    let run = go_limited(&mut f, &plain(&file, 1 << 20), 32_768);
    assert_eq!(run.verdict, RunVerdict::Ok, "40 MiB of input read under a 32 MiB limit");
    assert!(run.memory_kib < 8192, "memory {} KiB", run.memory_kib);
}

/// The box for a run with a disk quota of `headroom_kib` over what the judge stages, or None (skip) when
/// isolate's box root has no user quotas.
fn quota_box(offset: u32, meta: &std::path::Path, headroom_kib: u32) -> Option<(SandboxConfig, IsolateBox)> {
    let quota = DiskQuota { headroom_kib, headroom_inodes: 256 };
    let config = SandboxConfig { meta_dir: meta.into(), disk_quota: Some(quota), ..SandboxConfig::default() };
    if !disk_quota_works(&config, box_id(offset)).unwrap() {
        eprintln!("skip: isolate's box root has no user quotas on this host");
        return None;
    }
    let sandbox = IsolateBox::open(&config, box_id(offset), None).unwrap();
    Some((config, sandbox))
}

/// isolate charges the staged input to the box's disk quota: an input over the headroom still gives the
/// correct, full output.
#[test]
fn a_case_input_over_the_disk_quota_headroom_is_judged_in_full() {
    let src = "import sys\nsys.stdout.buffer.write(sys.stdin.buffer.read())\n";
    let Some(mut f) = fixture(44, "PY3", src) else { return };
    let meta = tempfile::tempdir().unwrap();
    let Some((config, quota_sandbox)) = quota_box(45, meta.path(), 8192) else { return };
    f.sandbox = quota_sandbox;
    let text = "7".repeat(12 << 20);
    let file = input(&f, &text);
    let (run, out) = go(&mut f, &plain(&file, 12 << 20), "out");
    let output_len = std::fs::metadata(&out).unwrap().len();
    drop(f);
    let no_limit = DiskQuota { headroom_kib: i32::MAX as u32, headroom_inodes: i32::MAX as u32 };
    disk_quota_works(&SandboxConfig { disk_quota: Some(no_limit), ..config }, box_id(45)).unwrap();
    assert_eq!((run.verdict, output_len), (RunVerdict::Ok, 12 << 20));
}
