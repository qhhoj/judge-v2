use std::{fs, path::Path};

use conformance::matcher::matches;
use serde_json::Value;

#[test]
fn the_shared_matcher_cases_hold() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../proto/conformance/matcher-cases.json");
    let cases: Vec<Value> = serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap();
    assert!(cases.len() >= 10);
    for case in cases {
        let outcome = matches(&case["expected"], &case["actual"]);
        assert_eq!(outcome.is_ok(), case["matches"].as_bool().unwrap(), "{}: {outcome:?}", case["why"]);
    }
}
