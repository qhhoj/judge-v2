//! The generated sources and the runtime compile: every grammar-v1 type in every position, on both sides, under
//! the oldest and a recent C++ standard, with warnings as errors for the runtime. Linking and running them is
//! tested in isolate (`crates/grader/tests/split_isolate.rs`). Without `g++` the tests fail under CI (`CI` set)
//! and are skipped locally with a note.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use judge_interface::glue::{contestant::contestant_glue, jury::jury_glue, probe};
use judge_interface::{Role, parse_header, runtime};

const TYPES: [&str; 11] = [
    "int",
    "long long",
    "double",
    "bool",
    "std::string",
    "std::vector<int>",
    "std::vector<bool>",
    "std::vector<std::vector<long long> >",
    "std::pair<int, int>",
    "std::pair<std::string, std::vector<bool> >",
    "std::vector<std::pair<double, bool> >",
];

/// Per type: a callback and a contestant function taking it in every passing form and returning it, a callback
/// returning `const&`, and a void callback and contestant function.
fn every_type_in_every_position() -> (String, Vec<Role>) {
    let mut header = String::from("#include <string>\n#include <utility>\n#include <vector>\n");
    let mut roles = Vec::new();
    for (i, ty) in TYPES.iter().enumerate() {
        let params = format!("{ty} a, const {ty} b, const {ty}& c, {ty} const& d");
        header += &format!("{ty} callback{i}({params});\n{ty} solve{i}({params});\n");
        header += &format!("const {ty}& view{i}();\nvoid note{i}({ty} a);\nvoid answer{i}({ty} a);\n");
        roles.extend([Role::Callback, Role::Contestant, Role::Callback, Role::Callback, Role::Contestant]);
    }
    header += "void start();\nint ping();\n";
    roles.extend([Role::Contestant, Role::Callback]);
    (header, roles)
}

/// A fresh directory per test (tests run in parallel).
fn work_dir(test: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("interface-compile").join(test);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Writes the header, the runtime and the four generated sources into `dir`; returns what to compile.
fn write_sources(dir: &Path, header: &str, roles: &[Role]) -> Vec<&'static str> {
    let interface = parse_header("grader.h", header).unwrap_or_else(|refusal| panic!("{refusal}"));
    let sources = [
        ("qhhoj_contestant.cpp", contestant_glue(&interface, roles, "grader.h")),
        ("qhhoj_jury.cpp", jury_glue(&interface, roles, "grader.h")),
        ("probe.cpp", probe::probe(&interface, "grader.h")),
        ("null_contestant.cpp", probe::null_contestant(&interface, roles, "grader.h")),
    ];
    std::fs::write(dir.join("grader.h"), header).unwrap();
    std::fs::write(dir.join(runtime::HEADER_NAME), runtime::HEADER).unwrap();
    std::fs::write(dir.join(runtime::SOURCE_NAME), runtime::SOURCE).unwrap();
    for (name, text) in &sources {
        std::fs::write(dir.join(name), text).unwrap();
    }
    sources.iter().map(|(name, _)| *name).chain([runtime::SOURCE_NAME]).collect()
}

/// Compiler errors of every generated source under each standard; empty when all compile.
fn compile_errors(test: &str, header: &str, roles: &[Role]) -> Option<Vec<String>> {
    if Command::new("g++").arg("--version").output().is_err() {
        let note = "g++ is not installed: the generated sources were not compiled";
        assert!(std::env::var_os("CI").is_none(), "{note}");
        // Written to the process's stderr, not `eprintln!`, which the test harness captures for passing tests.
        let _ = writeln!(std::io::stderr().lock(), "note: {note}");
        return None;
    }
    let dir = work_dir(test);
    let mut errors = Vec::new();
    for name in write_sources(&dir, header, roles) {
        for standard in ["-std=gnu++03", "-std=gnu++17"] {
            let output = Command::new("g++")
                .args([standard, "-fsyntax-only", "-Wall", "-Werror", name])
                .current_dir(&dir)
                .output()
                .unwrap();
            if !output.status.success() {
                errors.push(format!("{name} {standard}:\n{}", String::from_utf8_lossy(&output.stderr)));
            }
        }
    }
    Some(errors)
}

#[test]
fn the_generated_sources_compile_for_every_type_in_every_position() {
    let (header, roles) = every_type_in_every_position();
    if let Some(errors) = compile_errors("every_type", &header, &roles) {
        assert!(errors.is_empty(), "{}", errors.join("\n"));
    }
}

#[test]
fn the_generated_sources_compile_when_a_name_is_also_a_standard_library_name() {
    let header = "#include <algorithm>\n#include <vector>\nusing namespace std;\n\
                  int count(int x);\nint find(const vector<int>& v);\nvoid sort();\nint max(int a, int b);\n";
    let roles = [Role::Callback, Role::Contestant, Role::Contestant, Role::Callback];
    if let Some(errors) = compile_errors("library_names", header, &roles) {
        assert!(errors.is_empty(), "{}", errors.join("\n"));
    }
}
