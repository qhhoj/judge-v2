//! Compiles and runs an accepted a+b for every language this host has (spec §5, §7 "Update" step 2).

use crate::local::LocalJudge;
use crate::run_cmd::outcome_text;
use judge_grader::judge::{Outcome, Submission, SubmissionSource};
use judge_grader::languages::{LanguageRegistry, host_languages};
use judge_grader::report::{CaseReport, Reporter};
use judge_problem::verdict::Status;
use std::path::{Path, PathBuf};

const C_AC: &str = include_str!("../../../tests/programs/C/ac.c");
const CPP_AC: &str = include_str!("../../../tests/programs/CPP17/ac.cpp");
const PY_AC: &str = include_str!("../../../tests/programs/PY3/ac.py");
const JAVA_AC: &str = include_str!("../../../tests/programs/JAVA/ac.java");

/// The accepted programs of the isolate suite, one per language key (OUTPUT needs a zip: not self-tested).
pub const PROGRAMS: &[(&str, &str)] = &[
    ("C", C_AC),
    ("C11", C_AC),
    ("CLANG", C_AC),
    ("CPP03", include_str!("../../../tests/programs/CPP03/ac.cpp")),
    ("CPP11", CPP_AC),
    ("CPP14", CPP_AC),
    ("CPP17", CPP_AC),
    ("CPP20", CPP_AC),
    ("CPP23", CPP_AC),
    ("CLANGX", CPP_AC),
    ("PY3", PY_AC),
    ("PYPY3", PY_AC),
    ("PAS", include_str!("../../../tests/programs/PAS/ac.pas")),
    ("JAVA", JAVA_AC),
    ("JAVA8", JAVA_AC),
    ("KOTLIN", include_str!("../../../tests/programs/KOTLIN/ac.kt")),
    ("RUBY", include_str!("../../../tests/programs/RUBY/ac.rb")),
    ("D", include_str!("../../../tests/programs/D/ac.d")),
    ("LUA", include_str!("../../../tests/programs/LUA/ac.lua")),
    ("ADA", include_str!("../../../tests/programs/ADA/ac.adb")),
    ("BF", include_str!("../../../tests/programs/BF/ac.bf")),
    ("GAS32", include_str!("../../../tests/programs/GAS32/ac.s")),
    ("GAS64", include_str!("../../../tests/programs/GAS64/ac.s")),
    ("TEXT", include_str!("../../../tests/programs/TEXT/three.txt")),
];

#[derive(clap::Args)]
pub struct SelftestArgs {
    #[arg(long)]
    pub languages: Option<PathBuf>,
    #[arg(long)]
    pub testlib: Option<PathBuf>,
    #[arg(long, default_value_t = 900)]
    pub box_base: u32,
    #[arg(long, value_delimiter = ',')]
    pub require: Vec<String>,
    /// Write the host copy of the languages file (runtimes resolved past `/etc`) here and self-test it.
    #[arg(long)]
    pub host_languages: Option<PathBuf>,
}

/// Prints each runtime isolate cannot reach; with `out`, writes the rewritten copy and returns its path.
fn host_copy(languages: &Path, out: Option<&Path>) -> anyhow::Result<PathBuf> {
    let text = std::fs::read_to_string(languages)?;
    let registry = LanguageRegistry::from_toml(&text)?;
    let (host, rewrites) = host_languages(&text, &registry);
    for (from, to) in &rewrites {
        println!("rewrite {from} -> {to}");
    }
    let Some(out) = out else { return Ok(languages.to_path_buf()) };
    std::fs::write(out, host)?;
    println!("wrote {}", out.display());
    Ok(out.to_path_buf())
}

#[derive(Default)]
struct Quiet(Vec<CaseReport>);

impl Reporter for Quiet {
    fn compiled(&mut self, _: &str) {}
    fn started(&mut self, _: bool, _: u32) {}
    fn case(&mut self, c: CaseReport) {
        self.0.push(c);
    }
    fn before_case(&mut self) -> bool {
        true
    }
}

