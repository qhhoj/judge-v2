use conformance::{
    Step,
    play::{play_pair, self_play},
    scenario::{Scenario, load_all, scenarios_dir},
};

fn playable() -> Vec<Scenario> {
    let all = load_all(&scenarios_dir()).unwrap();
    all.into_iter()
        .filter(|scenario| !scenario.steps.iter().any(|step| matches!(step, Step::RestartBridge {})))
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_scenario_plays_against_itself() {
    for scenario in playable() {
        self_play(&scenario).await.unwrap_or_else(|error| panic!("{}: {error}", scenario.name));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_judge_that_breaks_the_transcript_is_caught() {
    let scenario = playable().into_iter().find(|scenario| scenario.name == "11-grade").unwrap();
    let mut broken = scenario.clone();
    // the bridge role now expects seq 9 where the judge role sends seq 1
    for step in &mut broken.steps {
        if let Step::Judge { message, .. } = step
            && message.get("accepted").is_some()
        {
            message["accepted"]["seq"] = 9.into();
        }
    }
    let error = play_pair(&broken, &scenario).await.unwrap_err();
    assert!(error.contains("seq"), "{error}");
}
