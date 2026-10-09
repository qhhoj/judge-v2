use judge_problem::init::parse_init;
use std::fs;

const DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/init");

fn fixture(name: &str) -> String {
    fs::read_to_string(format!("{DIR}/{name}.yml")).unwrap()
}

#[test]
fn every_backend_shape_parses_and_is_supported() {
    let mut count = 0;
    for entry in fs::read_dir(DIR).unwrap() {
        let path = entry.unwrap().path();
        let text = fs::read_to_string(&path).unwrap();
        let init = parse_init(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        assert_eq!(init.unsupported_feature(), None, "{}", path.display());
        count += 1;
    }
    assert!(count >= 19, "fixtures missing: {count}");
}

#[test]
fn batches_fixture_has_a_batch_then_a_plain_case() {
    let init = parse_init(&fixture("batches")).unwrap();
    assert_eq!(init.test_cases.len(), 2);
    assert_eq!(init.test_cases[0].batched.as_ref().map(Vec::len), Some(2));
    assert_eq!(init.test_cases[0].points, Some(40.0));
    assert_eq!(init.test_cases[1].points, Some(60.0));
}

#[test]
fn pretests_fixture_splits_the_lists() {
    let init = parse_init(&fixture("pretests")).unwrap();
    assert_eq!(init.pretest_test_cases.len(), 2);
    assert_eq!(init.test_cases.len(), 1);
}

#[test]
fn custom_checker_fixtures_carry_lang_and_files() {
    let init = parse_init(&fixture("default_pas")).unwrap();
    let args = init.checker.as_ref().unwrap().args();
    assert_eq!(args.get("lang").and_then(|v| v.as_str()), Some("PAS"));
    assert_eq!(args.get("type").and_then(|v| v.as_str()), Some("default"));
    assert_eq!(args.get("files").and_then(|v| v.as_str()), Some("chk.pas"));
}
