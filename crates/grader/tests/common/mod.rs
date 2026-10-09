//! Shared helpers of the isolate integration tests. A language whose runtime is missing on the host
//! is skipped (the test prints `skip:` and returns), never failed.
#![allow(dead_code)]

use judge_grader::languages::{Language, LanguageRegistry};
use judge_sandbox::{IsolateBox, SandboxConfig};
use std::path::Path;

pub const SHIPPED: &str = include_str!("../../../../languages.toml");

pub fn registry() -> LanguageRegistry {
    LanguageRegistry::from_toml(SHIPPED).unwrap()
}

pub fn box_id(offset: u32) -> u32 {
    let base: u32 = std::env::var("JUDGE_TEST_BOX_BASE").ok().and_then(|v| v.parse().ok()).unwrap_or(500);
    base + offset
}

pub fn sandbox(offset: u32, meta: &Path) -> IsolateBox {
    let config = SandboxConfig { meta_dir: meta.into(), ..SandboxConfig::default() };
    IsolateBox::open(&config, box_id(offset), None).unwrap()
}

/// True when every absolute program the language compiles or runs exists on this host.
pub fn installed(language: &Language) -> bool {
    let argvs = [language.compile.as_ref(), Some(&language.run)];
    let programs = argvs.into_iter().flatten().filter_map(|argv| argv.first());
    let wanted = programs.filter(|p| p.starts_with('/')).collect::<Vec<_>>();
    wanted.iter().all(|p| Path::new(p).exists())
}

/// The language when its runtime exists, else prints `skip: KEY` and returns None.
pub fn language_or_skip(registry: &LanguageRegistry, key: &str) -> Option<Language> {
    let language = registry.get(key).unwrap().clone();
    if installed(&language) {
        return Some(language);
    }
    eprintln!("skip: {key} (runtime not installed on this host)");
    None
}

pub fn testlib_dir() -> std::path::PathBuf {
    concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/testlib").into()
}

/// Compiled-helper cache over `cache` with the vendored testlib mounted.
pub fn artefacts(cache: &Path) -> judge_grader::artefacts::Artefacts {
    use judge_grader::artefacts::{ArtefactSetup, Artefacts};
    Artefacts::new(ArtefactSetup {
        cache: std::sync::Arc::new(judge_problem::dir_cache::DirCache::new(cache.into(), 1 << 30)),
        registry: std::sync::Arc::new(registry()),
        build_ids: [("CPP17".to_string(), "test".to_string())].into(),
        testlib_dir: testlib_dir(),
    })
}

pub fn cpp_helper(source: &str) -> judge_grader::artefacts::HelperSource {
    use judge_grader::program::SourceFile;
    let file = SourceFile { name: "helper.cpp".into(), bytes: source.as_bytes().to_vec() };
    judge_grader::artefacts::HelperSource { lang: "CPP17".into(), files: vec![file] }
}
