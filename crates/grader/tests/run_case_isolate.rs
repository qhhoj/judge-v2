#![cfg(feature = "isolate")]
mod common;

use common::{box_id, language_or_skip, registry, sandbox};
use judge_grader::compile::{CompileJob, build, program_for};
use judge_grader::file_io::SolutionFileIo;
use judge_grader::languages::{DEFAULT_JAVA, Language, jdk_etc_dir};
use judge_grader::program::Program;
use judge_grader::run_case::{CaseIo, CaseLimits, CaseRun, Runnable, run_case};
use judge_problem::init::FileIo;
use judge_sandbox::classify::RunVerdict;
use judge_sandbox::spec::{hard_cap_s, whole_ms};
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

/// Keeps 256 MiB in `/tmp` files of 1 MiB (the output cap), opened with `O_DIRECT` where the file system takes it.
const STASH_IN_TMP: &str = "import mmap, os
block = mmap.mmap(-1, 1 << 20)
block.write(b'x' * (1 << 20))
for i in range(256):
    try:
        fd = os.open(f'/tmp/{i}', os.O_WRONLY | os.O_CREAT | os.O_DIRECT)
    except OSError:
        fd = os.open(f'/tmp/{i}', os.O_WRONLY | os.O_CREAT)
    os.write(fd, block)
    os.close(fd)
print(9)
";

