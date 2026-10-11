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

fn named(name: &str) -> Scenario {
    playable().into_iter().find(|scenario| scenario.name == name).unwrap()
}

/// The bridge role plays `scenario`; a deliberately wrong judge plays the changed copy.
async fn wrong_judge_error(name: &str, change: impl FnOnce(&mut Scenario)) -> String {
    let scenario = named(name);
    let mut wrong = scenario.clone();
    change(&mut wrong);
    play_pair(&scenario, &wrong).await.unwrap_err()
}

fn open_steps(scenario: &mut Scenario) -> impl Iterator<Item = &mut Step> {
    scenario.steps.iter_mut().filter(|step| matches!(step, Step::Open { .. }))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_judge_with_the_wrong_key_is_caught() {
    let error = wrong_judge_error("11-grade", |judge| {
        if let Some(Step::Open { wrong_key, .. }) = open_steps(judge).next() {
            *wrong_key = true;
        }
    })
    .await;
    assert!(error.contains("x-judge-key"), "{error}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_judge_that_never_fetches_is_caught() {
    let error = wrong_judge_error("32-fetch-source", |judge| {
        judge.steps.retain(|step| !matches!(step, Step::Fetch { .. }))
    })
    .await;
    assert!(error.contains("FetchSource"), "{error}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_judge_that_talks_after_the_last_step_is_caught() {
    let error = wrong_judge_error("30-judge-refuses-beyond-slots", |judge| {
        judge.steps.push(Step::Judge {
            from: "j1".into(),
            message: serde_json::json!({"pong": {"nonce": "1"}}),
            repeat: 1,
        });
    })
    .await;
    assert!(error.contains("after the last step"), "{error}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_fetch_with_another_session_id_than_the_welcomes_is_caught() {
    // the judge gets a session id the bridge did not bind to j1, then fetches with it
    let judge_side = named("32-fetch-source");
    let mut bridge_side = judge_side.clone();
    for step in &mut bridge_side.steps {
        if let Step::Bridge { message, .. } = step
            && message.get("welcome").is_some()
        {
            message["welcome"]["sessionId"] = "someone-elses-session".into();
        }
    }
    let error = play_pair(&bridge_side, &judge_side).await.unwrap_err();
    assert!(error.contains("FetchSource request") && error.contains("sessionId"), "{error}");
}
