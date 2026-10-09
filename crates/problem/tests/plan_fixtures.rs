use judge_problem::init::parse_init;
use judge_problem::plan::{PlanOptions, plan_cases};

#[test]
fn every_fixture_plans_with_positive_total() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/init");
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        let init = parse_init(&std::fs::read_to_string(&path).unwrap()).unwrap();
        for pretests_only in [false, true] {
            let options =
                PlanOptions { pretests_only, default_output_limit: 1 << 20, default_output_prefix: 64 };
            let plan = plan_cases(&init, &options).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            assert!(!plan.cases.is_empty(), "{}", path.display());
            let positions: Vec<u32> = plan.cases.iter().map(|c| c.position).collect();
            assert_eq!(positions, (1..=plan.cases.len() as u32).collect::<Vec<_>>());
        }
    }
}
