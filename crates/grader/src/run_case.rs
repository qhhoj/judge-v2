//! One case of the solution in a fresh box (Review Focus 1).

use crate::error::GradeError;
use crate::file_io::SolutionFileIo;
use crate::languages::{ExceptionStyle, Placeholders, expand};
use crate::program::Program;
use judge_problem::init::is_plain_box_name;
use judge_sandbox::classify::{OutputUse, RunVerdict, classify};
use judge_sandbox::meta::Meta;
use judge_sandbox::spec::{Limits, RunSpec, Stdio};
use judge_sandbox::{IsolateBox, Room};
use std::fs::File;
use std::io::Read;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};

pub const MAX_PREFIX_BYTES: u64 = 512;
/// A reason (signal or exception name) is feedback, so it is clipped like the 50-byte feedback.
const MAX_REASON_BYTES: usize = 50;
/// Extra file size a run with a kept stderr may write beyond the output cap, and how much of the end of
/// that stderr is searched for the exception.
const STDERR_ALLOWANCE: u64 = 16 << 20;
const MAX_STDERR_TAIL: usize = 64 << 10;
pub(crate) const STDERR_FILE: &str = ".stderr";

#[derive(Debug, Clone)]
pub struct CaseLimits {
    pub time_limit_s: f64,
    pub memory_kib: u64,
    pub jvm_reserve_kib: u64,
}

pub struct Runnable<'a> {
    pub program: &'a Program,
    pub limits: &'a CaseLimits,
}

/// `input` is the handle `ResolvedProblem::open_case_file` returned; it is copied from, never reopened by path.
pub struct CaseIo<'a> {
    pub input: &'a File,
    pub output_cap: u64,
    pub file_io: Option<&'a SolutionFileIo>,
}

#[derive(Debug, Clone)]
pub struct CaseRun {
    pub verdict: RunVerdict,
    pub time_s: f64,
    pub memory_kib: u64,
    pub output: PathBuf,
    pub output_len: u64,
}

/// The memory limit a run is held to: the case limit, plus the JVM reserve for a JVM program.
pub fn memory_limit_kib(limits: &CaseLimits, jvm: bool) -> u64 {
    limits.memory_kib + if jvm { limits.jvm_reserve_kib } else { 0 }
}

/// At most `bytes` (and never more than 512) of the file, lossy UTF-8; a missing file is empty.
pub fn read_prefix(path: &Path, bytes: u64) -> String {
    let mut buffer = Vec::new();
    if let Ok(file) = File::open(path) {
        let _ = file.take(bytes.min(MAX_PREFIX_BYTES)).read_to_end(&mut buffer);
    }
    String::from_utf8_lossy(&buffer).into_owned()
}

/// Box names of the case input (and its aliases) and output, the stdio that connects them, and whether the
/// output may be in any letter case.
struct Layout {
    input: String,
    aliases: Vec<String>,
    output: String,
    output_any_case: bool,
    stdio: Stdio,
}

fn layout(program: &Program, file_io: Option<&SolutionFileIo>) -> Layout {
    let keeps_stderr = program.jvm || program.exception_marker.is_some();
    let stderr = Some(if keeps_stderr { STDERR_FILE } else { "/dev/null" }.to_string());
    match file_io {
        Some(io) => Layout {
            input: io.input.clone(),
            aliases: io.aliases.clone(),
            output: io.output.clone(),
            output_any_case: true,
            stdio: Stdio { stdin: Some("/dev/null".into()), stdout: Some("/dev/null".into()), stderr },
        },
        None => Layout {
            input: "input".into(),
            aliases: Vec::new(),
            output: "output".into(),
            output_any_case: false,
            stdio: Stdio { stdin: Some("input".into()), stdout: Some("output".into()), stderr },
        },
    }
}

/// A `file_io` name is data from init.yml: it must be a plain, unreserved box file name (defence in depth;
/// `parse_init` refuses the same names).
fn check_file_io(file_io: &SolutionFileIo) -> Result<(), GradeError> {
    match [&file_io.input, &file_io.output].into_iter().find(|name| !is_plain_box_name(name)) {
        Some(bad) => Err(GradeError::Io(format!("file_io name {bad:?} is not a plain box file name"))),
        None => Ok(()),
    }
}

/// stderr is kept (in a file next to the output) for JVM programs and languages with an exception marker.
pub(crate) fn keeps_stderr(program: &Program) -> bool {
    program.jvm || program.exception_marker.is_some()
}

