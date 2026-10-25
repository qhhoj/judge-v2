//! Judge half of the backend-judge init.yml contract. Each `<name>.yml` under
//! `crates/problem/tests/fixtures/judge-contract/init-yml` was written by the backend's real writer, and
//! `<name>.expect.json` says whether the backend saves that data and which cases its stored rows mean (DMOJ plan
//! rules; CONTRIBUTING.md, "Backend contract fixtures"). Every file the backend saves must parse, be supported,
//! plan the same cases and build its checker here; a difference would be an IE or a wrong grade in production.

use judge_grader::checkers::select::choose;
use judge_grader::judge::single_checker;
use judge_problem::init::{CheckerSpec, InitYaml, parse_init};
use judge_problem::plan::{CasePlan, PlanOptions, PlannedCase, plan_cases};
use serde_json::Value;
use std::path::{Path, PathBuf};

const DEFAULT_OUTPUT_LIMIT: u64 = 25_165_824;
const DEFAULT_OUTPUT_PREFIX: u64 = 64;

fn scenarios_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../problem/tests/fixtures/judge-contract/init-yml")
}

/// YAML numbers against JSON numbers, by value.
fn same_value(yaml: &serde_norway::Value, json: &Value) -> bool {
    serde_json::to_value(yaml).is_ok_and(|yaml| match (&yaml, json) {
        (Value::Number(a), Value::Number(b)) => a.as_f64() == b.as_f64(),
        _ => yaml == *json,
    })
}

fn case_differences(got: &PlannedCase, want: &Value) -> Vec<String> {
    let want_limit = want["outputLimit"].as_u64().unwrap_or(DEFAULT_OUTPUT_LIMIT);
    let want_prefix = want["outputPrefix"].as_u64().unwrap_or(DEFAULT_OUTPUT_PREFIX);
    let want_batch = want["batch"].as_u64().map(|batch| batch as u32);
    let args = serde_norway::Value::Mapping(got.checker.args().clone());
    let checks = [
        (got.input == want["in"].as_str().unwrap(), format!("in {:?}", got.input)),
        (got.output == want["out"].as_str().unwrap(), format!("out {:?}", got.output)),
        (got.total == want["total"].as_f64().unwrap(), format!("total {}", got.total)),
        (got.batch == want_batch, format!("batch {:?}", got.batch)),
        (got.output_limit == want_limit, format!("output limit {}", got.output_limit)),
        (got.output_prefix == want_prefix, format!("output prefix {}", got.output_prefix)),
        (
            got.checker.name() == want["checker"]["name"].as_str().unwrap(),
            format!("checker {}", got.checker.name()),
        ),
        (same_value(&args, &want["checker"]["args"]), format!("checker args {args:?}")),
    ];
    checks
        .into_iter()
        .filter(|(same, _)| !same)
        .map(|(_, what)| format!("case {}: {what}", got.position))
        .collect()
}

fn plan_differences(plan: &CasePlan, want: &Value) -> Vec<String> {
    let cases = want["cases"].as_array().unwrap();
    if plan.pretested != want["pretested"].as_bool().unwrap() || plan.cases.len() != cases.len() {
        return vec![format!("pretested {} with {} cases, want {want}", plan.pretested, plan.cases.len())];
    }
    plan.cases.iter().zip(cases).flat_map(|(got, want)| case_differences(got, want)).collect()
}

fn checker_refusal(spec: &CheckerSpec, init: &InitYaml) -> Option<String> {
    let unicode = init.hints.iter().any(|hint| hint == "unicode");
    choose(spec, unicode).err().map(|error| format!("checker refused: {}", error.0))
}

fn run_differences(init: &InitYaml, pretests_only: bool, want: &Value) -> Vec<String> {
    let options = PlanOptions {
        pretests_only,
        default_output_limit: DEFAULT_OUTPUT_LIMIT,
        default_output_prefix: DEFAULT_OUTPUT_PREFIX,
    };
    let plan = match plan_cases(init, &options) {
        Ok(plan) => plan,
        Err(error) => return vec![format!("plan refused: {error}")],
    };
    let mut diffs = plan_differences(&plan, want);
    if init.interactive.is_none() {
        match single_checker(&plan) {
            Ok(spec) => diffs.extend(checker_refusal(&spec, init)),
            Err(error) => diffs.push(format!("single checker: {error:?}")),
        }
    }
    diffs
}

/// What the judge does differently from what the backend meant, for one saved scenario.
fn scenario_differences(yml: &Path, expected: &Value) -> Vec<String> {
    let init = match parse_init(&std::fs::read_to_string(yml).unwrap()) {
        Ok(init) => init,
        Err(error) => return vec![format!("parse refused: {error}")],
    };
    let mut diffs: Vec<String> =
        init.unsupported_feature().map(|f| format!("unsupported: {f}")).into_iter().collect();
    for (pretests_only, key) in [(false, "full"), (true, "pretest")] {
        diffs.extend(
            run_differences(&init, pretests_only, &expected[key]).into_iter().map(|d| format!("{key}: {d}")),
        );
    }
    if let Some(checker) = init.interactive.as_ref().and_then(|interactive| interactive.checker.as_ref()) {
        diffs.extend(checker_refusal(checker, &init).map(|d| format!("interactive: {d}")));
    }
    diffs
}

#[test]
fn every_init_yml_the_backend_saves_means_the_same_here() {
    let dir = scenarios_dir();
    let mut expectations: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.to_string_lossy().ends_with(".expect.json"))
        .collect();
    expectations.sort();
    assert!(!expectations.is_empty(), "no fixtures in {}", dir.display());
    let mut mismatches = Vec::new();
    for path in &expectations {
        let expected: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        let name = path.file_name().unwrap().to_string_lossy().trim_end_matches(".expect.json").to_string();
        if expected["accepted"].as_bool() != Some(true) {
            continue;
        }
        let diffs = scenario_differences(&dir.join(format!("{name}.yml")), &expected);
        if !diffs.is_empty() {
            mismatches.push(format!("{name}: {}", diffs.join("; ")));
        }
    }
    assert!(
        mismatches.is_empty(),
        "init.yml the backend saves but the judge reads differently:\n{}",
        mismatches.join("\n")
    );
}
