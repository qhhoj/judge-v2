//! Judge half of the backend-judge archive contract. `fixtures/judge-contract` is a copy of the backend's
//! `libs/problems/src/judge-contract/fixtures` (CONTRIBUTING.md, "Backend contract fixtures"): crafted zips and
//! `backend.json`, what the backend's upload reader accepted and the bytes of each name a case may use. An archive
//! the backend accepts must extract here to the same bytes under the same names; one it refuses must be refused
//! here too, so the two readers never grade different files. `limits.json` holds the backend's archive caps.

use judge_problem::archive::{BACKEND_ARCHIVE_MAX_BYTES, MIN_CACHE_MAX_BYTES, ZipLimits, extract_zip};
use serde_json::Value;
use std::path::{Path, PathBuf};

fn contract_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/judge-contract")
}

fn read_json(path: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

/// Empty when the judge reads what the backend's verdict says, else what differs.
fn difference(zip: &Path, verdict: &Value) -> Option<String> {
    let out = tempfile::tempdir().unwrap();
    let extracted = extract_zip(zip, out.path(), &ZipLimits::default());
    let Some(files) = verdict["files"].as_object() else {
        return extracted.is_ok().then(|| "backend refuses, judge extracts".to_string());
    };
    if let Err(error) = extracted {
        return Some(format!("backend accepts, judge refuses: {error}"));
    }
    let diffs: Vec<String> = files
        .iter()
        .filter_map(|(name, want)| match std::fs::read(out.path().join(name)) {
            Ok(got) if String::from_utf8_lossy(&got) == want.as_str().unwrap() => None,
            Ok(got) => Some(format!("{name:?} reads {:?}", String::from_utf8_lossy(&got))),
            Err(error) => Some(format!("{name:?}: {error}")),
        })
        .collect();
    (!diffs.is_empty()).then(|| diffs.join("; "))
}

#[test]
fn archives_read_the_same_on_both_sides() {
    let dir = contract_dir().join("zips");
    let verdicts = read_json(&dir.join("backend.json"));
    let verdicts = verdicts.as_object().unwrap();
    assert!(!verdicts.is_empty(), "no fixtures in {}", dir.display());
    let mismatches: Vec<String> = verdicts
        .iter()
        .filter_map(|(name, verdict)| {
            difference(&dir.join(format!("{name}.zip")), verdict).map(|diff| format!("{name}: {diff}"))
        })
        .collect();
    assert!(
        mismatches.is_empty(),
        "backend and judge read these archives differently:\n{}",
        mismatches.join("\n")
    );
}

#[test]
fn every_archive_the_backend_accepts_fits_the_smallest_allowed_cache() {
    let limits = read_json(&contract_dir().join("limits.json"));
    assert_eq!(limits["maxTotalBytes"].as_u64(), Some(BACKEND_ARCHIVE_MAX_BYTES));
    let smallest = ZipLimits::for_cache(MIN_CACHE_MAX_BYTES);
    assert!(smallest.max_total_bytes >= BACKEND_ARCHIVE_MAX_BYTES);
    assert!(smallest.max_entries as u64 >= limits["maxEntries"].as_u64().unwrap());
}