pub(crate) fn solution_limits(runnable: &Runnable, output_cap: u64) -> Limits {
    let (program, limits) = (runnable.program, runnable.limits);
    // isolate has one --fsize for every file the run writes. A kept stderr gets its own allowance on top of
    // the output cap, so a chatty correct program is not killed or failed; stdout is still held to the cap
    // by the `written > cap` check, and stderr never decides the verdict.
    let fsize = output_cap + if keeps_stderr(program) { STDERR_ALLOWANCE } else { 0 };
    Limits::for_solution(limits.time_limit_s, memory_limit_kib(limits, program.jvm), program.processes, fsize)
}

fn run_spec(runnable: &Runnable, layout: Layout, output_cap: u64) -> RunSpec {
    let program = runnable.program;
    let memory = Placeholders { memory_kib: runnable.limits.memory_kib, class: None };
    RunSpec {
        argv: expand(&program.run, &memory),
        limits: solution_limits(runnable, output_cap),
        stdio: layout.stdio,
        env: program.env.clone(),
        dirs: program.dirs.clone(),
    }
}

/// Resets the box and puts the program and the case input in it, the input also under each alias that
/// names no program file. The box's disk quota also holds both and an output of `output_cap` bytes.
fn stage(
    sandbox: &mut IsolateBox,
    program: &Program,
    (input, output_cap): (&File, u64),
    layout: &Layout,
) -> Result<(), GradeError> {
    let room = Room::for_files(&[&program.dir])?.plus_file(input.metadata()?.len()).plus_file(output_cap);
    sandbox.reset_with_room(room)?;
    sandbox.put_tree(&program.dir)?;
    // The handle's own inode, reached through the kernel's fd link; nothing is looked up by path.
    let handle = format!("/proc/self/fd/{}", input.as_raw_fd());
    sandbox.put(&layout.input, Path::new(&handle), false)?;
    for alias in &layout.aliases {
        sandbox.link(&layout.input, alias)?;
    }
    Ok(())
}

/// The box name the output is read from: with file I/O, the program may have spelled it in any letter case.
fn output_name(sandbox: &IsolateBox, layout_output: &str, any_case: bool) -> String {
    let found = any_case.then(|| sandbox.find_ignoring_case(layout_output).ok().flatten()).flatten();
    found.unwrap_or_else(|| layout_output.to_string())
}

pub fn run_case(
    sandbox: &mut IsolateBox,
    runnable: &Runnable,
    io: &CaseIo,
    output: &Path,
) -> Result<CaseRun, GradeError> {
    let layout = layout(runnable.program, io.file_io);
    if let Some(file_io) = io.file_io {
        check_file_io(file_io)?;
    }
    stage(sandbox, runnable.program, (io.input, io.output_cap), &layout)?;
    let (box_output, any_case) = (layout.output.clone(), layout.output_any_case);
    let spec = run_spec(runnable, layout, io.output_cap);
    let meta = sandbox.run(&spec)?;
    let output_name = output_name(sandbox, &box_output, any_case);
    // One byte past the cap tells "exactly the cap" (fine) from "over it" (OLE).
    let written = sandbox.read_output(&output_name, io.output_cap + 1).unwrap_or_default();
    std::fs::write(output, &written)?;
    let output_len = written.len() as u64;
    let own_files = own_files_kib(sandbox, runnable.program, io.input.metadata()?.len(), Some(&output_name));
    let usage = OutputUse { written: output_len, cap: io.output_cap, own_files_kib: Some(own_files) };
    let verdict = refine(classify(&meta, usage, &spec.limits), runnable.program, sandbox);
    Ok(CaseRun {
        memory_kib: reported_memory(&meta, usage.own_files_kib, &verdict),
        verdict,
        time_s: meta.time_s,
        output: output.to_path_buf(),
        output_len,
    })
}

/// The memory shown for a run: the program's own (`Meta::memory_kib`), but for an MLE the larger of the
/// cgroup peak and the peak RSS, so an MLE is never shown under the limit: one `refine` found on stderr (a
/// JVM OutOfMemoryError, cgroup peak) or one by the program's own memory over the limit, no OOM kill.
pub(crate) fn reported_memory(meta: &Meta, own_files_kib: Option<u64>, verdict: &RunVerdict) -> u64 {
    if *verdict == RunVerdict::Mle {
        meta.cg_mem_kib.max(meta.max_rss_kib)
    } else {
        meta.memory_kib(own_files_kib)
    }
}

/// KiB of page cache `bytes` of a file take, in whole 4 KiB pages.
fn page_cache_kib(bytes: u64) -> u64 {
    bytes.div_ceil(4096) * 4
}

