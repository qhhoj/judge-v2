//! Themis checkers (`bridged`, `type: themis`), as the old judge ran them.
//!
//! The checker takes no arguments. Its stdin holds two lines: the test directory (the case input and the
//! expected answer) and the contestant's directory (their output). It opens files there by names written in
//! its own source, so the judge lays out each file under every name the checker may use: the problem's file
//! I/O names and the `.inp`/`.out` string literals of the checker source, each in a few letter cases. stdout's
//! last non-empty line is the score, 0..1; the lines before it are the feedback. Any exit code but 0 is a
//! checker failure.

use super::custom::{MAX_EXTENDED, MAX_FEEDBACK, clip_text};
use super::{CheckError, CheckOutcome};
use judge_problem::init::{FileIo, is_plain_box_name};
use std::collections::BTreeSet;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

/// Box directories of the test files and of the contestant's output, as the checker reads them on stdin.
pub const TEST_DIR: &str = "test";
pub const WORK_DIR: &str = "work";
/// The checker's stdin inside the box.
pub const PATHS_FILE: &str = ".themis-paths";
/// Literals taken from a source at most; a checker names two or three files.
const MAX_LITERAL_NAMES: usize = 16;

/// Where the checker looks for the case files: input names (in the test directory) and output names (the
/// answer in the test directory, the contestant's output in theirs).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ThemisNames {
    pub inputs: BTreeSet<String>,
    pub outputs: BTreeSet<String>,
}

impl ThemisNames {
    pub fn is_empty(&self) -> bool {
        self.inputs.is_empty() || self.outputs.is_empty()
    }

    fn add(&mut self, name: &str) {
        let Some((stem, ext)) = name.rsplit_once('.') else { return };
        let set = match ext.to_ascii_lowercase().as_str() {
            "inp" => &mut self.inputs,
            "out" => &mut self.outputs,
            _ => return,
        };
        // Teachers mix cases (`Path.inp` set as file I/O, `Path.INP` opened by the checker).
        for stem in [stem.to_string(), stem.to_ascii_lowercase(), stem.to_ascii_uppercase()] {
            for ext in [ext.to_ascii_lowercase(), ext.to_ascii_uppercase()] {
                let variant = format!("{stem}.{ext}");
                if is_plain_box_name(&variant) {
                    set.insert(variant);
                }
            }
        }
    }
}

/// `"...name.inp"` / `"...name.out"` literals of a source, the part after the last `/` or `\`.
fn literal_names(source: &[u8]) -> Vec<String> {
    let text = String::from_utf8_lossy(source);
    let mut names = Vec::new();
    for literal in text.split('"').skip(1).step_by(2) {
        let name = literal.rsplit(['/', '\\']).next().unwrap_or(literal);
        let lower = name.to_ascii_lowercase();
        if (lower.ends_with(".inp") || lower.ends_with(".out")) && !names.iter().any(|n| n == name) {
            names.push(name.to_string());
        }
        if names.len() >= MAX_LITERAL_NAMES {
            break;
        }
    }
    names
}

/// The names to lay the case files out under: the problem's file I/O names and the checker source's literals.
pub fn names_for(file_io: Option<&FileIo>, source: &[u8]) -> ThemisNames {
    let mut names = ThemisNames::default();
    if let Some(io) = file_io {
        names.add(&io.input);
        names.add(&io.output);
    }
    for literal in literal_names(source) {
        names.add(&literal);
    }
    names
}

/// The checker's stdin: the two directories, absolute inside the box, without a trailing slash.
pub fn paths_text() -> String {
    format!("/box/{TEST_DIR}\n/box/{WORK_DIR}\n")
}

fn readable_copy(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::copy(from, to)?;
    std::fs::set_permissions(to, std::fs::Permissions::from_mode(0o644))
}

