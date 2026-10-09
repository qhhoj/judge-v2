//! One case of the solution in a fresh box (Review Focus 1).

use crate::error::GradeError;
use crate::languages::{Placeholders, expand};
use crate::program::Program;
use judge_problem::init::FileIo;
use judge_sandbox::IsolateBox;
use judge_sandbox::classify::{OutputUse, RunVerdict, classify};
use judge_sandbox::spec::{Limits, RunSpec, Stdio};
use std::fs::File;
use std::io::Read;
use std::os::fd::AsRawFd;
use std::path::{Component, Path, PathBuf};

pub const MAX_PREFIX_BYTES: u64 = 512;
/// A reason (signal or exception name) is feedback, so it is clipped like the 50-byte feedback.
const MAX_REASON_BYTES: usize = 50;
/// How much of an interpreter's or JVM's stderr is looked at for exceptions.
const MAX_STDERR_BYTES: u64 = 64 << 10;
const STDERR_FILE: &str = ".stderr";

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
    pub file_io: Option<&'a FileIo>,
}

#[derive(Debug, Clone)]
pub struct CaseRun {
    pub verdict: RunVerdict,
    pub time_s: f64,
    pub memory_kib: u64,
    pub output: PathBuf,
    pub output_len: u64,
}

pub fn cgroup_memory(limits: &CaseLimits, jvm: bool) -> u64 {
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

/// Box names of the case input and output, and the stdio that connects them.
struct Layout {
    input: String,
    output: String,
    stdio: Stdio,
}

fn layout(program: &Program, file_io: Option<&FileIo>) -> Layout {
    let keeps_stderr = program.jvm || program.exception_marker.is_some();
    let stderr = Some(if keeps_stderr { STDERR_FILE } else { "/dev/null" }.to_string());
    match file_io {
        Some(io) => Layout {
            input: io.input.clone(),
            output: io.output.clone(),
            stdio: Stdio { stdin: Some("/dev/null".into()), stdout: Some("/dev/null".into()), stderr },
        },
        None => Layout {
            input: "input".into(),
            output: "output".into(),
            stdio: Stdio { stdin: Some("input".into()), stdout: Some("output".into()), stderr },
        },
    }
}

fn is_plain(name: &str) -> bool {
    matches!(Path::new(name).components().next(), Some(Component::Normal(part)) if part == name)
}

pub fn run_case(
    sandbox: &mut IsolateBox,
    runnable: &Runnable,
    io: &CaseIo,
    output: &Path,
) -> Result<CaseRun, GradeError> {
    let (program, limits) = (runnable.program, runnable.limits);
    let layout = layout(program, io.file_io);
    if !is_plain(&layout.input) || !is_plain(&layout.output) {
        return Err(GradeError::Io(format!(
            "file_io name {:?}/{:?} is not a plain file name",
            layout.input, layout.output
        )));
    }
    sandbox.reset()?;
    sandbox.put_tree(&program.dir)?;
    // The handle's own inode, reached through the kernel's fd link; nothing is looked up by path.
    sandbox.put(&layout.input, Path::new(&format!("/proc/self/fd/{}", io.input.as_raw_fd())), false)?;
    let memory = Placeholders { memory_kib: limits.memory_kib, class: None };
    let spec = RunSpec {
        argv: expand(&program.run, &memory),
        limits: Limits::for_solution(
            limits.time_limit_s,
            cgroup_memory(limits, program.jvm),
            program.processes,
            io.output_cap,
        ),
        stdio: layout.stdio,
        env: program.env.clone(),
        dirs: program.dirs.clone(),
    };
    let meta = sandbox.run(&spec)?;
    // One byte past the cap tells "exactly the cap" (fine) from "over it" (OLE).
    let written = sandbox.read_output(&layout.output, io.output_cap + 1).unwrap_or_default();
    std::fs::write(output, &written)?;
    let output_len = written.len() as u64;
    let verdict = classify(&meta, OutputUse { written: output_len, cap: io.output_cap });
    let verdict = refine(verdict, program, sandbox);
    Ok(CaseRun {
        verdict,
        time_s: meta.time_s,
        memory_kib: meta.cg_mem_kib,
        output: output.to_path_buf(),
        output_len,
    })
}

/// JVM OutOfMemoryError → MLE; an uncaught exception (the language's marker on stderr) → RTE named by
/// the exception (DMOJ reports these as RTE, not IR; spec §11).
fn refine(verdict: RunVerdict, program: &Program, sandbox: &IsolateBox) -> RunVerdict {
    if !matches!(verdict, RunVerdict::Rte { .. } | RunVerdict::Ir { .. }) {
        return verdict;
    }
    let bytes = sandbox.read_output(STDERR_FILE, MAX_STDERR_BYTES).unwrap_or_default();
    let stderr = String::from_utf8_lossy(&bytes);
    if program.jvm && stderr.contains("java.lang.OutOfMemoryError") {
        return RunVerdict::Mle;
    }
    let reason = program.exception_marker.as_deref().and_then(|marker| exception_reason(&stderr, marker));
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

/// Names the uncaught exception when stderr carries the language's marker, else None.
/// - text after the marker names it (`Exception in thread "main" java.lang.ArithmeticException: x`,
///   `raised CONSTRAINT_ERROR : x`): its first word, last `.` segment;
/// - a bare header (`Traceback (most recent call last):`): the last line when the header opens stderr
///   (Python's `ZeroDivisionError: division by zero`), else the line above it (Lua's message).
pub fn exception_reason(stderr: &str, marker: &str) -> Option<String> {
    let lines: Vec<&str> = stderr.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    let at = lines.iter().position(|l| l.contains(marker))?;
    let after = lines[at].split_once(marker).map_or("", |(_, rest)| rest);
    let after = after.trim_start_matches([':', ' ']);
    if !after.is_empty() {
        return Some(clip(&named_by(after)));
    }
    let line = if at == 0 { lines.last()? } else { &lines[at - 1] };
    Some(clip(line.split(':').next().unwrap_or(line)))
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
    fn a_python_traceback_is_named_by_its_last_line() {
        let python = "Traceback (most recent call last):\n  File \"main.py\", line 1\nZeroDivisionError: division by zero\n";
        assert_eq!(exception_reason(python, PYTHON).as_deref(), Some("ZeroDivisionError"));
        assert_eq!(exception_reason("plain exit\n", PYTHON), None);
    }

    #[test]
    fn a_java_exception_is_named_by_its_class_not_its_stack() {
        let java = "Exception in thread \"main\" java.lang.ArithmeticException: / by zero\n\tat Main.main(Main.java:3)\n";
        assert_eq!(exception_reason(java, "Exception in thread").as_deref(), Some("ArithmeticException"));
        let custom = "Exception in thread \"Thread-0\" MyError\n";
        assert_eq!(exception_reason(custom, "Exception in thread").as_deref(), Some("MyError"));
    }

    #[test]
    fn ada_and_lua_exceptions_are_named() {
        let ada = "\nraised CONSTRAINT_ERROR : main.adb:3 range check failed\n";
        assert_eq!(exception_reason(ada, "raised ").as_deref(), Some("CONSTRAINT_ERROR"));
        let lua = "lua5.4: main.lua:1: attempt to index a nil value\nstack traceback:\n\t[C]: in ?\n";
        let reason = exception_reason(lua, "stack traceback:").unwrap();
        assert_eq!(reason, "lua5.4");
    }

    #[test]
    fn a_reason_is_clipped_on_a_char_boundary() {
        let stderr = format!("Traceback (most recent call last):\n{}\n", "é".repeat(100));
        let reason = exception_reason(&stderr, PYTHON).unwrap();
        assert!(reason.len() <= MAX_REASON_BYTES && reason.chars().all(|c| c == 'é'));
    }

    #[test]
    fn jvm_memory_gets_the_reserve() {
        let limits = CaseLimits { time_limit_s: 1.0, memory_kib: 65_536, jvm_reserve_kib: 131_072 };
        assert_eq!(cgroup_memory(&limits, true), 196_608);
        assert_eq!(cgroup_memory(&limits, false), 65_536);
    }

    #[test]
    fn only_plain_names_are_box_files() {
        assert!(is_plain("sum.inp"));
        for bad in ["", "a/b", "../x", "/etc/passwd", "."] {
            assert!(!is_plain(bad), "{bad:?}");
        }
    }
}
