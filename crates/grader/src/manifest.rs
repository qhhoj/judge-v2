//! The conformance manifest (`tests/programs/manifest.toml`): the verdict each program gets on the a+b
//! problem under the limits below. Read by the isolate suite and by `qhhoj-judge selftest`; the
//! expectations only hold with exactly these cases and limits.

use crate::judge::{GraderDefaults, Outcome, Submission, SubmissionSource};
use serde::Deserialize;
use std::path::Path;

/// The a+b problem every row is judged on: (input, expected output) per case.
pub const CASES: [(&str, &str); 2] = [("1 2\n", "3\n"), ("4 5\n", "9\n")];
pub const TIME_LIMIT_S: f64 = 1.0;
pub const MEMORY_KIB: u64 = 65_536;
/// JVM languages need room for the VM itself.
pub const JVM_MEMORY_KIB: u64 = 262_144;
/// A 64 KiB output limit, so the OLE rows hit it long before the time limit.
pub const DEFAULTS: GraderDefaults =
    GraderDefaults { default_output_limit: 64 << 10, default_output_prefix: 512, jvm_reserve_kib: 131_072 };

#[derive(Deserialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct Row {
    pub lang: String,
    /// Relative to the manifest's directory.
    pub file: String,
    /// Every case's status code (`AC`, `TLE`, ...), or `CE`. Exactly one of `expect` and `cases`.
    #[serde(default)]
    expect: Option<String>,
    /// One status code per case, for a program whose cases differ.
    #[serde(default)]
    cases: Option<Vec<String>>,
}

impl Row {
    /// What `statuses` must equal: `["CE"]`, or one status code per case.
    pub fn expected(&self) -> Vec<String> {
        match (&self.expect, &self.cases) {
            (_, Some(cases)) => cases.clone(),
            (Some(ce), None) if ce == "CE" => vec![ce.clone()],
            (Some(status), None) => vec![status.clone(); CASES.len()],
            (None, None) => Vec::new(),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SameAs {
    lang: String,
    rows_of: String,
    #[serde(default)]
    except: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    rows: Vec<Row>,
    #[serde(default)]
    same_as: Vec<SameAs>,
}

impl Manifest {
    /// Refuses unknown keys and a row without exactly one of `expect` and `cases` (of one code per case).
    pub fn parse(text: &str) -> Result<Manifest, String> {
        let manifest: Manifest = toml::from_str(text).map_err(|e| e.to_string())?;
        let malformed = |row: &&Row| match (&row.expect, &row.cases) {
            (Some(_), None) => false,
            (None, Some(cases)) => cases.len() != CASES.len(),
            _ => true,
        };
        match manifest.rows.iter().find(malformed) {
            Some(row) => Err(format!("{} {}: give `expect` or {} `cases`", row.lang, row.file, CASES.len())),
            None => Ok(manifest),
        }
    }

    /// The rows plus, per `same_as`, the named key's rows run under another key.
    pub fn expanded(&self) -> Vec<Row> {
        let mut rows = self.rows.clone();
        for alias in &self.same_as {
            let copies =
                self.rows.iter().filter(|r| r.lang == alias.rows_of && !alias.except.contains(&r.file));
            rows.extend(copies.map(|r| Row { lang: alias.lang.clone(), ..r.clone() }).collect::<Vec<_>>());
        }
        rows
    }
}

/// Writes `init.yml` and `<i>.in`/`<i>.out` for each case into `dir` (created).
pub fn write_problem(dir: &Path, cases: &[(&str, &str)]) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let mut init = String::from("test_cases:\n");
    for (i, (input, output)) in cases.iter().enumerate() {
        std::fs::write(dir.join(format!("{i}.in")), input)?;
        std::fs::write(dir.join(format!("{i}.out")), output)?;
        init.push_str(&format!("- {{in: {i}.in, out: {i}.out, points: 1}}\n"));
    }
    std::fs::write(dir.join("init.yml"), init)
}

/// A row's submission under the manifest's limits.
pub fn submission(lang: &str, jvm: bool, source: Vec<u8>) -> Submission {
    Submission {
        language_key: lang.into(),
        source: SubmissionSource::Inline(source),
        time_limit_s: TIME_LIMIT_S,
        memory_kib: if jvm { JVM_MEMORY_KIB } else { MEMORY_KIB },
        short_circuit: false,
        pretests_only: false,
    }
}

/// What a row's `expected()` is compared with: `["CE"]` for a compile error, every case's status code when
/// graded, else the outcome's debug text.
pub fn statuses(outcome: &Outcome, cases: &[&str]) -> Vec<String> {
    match outcome {
        Outcome::CompileError(_) => vec!["CE".into()],
        Outcome::Graded => cases.iter().map(|code| code.to_string()).collect(),
        other => vec![format!("{other:?}")],
    }
}