/// The page cache the run's own files may have charged to its cgroup (`Meta::memory_kib`): the
/// `input_bytes` of case input it read, its `output` file and its kept stderr. The box files count by their
/// allocated blocks, so a sparse file the program made adds nothing.
pub(crate) fn own_files_kib(
    sandbox: &IsolateBox,
    program: &Program,
    input_bytes: u64,
    output: Option<&str>,
) -> u64 {
    let stderr = keeps_stderr(program).then_some(STDERR_FILE);
    let written = [output, stderr].into_iter().flatten().map(|name| sandbox.allocated_bytes(name)).sum();
    page_cache_kib(input_bytes) + page_cache_kib(written)
}

/// The last 64 KiB of the kept stderr (where the final exception is), lossy UTF-8.
fn stderr_tail(sandbox: &IsolateBox) -> String {
    let bytes = sandbox.read_output(STDERR_FILE, STDERR_ALLOWANCE).unwrap_or_default();
    let tail = &bytes[bytes.len().saturating_sub(MAX_STDERR_TAIL)..];
    String::from_utf8_lossy(tail).into_owned()
}

/// JVM OutOfMemoryError → MLE; an uncaught exception (the language's marker on stderr) → RTE named by
/// the exception (DMOJ reports these as RTE, not IR; spec §11).
pub(crate) fn refine(verdict: RunVerdict, program: &Program, sandbox: &IsolateBox) -> RunVerdict {
    if !matches!(verdict, RunVerdict::Rte { .. } | RunVerdict::Ir { .. }) || !keeps_stderr(program) {
        return verdict;
    }
    let stderr = stderr_tail(sandbox);
    if program.jvm && stderr.contains("java.lang.OutOfMemoryError") {
        return RunVerdict::Mle;
    }
    let marker = program.exception_marker.as_deref();
    let reason = marker.and_then(|marker| exception_reason(&stderr, marker, program.exception_style));
    match (verdict, reason) {
        (RunVerdict::Ir { .. }, Some(reason)) => RunVerdict::Rte { reason },
        (verdict, _) => verdict,
    }
}

fn clip(text: &str) -> String {
    let mut end = text.len().min(MAX_REASON_BYTES);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_string()
}

/// Names the uncaught exception when stderr carries the language's marker, else None. `style` says where
/// the name is (`ExceptionStyle`); only the traceback nearest the end counts for the header styles, so
/// text printed before it (warnings, debug output) is ignored.
pub fn exception_reason(stderr: &str, marker: &str, style: ExceptionStyle) -> Option<String> {
    let lines: Vec<&str> = stderr.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    let reason = match style {
        ExceptionStyle::After => {
            let line = lines.iter().find(|l| l.contains(marker))?;
            let after = line.split_once(marker)?.1.trim_start_matches([':', ' ']);
            (!after.is_empty()).then(|| named_by(after))
        }
        ExceptionStyle::LastLine => {
            let header = lines.iter().rposition(|l| l.contains(marker))?;
            let last = lines.last()?;
            (header + 1 < lines.len()).then(|| last.split(':').next().unwrap_or(last).to_string())
        }
        ExceptionStyle::LineAbove => {
            let header = lines.iter().position(|l| l.contains(marker))?;
            header.checked_sub(1).map(|above| strip_location(lines[above]).to_string())
        }
        ExceptionStyle::Parenthesized => {
            let line = lines.iter().find(|l| l.contains(marker))?;
            let inner = line.strip_suffix(')')?;
            Some(inner[inner.rfind('(')? + 1..].to_string())
        }
    };
    reason.filter(|r| !r.is_empty()).map(|r| clip(&r))
}

/// `lua5.4: main.lua:1: attempt to ...` → `attempt to ...`.
fn strip_location(line: &str) -> &str {
    let mut rest = line;
    for _ in 0..2 {
        match rest.split_once(": ") {
            Some((head, tail)) if !head.contains(' ') => rest = tail,
            _ => break,
        }
    }
    rest
}

