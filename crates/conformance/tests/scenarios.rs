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
