use judge_problem::init::parse_init;
use judge_problem::plan::{PlanOptions, plan_cases};
use std::fs;

const DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/init");

/// Every init.yml shape the backend writes parses, is supported, and plans to cases numbered 1..=n.
#[test]
fn every_backend_shape_parses_is_supported_and_plans() {
    let mut count = 0;
    for entry in fs::read_dir(DIR).unwrap() {
        let path = entry.unwrap().path();
        let text = fs::read_to_string(&path).unwrap();
        let init = parse_init(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        assert_eq!(init.unsupported_feature(), None, "{}", path.display());
        for pretests_only in [false, true] {
            let options =
                PlanOptions { pretests_only, default_output_limit: 1 << 20, default_output_prefix: 64 };
            let plan = plan_cases(&init, &options).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            let positions: Vec<u32> = plan.cases.iter().map(|c| c.position).collect();
            assert!(!positions.is_empty(), "{}", path.display());
            assert_eq!(positions, (1..=plan.cases.len() as u32).collect::<Vec<_>>());
        }
        count += 1;
    }
    assert!(count >= 19, "fixtures missing: {count}");
}