/// First word of `text` after an optional quoted thread name, without a trailing `:` and package path.
fn named_by(text: &str) -> String {
    let text = match text.strip_prefix('"').and_then(|rest| rest.split_once('"')) {
        Some((_, rest)) => rest.trim_start(),
        None => text,
    };
    let word = text.split(|c: char| c.is_whitespace() || c == ':').next().unwrap_or(text);
    word.rsplit('.').next().unwrap_or(word).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    const PYTHON: &str = "Traceback (most recent call last)";

    #[test]
    fn an_mle_shows_the_larger_of_cgroup_peak_and_rss_and_other_runs_the_program_memory() {
        let meta = Meta { cg_mem_kib: 200_000, max_rss_kib: 180_000, ..Meta::default() };
        assert_eq!(reported_memory(&meta, Some(0), &RunVerdict::Mle), 200_000, "JVM OutOfMemoryError");
        assert_eq!(reported_memory(&meta, Some(20 << 10), &RunVerdict::Ok), 180_000, "20 MiB of output");
        assert_eq!(reported_memory(&meta, Some(0), &RunVerdict::Ok), 200_000, "unmapped memory counts");
        let rss_only = Meta { cg_mem_kib: 8_500, max_rss_kib: 9_000, ..Meta::default() };
        assert_eq!(
            reported_memory(&rss_only, Some(0), &RunVerdict::Mle),
            9_000,
            "an MLE by peak RSS shows the RSS"
        );
        assert_eq!(reported_memory(&rss_only, Some(0), &RunVerdict::Ok), 8_500);
    }

    #[test]
    fn prefix_is_capped_and_lossy() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("o");
        std::fs::write(&file, [b'a'; 2000]).unwrap();
        assert_eq!(read_prefix(&file, 10), "a".repeat(10));
        assert_eq!(read_prefix(&file, 100_000).len(), 512);
        std::fs::write(&file, [0xff, b'x']).unwrap();
        assert_eq!(read_prefix(&file, 64), "\u{fffd}x");
        assert_eq!(read_prefix(&dir.path().join("missing"), 64), "");
    }

    #[test]
    fn a_python_exception_is_named_by_the_last_traceback_line() {
        let python = |stderr: &str| exception_reason(stderr, PYTHON, ExceptionStyle::LastLine);
        let traceback =
            "Traceback (most recent call last):\n  File \"main.py\", line 1\nZeroDivisionError: x\n";
        assert_eq!(python(traceback).as_deref(), Some("ZeroDivisionError"));
        assert_eq!(python("plain exit\n"), None);
        assert_eq!(python("Traceback (most recent call last):\n"), None);
        let noise_first = "main.py:3: SyntaxWarning: invalid escape sequence '\\d'\n  re.compile('\\d')\ndbg\n\
                           Traceback (most recent call last):\n  File \"main.py\", line 5\nValueError: bad\n";
        assert_eq!(python(noise_first).as_deref(), Some("ValueError"), "text before the traceback");
        let chained = "Traceback (most recent call last):\nKeyError: 1\n\nDuring handling of the above exception, \
                       another exception occurred:\n\nTraceback (most recent call last):\nOSError: x\n";
        assert_eq!(python(chained).as_deref(), Some("OSError"));
    }

    #[test]
    fn java_ada_lua_and_ruby_exceptions_are_named() {
        use ExceptionStyle::{After, LineAbove, Parenthesized};
        let java = "Exception in thread";
        // (stderr, marker, style, reason)
        let table = [
            (
                "Exception in thread \"main\" java.lang.ArithmeticException: / by zero\n\tat M.main(M.java:3)\n",
                java,
                After,
                Some("ArithmeticException"),
            ),
            ("Exception in thread \"Thread-0\" MyError\n", java, After, Some("MyError")),
            (
                "\nraised CONSTRAINT_ERROR : main.adb:3 range check failed\n",
                "raised ",
                After,
                Some("CONSTRAINT_ERROR"),
            ),
            (
                "lua5.4: main.lua:1: attempt to index a nil value\nstack traceback:\n\t[C]: in ?\n",
                "stack traceback:",
                LineAbove,
                Some("attempt to index a nil value"),
            ),
            (
                "main.rb:1:in 'Integer#/': divided by 0 (ZeroDivisionError)\n\tfrom main.rb:1:in '<main>'\n",
                "main.rb:",
                Parenthesized,
                Some("ZeroDivisionError"),
            ),
            ("main.rb:1: note\n", "main.rb:", Parenthesized, None),
        ];
        for (stderr, marker, style, reason) in table {
            assert_eq!(exception_reason(stderr, marker, style).as_deref(), reason, "{stderr:?}");
        }
    }

    #[test]
    fn a_reason_is_clipped_on_a_char_boundary() {
        let stderr = format!("Traceback (most recent call last):\n{}\n", "é".repeat(100));
        let reason = exception_reason(&stderr, PYTHON, ExceptionStyle::LastLine).unwrap();
        assert!(reason.len() <= MAX_REASON_BYTES && reason.chars().all(|c| c == 'é'));
    }
}
