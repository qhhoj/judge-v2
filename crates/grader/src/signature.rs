//! `signature_grader`: contestant functions linked with the problem's `entry` (legacy-judge-usage.md §3).
//! Everything is compiled inside the box with compile limits; the box keeps only the linked binary.

use crate::artefacts::read_data_source;
use crate::compile::CompileJob;
use crate::error::GradeError;
use crate::languages::Language;
use crate::program::SourceFile;
use judge_problem::init::SignatureSpec;
use judge_problem::resolve::ResolvedProblem;
use std::io::Read;

pub const SIGNATURE_LANGUAGES: &str = "This problem accepts only C and C++ submissions.";
const RENAMED_MAIN: &str = "-Dmain=qhhoj_user_main";

/// File names come from init.yml and enter a shell command: plain, no leading dot, no path separator.
pub fn safe_name(name: &str) -> bool {
    (1..=64).contains(&name.len())
        && !name.starts_with('.')
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
}

/// Compile the user file with the header forced in, compile the entry, link both.
pub fn signature_script(compiler: &[String], spec: &SignatureSpec, user_file: &str) -> String {
    let cc = compiler.join(" ");
    let rename = if spec.allow_main { String::new() } else { format!(" {RENAMED_MAIN}") };
    format!(
        "{cc} -c {user_file} -o user.o -include {header}{rename} && {cc} -c {entry} -o entry.o && {cc} entry.o user.o -o main -lm",
        header = spec.header,
        entry = spec.entry,
    )
}

/// 64 random bits as hex from the kernel, so the entry's name cannot be guessed by contestant code.
fn unguessable_token() -> Result<String, GradeError> {
    let mut bytes = [0u8; 8];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(hex::encode(bytes))
}

fn bad_name(name: &str) -> GradeError {
    GradeError::Checker(format!("bad signature file name {name:?}"))
}

fn extension(file: &str) -> &str {
    file.rsplit_once('.').map_or("", |(_, extension)| extension)
}

/// `None` when the language has no `signature` compiler: the submission becomes CE with `SIGNATURE_LANGUAGES`.
///
/// The grader's entry is stored in the box under a random name, so contestant code (`#include`,
/// `.incbin`) cannot read it into the binary or into the compile log. The header is public by design.
pub fn signature_job(
    language: &Language,
    problem: &ResolvedProblem,
    spec: &SignatureSpec,
    source: &[u8],
) -> Result<Option<CompileJob>, GradeError> {
    let Some(compiler) = &language.signature else { return Ok(None) };
    let user_file = format!("user.{}", extension(&language.source));
    for name in [&spec.entry, &spec.header] {
        if !safe_name(name) || *name == user_file {
            return Err(bad_name(name));
        }
    }
    let entry = read_data_source(problem, &spec.entry)?;
    let header = read_data_source(problem, &spec.header)?;
    let hidden = format!("entry-{}.{}", unguessable_token()?, extension(&spec.entry));
    let hidden_spec = SignatureSpec { entry: hidden.clone(), ..spec.clone() };
    let mut job = CompileJob::for_language(language, source);
    job.argv =
        Some(vec!["/bin/sh".into(), "-c".into(), signature_script(compiler, &hidden_spec, &user_file)]);
    job.files = vec![
        SourceFile { name: user_file, bytes: source.to_vec() },
        SourceFile { name: hidden, bytes: entry.bytes },
        header,
    ];
    // Only the linked binary runs; the grader's sources and objects stay out of the run box.
    job.keep = Some(vec!["main".into()]);
    Ok(Some(job))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(allow_main: bool) -> SignatureSpec {
        SignatureSpec { entry: "grader.cpp".into(), header: "sol.h".into(), allow_main }
    }

    #[test]
    fn script_compiles_user_with_header_then_links() {
        let cc: Vec<String> = ["/usr/bin/g++", "-std=gnu++17", "-O2"].map(String::from).to_vec();
        let script = signature_script(&cc, &spec(false), "user.cpp");
        assert!(script.contains(
            "/usr/bin/g++ -std=gnu++17 -O2 -c user.cpp -o user.o -include sol.h -Dmain=qhhoj_user_main"
        ));
        assert!(script.contains("-c grader.cpp -o entry.o"));
        assert!(script.ends_with("entry.o user.o -o main -lm"));
        assert!(!signature_script(&cc, &spec(true), "user.cpp").contains("-Dmain"));
    }

    #[test]
    fn unsafe_names_are_refused() {
        for name in ["a b.h", "x;rm -rf.h", "../h.h", "", "$(id).h", ".hidden.h", "a/b.h", "`id`.h"] {
            assert!(!safe_name(name), "{name:?}");
        }
        assert!(!safe_name(&"a".repeat(65)));
        assert!(safe_name("sol_v2.h"));
    }

    #[test]
    fn the_hidden_name_differs_every_time_and_is_plain() {
        let (a, b) = (unguessable_token().unwrap(), unguessable_token().unwrap());
        assert_ne!(a, b);
        assert!(safe_name(&format!("entry-{a}.cpp")));
    }
}