#[test]
fn what_a_run_keeps_in_tmp_counts_as_its_memory() {
    let Some(mut f) = fixture(106, "PY3", STASH_IN_TMP) else { return };
    let file = input(&f, "");
    let (run, _) = go(&mut f, &plain(&file, 1 << 20), "out");
    assert_eq!(run.verdict, RunVerdict::Mle, "{} KiB", run.memory_kib);
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
fn a_program_that_makes_the_output_file_a_symlink_has_an_empty_output() {
    let src = "import os\nos.symlink('/etc/hostname', 'sum.out')\n";
    let Some((verdict, output)) = file_io_case(3, src, ("sum.inp", "sum.out"), ("", 1 << 20)) else { return };
    assert_eq!((verdict, output), (RunVerdict::Ok, String::new()));
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

#[test]
fn a_tle_is_killed_past_the_hard_cap_and_reports_its_real_time() {
    let Some(mut f) = fixture(49, "CPP17", "int main(){ volatile unsigned long n = 0; for (;;) n++; }")
    else {
        return;
    };
    let file = input(&f, "");
    let (run, _) = go(&mut f, &plain(&file, 1 << 20), "out");
    assert_eq!(run.verdict, RunVerdict::Tle);
    // isolate kills once the CPU time is past both --time and --extra-time (the hard cap, 2 s for 1 s)
    assert!(whole_ms(run.time_s) > whole_ms(hard_cap_s(LIMITS.time_limit_s)), "{}", run.time_s);
}

/// JAVA with the JRE-only layout of this host: javac is reached through the compiler module, and the
/// box binds the `/etc` directory of the installed JDK, whatever its major.
fn java_language() -> Option<Language> {
    let java = format!("{DEFAULT_JAVA}/bin/java");
    if !std::path::Path::new(&java).exists() {
        eprintln!("skip: JAVA (no JVM on this host)");
        return None;
    }
    let mut language = registry().get("JAVA").unwrap().clone();
    let jdk_etc = jdk_etc_dir(std::path::Path::new(DEFAULT_JAVA)).expect("default-java names an OpenJDK");
    assert_eq!(language.dirs, [jdk_etc.as_str()]);
    assert_eq!(language.compile_dirs, [jdk_etc.as_str()]);
    let javac =
        [java.as_str(), "-m", "jdk.compiler/com.sun.tools.javac.Main", "-encoding", "UTF-8", "{class}.java"];
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
    go_within(f, io, &CaseLimits { memory_kib, ..LIMITS })
}

fn go_within(f: &mut Fixture, io: &CaseIo, limits: &CaseLimits) -> CaseRun {
    let runnable = Runnable { program: &f.program, limits };
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

/// Time limit of the memory tests that sync a large file in the box (the evicted-input test, and the `file_io`
/// row of `file_output_memory_is_bounded`): a sync waits behind the host's writeback (7.7 s,
/// and a 16 s wall-clock TLE at TL 1 s, under IO load: #92).
/// Honest-write tests stay at TL 1 s, so a stalled write still fails them at the production wall.
const EVICTED_INPUT_TIME_LIMIT_S: f64 = 20.0;

/// Drops its stdin's page cache, then reads it all: the input's pages are charged to the run.
const READ_EVICTED: &str = "#include <cstdio>\n#include <fcntl.h>\n#include <unistd.h>\n\
static char block[1 << 16];\n\
int main(){ fdatasync(0); posix_fadvise(0, 0, 0, POSIX_FADV_DONTNEED); long n = 0, r;\n\
while ((r = read(0, block, sizeof block)) > 0) n += r; printf(\"%ld\", n); }\n";

#[test]
fn the_page_cache_of_an_input_the_program_reads_is_not_its_memory() {
    let Some(mut f) = fixture(48, "CPP17", READ_EVICTED) else { return };
    let file = input(&f, &"x".repeat(40 << 20));
    let limits = CaseLimits { memory_kib: 32_768, time_limit_s: EVICTED_INPUT_TIME_LIMIT_S, ..LIMITS };
    let run = go_within(&mut f, &plain(&file, 1 << 20), &limits);
    assert_eq!(run.verdict, RunVerdict::Ok, "40 MiB of input read under a 32 MiB limit");
    assert!(run.memory_kib < 8192, "memory {} KiB", run.memory_kib);
}

/// `text` padded with spaces to `bytes`.
fn padded(text: &str, bytes: usize) -> String {
    text.to_string() + &" ".repeat(bytes.saturating_sub(text.len()))
}

/// Reads `n how` from stdin or `late.inp` (never the rest of its input), keeps `n` MiB in a memfd it reads back with pread and closes,
/// prints a checksum, then writes `n` MiB more: spaces after the answer on stdout (`s`), the same into the
/// file `late.out` after the answer (`f`), a file `junk` beside it (`j`), or nothing (`-`); `a` instead reserves
/// `n` MiB of disk past the end of `late.out` with `fallocate(KEEP_SIZE)`.
const LATE: &str = "#include <cstdio>\n#include <cstring>\n#include <fcntl.h>\n#include <sys/mman.h>\n#include <unistd.h>\n\
static char block[1 << 20];\n\
int main(){ char first[16] = {0}, how = '-'; int in = open(\"late.inp\", O_RDONLY); if (in < 0) in = 0;\n\
if (read(in, first, 15) < 1) return 2; int n = 0;\n\
sscanf(first, \"%d %c\", &n, &how);\n\
int fd = memfd_create(\"held\", 0); if (fd < 0) return 3; long sum = 0;\n\
for (int i = 0; i < n; i++) { memset(block, i + 1, sizeof block); if (write(fd, block, sizeof block) != sizeof block) return 4; }\n\
for (int i = 0; i < n; i++) { if (pread(fd, block, sizeof block, (off_t)i << 20) != sizeof block) return 5; sum += block[7]; }\n\
close(fd); FILE *out = how == 'f' || how == 'a' ? fopen(\"late.out\", \"w\") : how == 'j' ? fopen(\"junk\", \"w\") : stdout;\n\
if (how == 'j') printf(\"%ld\\n\", sum); else fprintf(out, \"%ld\\n\", sum);\n\
memset(block, ' ', sizeof block);\n\
if (how == 'a') { fflush(out); if (fallocate(fileno(out), FALLOC_FL_KEEP_SIZE, 0, (off_t)n << 20)) return 6; }\n\
else if (how != '-') for (int i = 0; i < n; i++) fwrite(block, 1, sizeof block, out);\n\
return fclose(out) == 0 ? 0 : 7; }\n";

#[test]
fn memory_after_the_peak_is_counted() {
    let Some(mut f) = fixture(50, "CPP17", LATE) else { return };
    // (input, MiB of input) under a 16 MiB limit: 20 MiB held, then 20 MiB written
    let rows = [("20 -\n", 0), ("20 -\n", 20), ("20 s\n", 0), ("20 j\n", 20)];
    for (head, input_mib) in rows {
        let file = input(&f, &padded(head, input_mib << 20));
        let run = go_limited(&mut f, &plain(&file, 24 << 20), 16_384);
        assert_eq!(
            run.verdict,
            RunVerdict::Mle,
            "{head:?} input {input_mib} MiB: memory {} KiB",
            run.memory_kib
        );
        assert!(run.memory_kib > 20 << 10, "{head:?}: memory {} KiB", run.memory_kib);
    }
    let file_io = SolutionFileIo::new(&FileIo { input: "late.inp".into(), output: "late.out".into() }, b"");
    for head in ["20 f\n", "20 a\n"] {
        let file = input(&f, head);
        let io = CaseIo { input: &file, output_cap: 24 << 20, file_io: Some(&file_io) };
        let run = go_limited(&mut f, &io, 16_384);
        assert_eq!(run.verdict, RunVerdict::Mle, "file I/O {head:?}: memory {} KiB", run.memory_kib);
    }
}

/// Keeps 15 MiB in a memfd it reads back with pread and closes, then writes `n` MiB to its kept stderr.
const PY_LATE_STDERR: &str = "import os\nn = int(input())\nblock = b'x' * 65536\n\
fd = os.memfd_create('held')\ns = 0\n\
for _ in range(15 * 16): os.write(fd, block)\n\
for i in range(15): s += os.pread(fd, 65536, i << 20)[7]\n\
os.close(fd)\nprint(s, flush=True)\n\
for _ in range(n * 16): os.write(2, block)\n";

#[test]
fn kept_stderr_memory_is_counted() {
    let Some(mut f) = fixture(52, "PY3", PY_LATE_STDERR) else { return };
    let none = input(&f, "0\n");
    let base = go_limited(&mut f, &plain(&none, 24 << 20), 262_144);
    assert_eq!(base.verdict, RunVerdict::Ok);
    // 4 MiB under what the run needs with the memfd: MLE whatever its stderr
    let limit = base.memory_kib - (4 << 10);
    let run = go_limited(&mut f, &plain(&none, 24 << 20), limit);
    assert_eq!(run.verdict, RunVerdict::Mle, "control at {limit} KiB: memory {} KiB", run.memory_kib);
    let chatty = input(&f, "15\n");
    let run = go_limited(&mut f, &plain(&chatty, 24 << 20), limit);
    assert_eq!(
        run.verdict,
        RunVerdict::Mle,
        "15 MiB of stderr at {limit} KiB: memory {} KiB",
        run.memory_kib
    );
}

/// Reads `pad how` from stdin or `pad.inp`, writes `3` and `pad` MiB of newlines to stdout or `pad.out`, syncs
/// that output and drops its page cache, then holds 20 MiB written to a memfd (`m`) or to the box file `held`
/// (`f`) together with 56 MiB of touched heap, and reads the 20 MiB back.
const PADDED_THEN_HELD: &str = "#include <cstdio>\n#include <cstdlib>\n#include <cstring>\n#include <fcntl.h>\n\
#include <sys/mman.h>\n#include <unistd.h>\nstatic char block[1 << 20];\nchar *volatile heap;\n\
int main(){ char first[16] = {0}, how = 'm'; int in = open(\"pad.inp\", O_RDONLY); if (in < 0) in = 0;\n\
if (read(in, first, 15) < 1) return 2; int pad = 0; sscanf(first, \"%d %c\", &pad, &how);\n\
int out = in == 0 ? 1 : open(\"pad.out\", O_WRONLY | O_CREAT | O_TRUNC, 0644); if (out < 0) return 3;\n\
if (write(out, \"3\\n\", 2) != 2) return 4; memset(block, '\\n', sizeof block);\n\
for (int i = 0; i < pad; i++) if (write(out, block, sizeof block) != sizeof block) return 5;\n\
fsync(out); posix_fadvise(out, 0, 0, POSIX_FADV_DONTNEED);\n\
int fd = how == 'm' ? memfd_create(\"held\", 0) : open(\"held\", O_RDWR | O_CREAT | O_TRUNC, 0600); if (fd < 0) return 6;\n\
for (int i = 0; i < 20; i++) { memset(block, i + 1, sizeof block); if (write(fd, block, sizeof block) != sizeof block) return 7; }\n\
heap = (char*)malloc(56 << 20); if (!heap) return 8; memset(heap, 1, 56 << 20); long sum = 0;\n\
for (int i = 0; i < 20; i++) { if (pread(fd, block, sizeof block, (off_t)i << 20) != sizeof block) return 9; sum += block[7] + heap[(size_t)i << 20]; }\n\
return sum == 230 ? 0 : 10; }\n";

#[test]
fn file_output_memory_is_bounded() {
    // 76 MiB held at once under a 64 MiB limit, inside the cgroup's 24 MiB page-cache room: no OOM kill.
    let Some(mut f) = fixture(62, "CPP17", PADDED_THEN_HELD) else { return };
    for head in ["0 m\n", "22 m\n", "22 f\n"] {
        let file = input(&f, head);
        let run = go_limited(&mut f, &plain(&file, 24 << 20), 65_536);
        assert_eq!(run.verdict, RunVerdict::Mle, "{head:?}: memory {} KiB", run.memory_kib);
        assert!(run.memory_kib > 65_536, "{head:?}: memory {} KiB", run.memory_kib);
    }
    let file_io = SolutionFileIo::new(&FileIo { input: "pad.inp".into(), output: "pad.out".into() }, b"");
    let file = input(&f, "22 m\n");
    let io = CaseIo { input: &file, output_cap: 24 << 20, file_io: Some(&file_io) };
    // its `fsync` of a 22 MiB file waits behind the host's writeback, as the evicted-input test's does
    let limits = CaseLimits { memory_kib: 65_536, time_limit_s: EVICTED_INPUT_TIME_LIMIT_S, ..LIMITS };
    let run = go_within(&mut f, &io, &limits);
    assert_eq!(run.verdict, RunVerdict::Mle, "file I/O: memory {} KiB", run.memory_kib);
    assert!(run.memory_kib > 65_536, "file I/O: memory {} KiB", run.memory_kib);
}

/// Checks that its input file (`held.inp`, else the box file `input`) starts with `h`, then holds 20 MiB in
/// that file: written in place if it can open it for writing (after a `chmod` if needed), else in a new file
/// of that name. Holds 56 MiB of touched heap with it and reads the 20 MiB back.
const HELD_IN_INPUT: &str = "#include <cstdlib>\n#include <cstring>\n#include <fcntl.h>\n#include <sys/stat.h>\n\
#include <unistd.h>\nstatic char block[1 << 20];\nchar *volatile heap;\n\
int main(){ const char *name = access(\"held.inp\", F_OK) == 0 ? \"held.inp\" : \"input\";\n\
int fd = open(name, O_RDONLY); if (fd < 0 || read(fd, block, 1) != 1 || block[0] != 'h') return 2; close(fd);\n\
fd = open(name, O_RDWR); if (fd < 0 && chmod(name, 0600) == 0) fd = open(name, O_RDWR);\n\
if (fd < 0) { unlink(name); fd = open(name, O_RDWR | O_CREAT | O_TRUNC, 0600); } if (fd < 0) return 3;\n\
for (int i = 0; i < 20; i++) { memset(block, i + 1, sizeof block); if (pwrite(fd, block, sizeof block, (off_t)i << 20) != sizeof block) return 4; }\n\
heap = (char*)malloc(56 << 20); if (!heap) return 5; memset(heap, 1, 56 << 20); long sum = 0;\n\
for (int i = 0; i < 20; i++) { if (pread(fd, block, sizeof block, (off_t)i << 20) != sizeof block) return 6; sum += block[7] + heap[(size_t)i << 20]; }\n\
return sum == 230 ? 0 : 10; }\n";

#[test]
fn input_file_memory_is_counted() {
    // 76 MiB held at once under a 64 MiB limit, inside the cgroup's 24 MiB page-cache room: no OOM kill.
    let Some(mut f) = fixture(64, "CPP17", HELD_IN_INPUT) else { return };
    let file_io = SolutionFileIo::new(&FileIo { input: "held.inp".into(), output: "held.out".into() }, b"");
    for file_io in [None, Some(&file_io)] {
        let file = input(&f, &padded("h", 20 << 20));
        let io = CaseIo { input: &file, output_cap: 24 << 20, file_io };
        let run = go_limited(&mut f, &io, 65_536);
        let mode = if file_io.is_some() { "file I/O" } else { "batch" };
        assert_eq!(run.verdict, RunVerdict::Mle, "{mode}: memory {} KiB", run.memory_kib);
        assert!(run.memory_kib > 65_536, "{mode}: memory {} KiB", run.memory_kib);
    }
}

/// Reads `heap out` from `fio.inp`, touches `heap` MiB, then writes `out` MiB and the answer to `fio.out`.
const FILE_IO_HONEST: &str = "#include <cstdio>\n#include <cstdlib>\n#include <cstring>\nstatic char block[1 << 16];\n\
int main(){ FILE *in = fopen(\"fio.inp\", \"r\"); int heap, out; if (!in || fscanf(in, \"%d %d\", &heap, &out) != 2) return 2;\n\
char *p = (char*)malloc((size_t)heap << 20); memset(p, 1, (size_t)heap << 20); FILE *o = fopen(\"fio.out\", \"w\");\n\
memset(block, 'x', sizeof block); for (int i = 0; i < out << 4; i++) fwrite(block, 1, sizeof block, o);\n\
fprintf(o, \"%d\\n\", p[heap]); return fclose(o); }\n";

#[test]
fn file_io_answer_memory_is_bounded() {
    let Some(mut f) = fixture(63, "CPP17", FILE_IO_HONEST) else { return };
    let file_io = SolutionFileIo::new(&FileIo { input: "fio.inp".into(), output: "fio.out".into() }, b"");
    // (heap, output) MiB under (limit) KiB: the output is 4 to 5 times the limit
    for (head, limit) in [("12 64", 16_384), ("26 64", 32_768)] {
        let file = input(&f, head);
        let io = CaseIo { input: &file, output_cap: 80 << 20, file_io: Some(&file_io) };
        let run = go_limited(&mut f, &io, limit);
        assert_eq!(run.verdict, RunVerdict::Ok, "{head} at {limit} KiB: memory {} KiB", run.memory_kib);
        assert_eq!(run.output_len, (64 << 20) + 2);
        assert!(run.memory_kib < limit, "{head}: memory {} KiB", run.memory_kib);
    }
}

/// Reads `heap out err` and the rest of its input, touches `heap` MiB, writes `err` MiB to stderr and `out`
/// MiB to stdout.
const HONEST: &str = "#include <cstdio>\n#include <cstdlib>\n#include <cstring>\n#include <unistd.h>\n\
static char block[1 << 16];\n\
int main(){ int heap, out, err; if (scanf(\"%d %d %d\", &heap, &out, &err) != 3) return 2; long in = 0, r;\n\
while ((r = read(0, block, sizeof block)) > 0) in += r;\n\
char *p = (char*)malloc((size_t)heap << 20); memset(p, 1, (size_t)heap << 20); memset(block, 'x', sizeof block);\n\
for (long i = 0; i < (long)err << 4; i++) if (write(2, block, sizeof block) < 0) return 3;\n\
for (long i = 0; i < (long)out << 4; i++) fwrite(block, 1, sizeof block, stdout);\n\
printf(\"%ld %d\", in, p[heap]); }\n";

const PY_HONEST: &str = "import sys\nheap, out, err = map(int, sys.stdin.buffer.readline().split())\n\
data = sys.stdin.buffer.read()\nheld = b'\\x01' * (heap << 20)\nblock = b'x' * 65536\n\
for _ in range(err * 16): sys.stderr.buffer.write(block)\n\
for _ in range(out * 16): sys.stdout.buffer.write(block)\n\
print(len(data), held[-1])\n";

/// What an honest run may use beyond its heap: a runtime, and KiB per MiB of input it keeps.
struct Overhead {
    runtime_kib: u64,
    kib_per_input_mib: u64,
}

/// One honest run per row (`heap out err` MiB, input MiB, output cap MiB) under a 128 MiB limit: each is Ok
/// and its memory is at most its heap plus `overhead`, whatever page cache its input, output and stderr hold.
fn honest_runs(offset: u32, (key, source): (&str, &str), overhead: Overhead) {
    let Some(mut f) = fixture(offset, key, source) else { return };
    let rows = [
        ("4 0 0", 0, 24),
        ("112 23 0", 0, 24),
        ("4 0 15", 0, 24),
        ("4 20 15", 0, 24),
        ("4 0 0", 30, 24),
        ("4 96 0", 0, 100),
    ];
    for (head, input_mib, cap_mib) in rows {
        let file = input(&f, &padded(&format!("{head}\n"), input_mib << 20));
        let run = go_limited(&mut f, &plain(&file, cap_mib << 20), 131_072);
        eprintln!("{key} {head} input {input_mib} MiB: {:?} memory {} KiB", run.verdict, run.memory_kib);
        assert_eq!(
            run.verdict,
            RunVerdict::Ok,
            "{key} {head} input {input_mib} MiB: memory {}",
            run.memory_kib
        );
        let heap_kib = head.split(' ').next().unwrap().parse::<u64>().unwrap() << 10;
        let most = heap_kib + overhead.runtime_kib + overhead.kib_per_input_mib * input_mib as u64;
        assert!(run.memory_kib <= most, "{key} {head} input {input_mib} MiB: memory {} KiB", run.memory_kib);
    }
}

#[test]
fn honest_runs_memory_is_bounded() {
    // 112 MiB of heap under 128 MiB with 23 MiB of output; 15 MiB of stderr, alone and with 20 MiB of
    // output; a 30 MiB input read; 96 MiB of output, past the cgroup's page-cache room.
    // The C++ runtime is ~3 MiB; on main the 96 MiB-output row reached 8.8 MiB under host load.
    honest_runs(53, ("CPP17", HONEST), Overhead { runtime_kib: 8 << 10, kib_per_input_mib: 0 });
    // CPython's runtime, and the input it read held as bytes (twice while `read` joins its chunks)
    honest_runs(54, ("PY3", PY_HONEST), Overhead { runtime_kib: 16 << 10, kib_per_input_mib: 2 << 10 });
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

/// A `file_io` output of 22 MiB under a 24 MiB cap, and one exactly at a 22 MiB cap, are judged in full with a
/// small disk quota headroom: the box's quota holds an output of the run's `--fsize`.
#[test]
fn a_file_io_output_up_to_the_cap_is_judged_in_full_over_a_small_disk_quota_headroom() {
    let src = "open('big.out', 'w').write('7' * (22 << 20))\n";
    let Some(mut f) = fixture(108, "PY3", src) else { return };
    let meta = tempfile::tempdir().unwrap();
    let Some((config, quota_sandbox)) = quota_box(109, meta.path(), 1024) else { return };
    f.sandbox = quota_sandbox;
    let file_io = SolutionFileIo::new(&FileIo { input: "big.inp".into(), output: "big.out".into() }, b"");
    let file = input(&f, "");
    let mut outputs = Vec::new();
    for cap in [24 << 20, 22 << 20] {
        let io = CaseIo { input: &file, output_cap: cap, file_io: Some(&file_io) };
        let (run, out) = go(&mut f, &io, "out");
        outputs.push((run.verdict, std::fs::metadata(&out).unwrap().len()));
    }
    drop(f);
    let no_limit = DiskQuota { headroom_kib: i32::MAX as u32, headroom_inodes: i32::MAX as u32 };
    disk_quota_works(&SandboxConfig { disk_quota: Some(no_limit), ..config }, box_id(109)).unwrap();
    assert_eq!(outputs, [(RunVerdict::Ok, 22 << 20), (RunVerdict::Ok, 22 << 20)]);
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

/// Reads `n then`, writes `n` bytes of `x` to stdout, then: `exit`, `alloc` (touches 200 MiB), `spin`, or
/// `flood` (writes forever).
const WRITE_THEN: &str = "#include <cstdio>\n#include <cstdlib>\n#include <cstring>\nstatic char block[1 << 16];\n\
int main(){ long n; char then[8]; if (scanf(\"%ld %7s\", &n, then) != 2) return 2; memset(block, 'x', sizeof block);\n\
for (long left = n; left > 0; left -= (long)sizeof block) fwrite(block, 1, left < (long)sizeof block ? left : sizeof block, stdout);\n\
fflush(stdout);\n\
if (then[0] == 'a') { char *p = (char*)malloc(200 << 20); memset(p, 1, 200 << 20); return p[5] - 1; }\n\
if (then[0] == 's') { volatile unsigned long k = 0; for (;;) k++; }\n\
if (then[0] == 'f') for (;;) fwrite(block, 1, sizeof block, stdout);\n\
return 0; }\n";

#[test]
fn output_past_the_cap_is_ole_after_memory_and_ends_a_flood_at_once() {
    let Some(mut f) = fixture(55, "CPP17", WRITE_THEN) else { return };
    let cap = 4096;
    // (input, verdict, output bytes kept)
    let rows = [
        ("4096 exit", RunVerdict::Ok, 4096),
        ("4097 exit", RunVerdict::Ole, 4097),
        ("4608 alloc", RunVerdict::Mle, 4097),
        ("4608 spin", RunVerdict::Ole, 4097),
        ("0 flood", RunVerdict::Ole, 4097),
    ];
    for (text, verdict, kept) in rows {
        let file = input(&f, text);
        let (run, out) = go(&mut f, &plain(&file, cap), "out");
        eprintln!(
            "{text}: {:?} {} B {:.3} s {} KiB",
            run.verdict, run.output_len, run.time_s, run.memory_kib
        );
        assert_eq!((&run.verdict, run.output_len), (&verdict, kept), "{text}");
        assert_eq!(std::fs::read(&out).unwrap(), vec![b'x'; kept as usize], "{text}");
        if text.ends_with("flood") {
            assert!(run.time_s < 0.5, "a flood ends at the file size limit: {} s", run.time_s);
        }
    }
}

#[test]
fn the_output_reaches_the_checker_byte_for_byte() {
    let src =
        "import sys\nsys.stdout.buffer.write(bytes(range(256)) + b'\\r\\n\\x00end' if input() else b'')\n";
    let Some(mut f) = fixture(56, "PY3", src) else { return };
    let file = input(&f, "binary\n");
    let (run, out) = go(&mut f, &plain(&file, 1 << 20), "out");
    let mut expected: Vec<u8> = (0..=255).collect();
    expected.extend(b"\r\n\x00end");
    assert_eq!((run.verdict, std::fs::read(&out).unwrap()), (RunVerdict::Ok, expected));
    let file = input(&f, "\n");
    let (run, out) = go(&mut f, &plain(&file, 1 << 20), "out");
    assert_eq!((run.verdict, run.output_len, std::fs::read(&out).unwrap()), (RunVerdict::Ok, 0, vec![]));
}

/// Writes `junk` MiB to stderr, then `raise` (1 // 0) or `print`s 1.
const PY_STDERR_THEN: &str = "import sys\njunk, then = input().split()\nblock = b'e' * 65536\n\
for _ in range(int(junk) * 16): sys.stderr.buffer.write(block)\nsys.stderr.flush()\n\
print(1 // 0 if then == 'raise' else 1)\n";

#[test]
fn a_kept_stderr_is_searched_in_its_first_16_mib_and_cut_past_its_allowance() {
    let Some(mut f) = fixture(57, "PY3", PY_STDERR_THEN) else { return };
    let rows = [
        ("15 raise", RunVerdict::Rte { reason: "ZeroDivisionError".into() }),
        ("20 raise", RunVerdict::Ir { exit_code: 120 }),
        ("20 print", RunVerdict::Ir { exit_code: 120 }),
        ("40 raise", RunVerdict::Ir { exit_code: 120 }),
    ];
    let mut seen = Vec::new();
    for (text, _) in &rows {
        let file = input(&f, text);
        let (run, out) = go(&mut f, &plain(&file, 1 << 20), "out");
        let output = std::fs::read_to_string(&out).unwrap();
        eprintln!("{text}: {:?} {output:?} {:.3} s {} KiB", run.verdict, run.time_s, run.memory_kib);
        seen.push((*text, run.verdict));
    }
    assert_eq!(seen, rows);
}

#[test]
fn a_jvm_program_whose_stderr_floods_past_its_allowance_still_finishes() {
    let Some(language) = java_language() else { return };
    let src = "public class Main { public static void main(String[] a) { byte[] b = new byte[65536]; \
               java.util.Arrays.fill(b, (byte) 'e'); for (int i = 0; i < 40 * 16; i++) System.err.write(b, 0, b.length); \
               System.err.flush(); System.out.println(1); } }";
    let mut f = fixture_with(58, &language, src);
    let file = input(&f, "");
    let (run, out) = go(&mut f, &plain(&file, 1 << 20), "out");
    let output = std::fs::read_to_string(&out).unwrap();
    eprintln!("java: {:?} {output:?} {:.3} s {} KiB", run.verdict, run.time_s, run.memory_kib);
    assert_eq!((run.verdict, output.as_str()), (RunVerdict::Ok, "1\n"));
}

/// Prints what it sees of its stdin: `fstat` size, the end by `lseek`, the size read again through
/// `/dev/stdin`, and its first byte through a shared read-only mapping.
const STDIN_VIEW: &str = "#include <cstdio>\n#include <fcntl.h>\n#include <sys/mman.h>\n#include <sys/stat.h>\n#include <unistd.h>\n\
static char block[1 << 16];\n\
int main(){ struct stat st; if (fstat(0, &st) != 0) return 2; long end = lseek(0, 0, SEEK_END);\n\
int again = open(\"/dev/stdin\", O_RDONLY); long n = 0, r; while ((r = read(again, block, sizeof block)) > 0) n += r;\n\
const char *p = (const char*)mmap(nullptr, st.st_size, PROT_READ, MAP_SHARED, 0, 0); if (p == MAP_FAILED) return 3;\n\
printf(\"%ld %ld %ld %c\", (long)st.st_size, end, n, p[0]); }\n";

const PY_STDIN_VIEW: &str = "import os, sys\nsize = os.fstat(0).st_size\nend = os.lseek(0, 0, os.SEEK_END)\n\
again = open('/dev/stdin', 'rb').read()\nos.lseek(0, 0, os.SEEK_SET)\nfirst = sys.stdin.read(1)\n\
print(size, end, len(again), first)\n";

#[test]
fn stdin_is_a_seekable_mappable_file_of_the_input() {
    let text = "q".repeat(100_000);
    for (offset, key, source) in [(59, "CPP17", STDIN_VIEW), (60, "PY3", PY_STDIN_VIEW)] {
        let Some(mut f) = fixture(offset, key, source) else { continue };
        let file = input(&f, &text);
        let (run, out) = go(&mut f, &plain(&file, 1 << 20), "out");
        let seen = std::fs::read_to_string(&out).unwrap();
        assert_eq!((run.verdict, seen.trim_end()), (RunVerdict::Ok, "100000 100000 100000 q"), "{key}");
    }
}

/// Tries to change its stdin by every route: `write`, `pwrite`, `ftruncate`, `fallocate`, a shared writable
/// mapping, and a write through `/proc/self/fd/0` reopened for writing. Prints how many worked.
const WRITE_STDIN: &str = "#include <cstdio>\n#include <fcntl.h>\n#include <sys/mman.h>\n#include <unistd.h>\n\
int main(){ int worked = 0; worked += write(0, \"x\", 1) == 1; worked += pwrite(0, \"x\", 1, 0) == 1;\n\
worked += ftruncate(0, 1 << 16) == 0; worked += fallocate(0, 0, 0, 1 << 16) == 0;\n\
worked += mmap(nullptr, 4096, PROT_READ | PROT_WRITE, MAP_SHARED, 0, 0) != MAP_FAILED;\n\
int again = open(\"/proc/self/fd/0\", O_RDWR); worked += again >= 0 && write(again, \"x\", 1) == 1;\n\
printf(\"%d\", worked); }\n";

#[test]
fn the_box_cannot_change_or_grow_its_stdin() {
    let Some(mut f) = fixture(61, "CPP17", WRITE_STDIN) else { return };
    let file = input(&f, "untouched\n");
    let (run, out) = go(&mut f, &plain(&file, 1 << 20), "out");
    assert_eq!((run.verdict, std::fs::read_to_string(&out).unwrap()), (RunVerdict::Ok, "0".to_string()));
}