/// Every a+b program reads one-digit operands (BF and GAS), so the problem is "1 2" -> "3".
fn write_problem(root: &Path) -> anyhow::Result<PathBuf> {
    let dir = root.join("selftest");
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join("init.yml"), "test_cases:\n- {in: 1.in, out: 1.out, points: 1}\n")?;
    std::fs::write(dir.join("1.in"), "1 2\n")?;
    std::fs::write(dir.join("1.out"), "3\n")?;
    Ok(dir)
}

/// `Ok(())` when the program is accepted, else why not.
fn check_one(
    judge: &mut LocalJudge,
    problem: &Path,
    key: &str,
    source: &str,
) -> anyhow::Result<Result<(), String>> {
    let submission = Submission {
        language_key: key.into(),
        source: SubmissionSource::Inline(source.as_bytes().to_vec()),
        time_limit_s: 5.0,
        memory_kib: 262_144,
        short_circuit: false,
        pretests_only: false,
    };
    let mut cases = Quiet::default();
    let outcome = judge.judge(problem, &submission, &mut cases)?;
    let accepted = outcome == Outcome::Graded && cases.0.iter().all(|c| c.status == Status::Ac);
    let statuses: Vec<_> = cases.0.iter().map(|c| c.status.code()).collect();
    Ok(if accepted { Ok(()) } else { Err(format!("{} {statuses:?}", outcome_text(&outcome))) })
}

/// Prints `KEY ok|FAIL|absent`; false when the key failed or is required and absent.
fn check_key(
    judge: &mut LocalJudge,
    problem: &Path,
    program: &(&str, &str),
    required: bool,
) -> anyhow::Result<bool> {
    let (key, source) = *program;
    if judge.registry.get(key).is_none() {
        println!("{key} absent{}", if required { " (required)" } else { "" });
        return Ok(!required);
    }
    match check_one(judge, problem, key, source)? {
        Ok(()) => println!("{key} ok"),
        Err(why) => {
            println!("{key} FAIL {why}");
            return Ok(false);
        }
    }
    Ok(true)
}

pub fn selftest(languages: &Path, testlib: &Path, args: &SelftestArgs) -> anyhow::Result<bool> {
    let languages = host_copy(languages, args.host_languages.as_deref())?;
    let mut judge = LocalJudge::new(&languages, testlib, args.box_base)?;
    let work = tempfile::tempdir()?;
    let problem = write_problem(work.path())?;
    let mut ok = true;
    for program in PROGRAMS {
        let required = args.require.iter().any(|r| r == program.0);
        ok &= check_key(&mut judge, &problem, program, required)?;
    }
    for missing in args.require.iter().filter(|r| !PROGRAMS.iter().any(|(k, _)| k == r)) {
        ok = false;
        println!("{missing} absent (required, no selftest program)");
    }
    Ok(ok)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_copy_rewrites_unreachable_runtimes_and_returns_the_copy() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("alt/lua-real");
        std::fs::create_dir_all(real.parent().unwrap()).unwrap();
        std::fs::write(&real, "").unwrap();
        let link = dir.path().join("lua");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let languages = dir.path().join("languages.toml");
        let toml = format!(
            "[[language]]\nkey='LUA'\nname='l'\nsource='main.lua'\nrun=[\"{}\", \"main.lua\"]\nversion=[]\n",
            link.display()
        );
        std::fs::write(&languages, toml).unwrap();
        assert_eq!(host_copy(&languages, None).unwrap(), languages, "without OUT the shipped file is used");
        let out = dir.path().join("host.toml");
        assert_eq!(host_copy(&languages, Some(&out)).unwrap(), out);
        let written = LanguageRegistry::from_toml(&std::fs::read_to_string(&out).unwrap()).unwrap();
        let real_path = std::fs::canonicalize(&real).unwrap().display().to_string();
        assert_eq!(written.get("LUA").unwrap().run[0], real_path);
    }
}
