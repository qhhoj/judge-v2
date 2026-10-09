use std::collections::BTreeSet;

use conformance::{
    Role, Step,
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

#[test]
fn the_suite_holds_every_scenario_of_the_spec() {
    let scenarios = load_all(&scenarios_dir()).unwrap();
    assert_eq!(scenarios.len(), 36, "scenarios 01-33 plus 21b, 28b, 28c");
    let for_judge: BTreeSet<String> = scenarios
        .into_iter()
        .filter(|scenario| scenario.applies_to.contains(&Role::Judge))
        .map(|scenario| scenario.name)
        .collect();
    for name in [
        "11-grade",
        "12-compile-error",
        "18-resume-continue",
        "20-resume-drop-after-rejudge",
        "21b-rejudge-mid-run-one-slot",
        "22-abort-running",
        "24-abort-in-grace-resume",
        "28b-accept-timeout-one-slot",
        "30-judge-refuses-beyond-slots",
        "32-fetch-source",
        "33-draining",
    ] {
        assert!(for_judge.contains(name), "{name} must apply to the judge");
    }
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
fn a_restart_is_refused_in_a_scenario_the_judge_runs() {
    let dir = std::env::temp_dir().join("conformance-restart-check");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("x.yaml");
    std::fs::write(&path, "name: x\nsummary: s\napplies_to: [judge]\nsetup: {judges: [{name: j1}]}\nsteps:\n  - restart_bridge: {}\n").unwrap();
    assert!(load(&path).unwrap_err().contains("restart_bridge"));
}

#[test]
fn a_message_that_is_not_a_judge_message_is_refused() {
    let dir = std::env::temp_dir().join("conformance-message-check");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("x.yaml");
    std::fs::write(&path, "name: x\nsummary: s\napplies_to: [bridge]\nsetup: {judges: [{name: j1}]}\nsteps:\n  - judge: {from: j1, message: {welcome: {}}}\n").unwrap();
    assert!(load(&path).unwrap_err().contains("not a JudgeMessage"));
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
}

#[test]
fn steps_deserialize_from_one_key_maps() {
    let step: Step = serde_json::from_value(json!({"wait": {"ms": 5}})).unwrap();
    assert!(matches!(step, Step::Wait { ms: 5 }));
}
