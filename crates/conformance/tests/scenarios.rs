use std::collections::BTreeSet;

use conformance::{
    Step,
    expand::{Bindings, expand},
    scenario::{load, load_all, scenarios_dir},
};
use serde_json::json;

#[test]
fn every_scenario_loads_and_its_messages_parse() {
    let scenarios = load_all(&scenarios_dir()).unwrap_or_else(|error| panic!("{error}"));
    let names: BTreeSet<_> = scenarios.iter().map(|scenario| scenario.name.clone()).collect();
    assert_eq!(names.len(), scenarios.len(), "names are unique");
}

/// Preflight C2: after the bridge aborts, voids or drops a run, the judge ends it with
/// Final{ABORTED}, and the generation-2 Assign comes only after that Final.
#[test]
fn a_released_run_holds_its_slot_until_the_judges_final() {
    let scenarios = load_all(&scenarios_dir()).unwrap();
    let released = [
        "20-resume-drop-after-rejudge",
        "21b-rejudge-mid-run-one-slot",
        "28-accept-timeout",
        "28b-accept-timeout-one-slot",
    ];
    for name in released {
        let scenario = scenarios.iter().find(|scenario| scenario.name == name).unwrap();
        let final_at = scenario.steps.iter().position(|step| {
            matches!(step, Step::Judge { message, .. }
                if message["final"]["outcome"] == "OUTCOME_ABORTED" && message["final"]["ref"]["generation"] == 1)
        });
        let reassign_at = scenario.steps.iter().position(
            |step| matches!(step, Step::Bridge { message, .. } if message["assign"]["ref"]["generation"] == 2),
        );
        let (final_at, reassign_at) = (final_at.expect(name), reassign_at.expect(name));
        assert!(final_at < reassign_at, "{name}: generation 2 is assigned before generation 1's Final");
    }
}

#[test]
fn placeholders_and_generators_expand() {
    let bindings: Bindings = [("s1".to_string(), "uuid-1".to_string())].into();
    let value = json!({"ref": {"submissionId": "{{s1}}"}, "log": {"$repeat": ["ab", 3]}, "cases": {"$cases": [2, "STATUS_AC"]}});
    assert_eq!(
        expand(&value, &bindings).unwrap(),
        json!({"ref": {"submissionId": "uuid-1"}, "log": "ababab", "cases": [{"position": 1, "status": "STATUS_AC"}, {"position": 2, "status": "STATUS_AC"}]})
    );
    assert!(expand(&json!("{{s9}}"), &bindings).unwrap_err().contains("unbound"));
    let crowded = json!({"$repeat": ["a", 2], "extra": 1});
    assert!(expand(&crowded, &bindings).unwrap_err().contains("only key"));
}

fn load_text(name: &str, text: &str) -> Result<conformance::Scenario, String> {
    let dir = std::env::temp_dir().join(format!("conformance-{name}"));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("x.yaml");
    std::fs::write(&path, text).unwrap();
    load(&path)
}

#[test]
fn scenarios_the_harness_cannot_play_are_refused_when_they_load() {
    let head = |role: &str| format!("name: x\nsummary: s\napplies_to: [{role}]\n");
    let one_judge = "setup: {judges: [{name: j1}]}\nsteps:\n";
    // (applies to, setup and steps, the error names)
    let table = [
        ("judge", format!("{one_judge}  - restart_bridge: {{}}\n"), "restart_bridge"),
        (
            "bridge",
            format!("{one_judge}  - judge: {{from: j1, message: {{welcome: {{}}}}}}\n"),
            "not a JudgeMessage",
        ),
        ("bridge", format!("{one_judge}  - closed: {{judge: j1, status: NOPE}}\n"), "unknown status NOPE"),
        (
            "bridge",
            "setup: {judges: [{name: j1, as: ghost}]}\nsteps:\n  - open: {judge: j1}\n".into(),
            "ghost",
        ),
    ];
    for (i, (role, rest, names)) in table.into_iter().enumerate() {
        let error = load_text(&format!("refused-{i}"), &format!("{}{rest}", head(role))).unwrap_err();
        assert!(error.contains(names), "{error}");
    }
}