fn new_dir(path: &Path) -> std::io::Result<()> {
    // `create_dir` fails on anything already there, so nothing in the box is followed.
    std::fs::create_dir(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
}

/// Lays the case out in a fresh box directory `dir`: `test/<input names>`, `test/<output names>` with the
/// answer, `work/<output names>` with the contestant's output.
pub fn stage(dir: &Path, names: &ThemisNames, files: (&Path, &Path, &Path)) -> std::io::Result<()> {
    let (input, output, answer) = files;
    let (test, work) = (dir.join(TEST_DIR), dir.join(WORK_DIR));
    new_dir(&test)?;
    new_dir(&work)?;
    for name in &names.inputs {
        readable_copy(input, &test.join(name))?;
    }
    for name in &names.outputs {
        readable_copy(answer, &test.join(name))?;
        readable_copy(output, &work.join(name))?;
    }
    Ok(())
}

/// The checker's report: exit 0 and a score on the last non-empty stdout line, else a checker error.
pub fn interpret(code: Option<i32>, stdout: &str) -> Result<CheckOutcome, CheckError> {
    let fail =
        |what: String| CheckError(format!("themis checker {what}: {}", clip_text(stdout, MAX_EXTENDED)));
    match code {
        Some(0) => {}
        Some(code) => return Err(fail(format!("failed with exit code {code}"))),
        None => return Err(fail("crashed".into())),
    }
    let lines: Vec<&str> = stdout.lines().map(str::trim).filter(|line| !line.is_empty()).collect();
    let (score_line, message) = lines.split_last().ok_or_else(|| fail("printed no score".into()))?;
    let score = score_line
        .parse::<f64>()
        .ok()
        .filter(|score| score.is_finite())
        .ok_or_else(|| fail(format!("printed {score_line:?} instead of a score")))?;
    let fraction = score.clamp(0.0, 1.0);
    let message = message.join("\n");
    Ok(CheckOutcome {
        passed: fraction > 0.0,
        fraction,
        feedback: clip_text(message.lines().next().unwrap_or(""), MAX_FEEDBACK),
        extended: clip_text(&message, MAX_EXTENDED),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|name| name.to_string()).collect()
    }

    #[test]
    fn file_io_names_and_source_literals_give_every_letter_case() {
        let io = FileIo { input: "Path.inp".into(), output: "Path.out".into() };
        let names = names_for(Some(&io), b"");
        assert_eq!(
            names.inputs,
            set(&["PATH.INP", "PATH.inp", "Path.INP", "Path.inp", "path.INP", "path.inp"])
        );
        assert!(names.outputs.contains("Path.OUT") && names.outputs.contains("path.out"));
        let source = br#"sprintf(f, "%s/%s", dir, "chiendich2701.inp"); open(work + "\\chiendich2701.out"); puts("x.txt");"#;
        let names = names_for(None, source);
        assert!(names.inputs.contains("chiendich2701.inp") && names.inputs.contains("CHIENDICH2701.INP"));
        assert!(names.outputs.contains("chiendich2701.out"));
        assert!(!names.outputs.iter().chain(&names.inputs).any(|name| name.contains("txt")));
    }

    #[test]
    fn names_that_would_leave_or_shadow_the_box_are_dropped() {
        let names = names_for(None, br#""../evil.inp" ".out" "input.inp""#);
        assert!(names.inputs.contains("evil.inp") && names.inputs.contains("input.inp"));
        assert!(names.outputs.is_empty(), "{names:?}");
        assert!(names.is_empty(), "no output name: the checker cannot be laid out");
    }

    #[test]
    fn the_last_line_is_the_score_and_the_rest_is_feedback() {
        let ac = interpret(Some(0), "Dung\r\n1.0\r\n").unwrap();
        assert!(ac.passed && ac.fraction == 1.0 && ac.feedback == "Dung", "{ac:?}");
        let wa = interpret(Some(0), "Sai o dong 3\n0\n\n").unwrap();
        assert!(!wa.passed && wa.fraction == 0.0 && wa.feedback == "Sai o dong 3", "{wa:?}");
        let half = interpret(Some(0), "0.5").unwrap();
        assert!(half.passed && (half.fraction - 0.5).abs() < 1e-9 && half.feedback.is_empty());
        assert_eq!(interpret(Some(0), "2").unwrap().fraction, 1.0);
        assert_eq!(interpret(Some(0), "-1").unwrap().fraction, 0.0);
    }

    #[test]
    fn a_missing_contestant_file_reported_as_zero_is_wrong_answer() {
        let missing = interpret(Some(0), "Khong tim thay file output\n0").unwrap();
        assert!(!missing.passed && missing.fraction == 0.0);
        assert_eq!(missing.feedback, "Khong tim thay file output");
    }

    #[test]
    fn no_score_a_bad_score_or_a_failed_run_is_a_checker_error() {
        assert!(interpret(Some(0), "").is_err());
        assert!(interpret(Some(0), "ok\nfull").is_err());
        assert!(interpret(Some(0), "NaN").is_err());
        assert!(interpret(Some(1), "1").is_err());
        assert!(interpret(None, "1").is_err());
    }
}
