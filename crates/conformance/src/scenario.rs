//! The scenario model. A scenario is a wire transcript of one or more judge
//! sessions: `judge` steps travel judge -> bridge, `bridge` steps bridge -> judge.
//! The runner playing a role sends that role's steps and checks the other's.
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

use judge_proto::v1::{BridgeMessage, JudgeMessage};
use serde::Deserialize;
use serde_json::Value;

use crate::{
    expand::{Bindings, expand},
    play::code_of,
};

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Bridge,
    Judge,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scenario {
    pub name: String,
    pub summary: String,
    /// Which real implementation the scenario tests: `bridge` (backend runner
    /// against the real bridge) and/or `judge` (judge-v2 runner against the daemon).
    pub applies_to: Vec<Role>,
    /// Bridge timing overrides in ms (`handshakeMs`, `acceptMs`, `resumeGraceMs`, ...).
    #[serde(default)]
    pub timings: serde_json::Map<String, Value>,
    pub setup: Setup,
    pub steps: Vec<Step>,
    /// Mongo state the backend runner asserts after the last step.
    #[serde(default)]
    pub end_state: Value,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Setup {
    pub judges: Vec<JudgeSetup>,
    #[serde(default)]
    pub problems: Vec<ProblemSetup>,
    #[serde(default)]
    pub submissions: Vec<SubmissionSetup>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JudgeSetup {
    /// The party's name in the steps. It is also the judge's name unless `as` is set.
    pub name: String,
    #[serde(default)]
    pub blocked: bool,
    /// A second connection of the judge `as` (same name and key), e.g. a session that
    /// replaces the first one. Steps address each connection by its own `name`.
    #[serde(default, rename = "as")]
    pub identity: Option<String>,
    /// Backend runner only: false makes the fake judge ignore pings (a hung judge).
    #[serde(default = "yes")]
    pub answers_pings: bool,
}

fn yes() -> bool {
    true
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ProblemSetup {
    pub code: String,
    #[serde(default)]
    pub data_version: u64,
    #[serde(default)]
    pub partial: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SubmissionSetup {
    /// Placeholder name (`s1`); runners bind it to the real id.
    pub id: String,
    pub problem: String,
    #[serde(default = "default_language")]
    pub language: String,
    /// File-only source of this many bytes, served by FetchSource.
    #[serde(default)]
    pub file_bytes: Option<u64>,
}

fn default_language() -> String {
    "CPP17".to_string()
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Step {
    /// Judge opens a Session; `expect` is `OK` or the gRPC status that refuses it.
    Open {
        judge: String,
        #[serde(default)]
        wrong_key: bool,
        #[serde(default = "ok")]
        expect: String,
    },
    /// A JudgeMessage (proto3 JSON), `repeat` times.
    Judge {
        from: String,
        message: Value,
        #[serde(default = "one")]
        repeat: u32,
    },
    /// A BridgeMessage (proto3 JSON). Pings are never part of a transcript.
    Bridge {
        to: String,
        message: Value,
    },
    /// The bridge sends `to` nothing but pings for `ms`.
    Quiet {
        to: String,
        ms: u64,
    },
    /// The bridge ends the session with this gRPC status (`OK` = a clean end).
    Closed {
        judge: String,
        status: String,
    },
    /// The judge's connection is lost (both sides drop the session).
    Drop {
        judge: String,
    },
    /// FetchSource for `ref` answers `expect` (`OK` with `bytes`, or a status).
    Fetch {
        judge: String,
        #[serde(rename = "ref")]
        reference: Value,
        expect: String,
        #[serde(default)]
        bytes: u64,
    },
    /// An api action (`{judge: [s1]}`, `{abort: s1}`, `{rejudge: s1}`, `{block: j1}`,
    /// `{rotate_key: j1}`, `{disable: j1}`); only the backend runner performs it.
    Api(Value),
    Wait {
        ms: u64,
    },
    /// Backend runner only: the bridge process restarts (same Mongo and Redis).
    RestartBridge {},
}

fn ok() -> String {
    "OK".to_string()
}

fn one() -> u32 {
    1
}

pub fn scenarios_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../proto/conformance/scenarios")
}

/// Every `*.yaml` scenario in `dir`, sorted by file name.
pub fn load_all(dir: &Path) -> Result<Vec<Scenario>, String> {
    let mut paths: Vec<PathBuf> = fs::read_dir(dir)
        .map_err(|error| error.to_string())?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "yaml"))
        .collect();
    paths.sort();
    paths.iter().map(|path| load(path)).collect()
}

pub fn load(path: &Path) -> Result<Scenario, String> {
    let text = fs::read_to_string(path).map_err(|error| error.to_string())?;
    // YAML -> JSON value first: externally tagged steps are then one-key maps
    let value: Value =
        serde_norway::from_str(&text).map_err(|error| format!("{}: {error}", path.display()))?;
    let scenario: Scenario =
        serde_json::from_value(value).map_err(|error| format!("{}: {error}", path.display()))?;
    let stem = path.file_stem().and_then(|stem| stem.to_str()).unwrap_or_default();
    scenario.validate(stem).map_err(|error| format!("{}: {error}", path.display()))?;
    Ok(scenario)
}

impl Scenario {
    /// Ids bound to placeholder names: `s1` -> a fixed UUID, `session:j1` -> a session id.
    pub fn sample_bindings(&self) -> Bindings {
        let mut bindings = Bindings::new();
        for (index, submission) in self.setup.submissions.iter().enumerate() {
            bindings.insert(submission.id.clone(), format!("01920000-0000-7000-8000-{:012}", index + 1));
        }
        for judge in &self.setup.judges {
            bindings.insert(format!("session:{}", judge.name), format!("session-of-{}", judge.name));
        }
        bindings
    }

    /// The judge name (and key owner) a party authenticates as.
    pub fn identity<'a>(&'a self, party: &'a str) -> &'a str {
        let setup = self.setup.judges.iter().find(|judge| judge.name == party);
        setup.and_then(|judge| judge.identity.as_deref()).unwrap_or(party)
    }

    pub fn validate(&self, file_stem: &str) -> Result<(), String> {
        if self.name != file_stem {
            return Err(format!("name {} must equal the file name", self.name));
        }
        if self.applies_to.is_empty() {
            return Err("applies_to is empty".into());
        }
        let restarts = self.steps.iter().any(|step| matches!(step, Step::RestartBridge {}));
        if restarts && self.applies_to != [Role::Bridge] {
            return Err("restart_bridge needs applies_to: [bridge]".into());
        }
        let judges: BTreeSet<&str> = self.setup.judges.iter().map(|judge| judge.name.as_str()).collect();
        for judge in &self.setup.judges {
            if let Some(identity) = judge.identity.as_deref().filter(|name| !judges.contains(name)) {
                return Err(format!("{} is `as` the unknown judge {identity}", judge.name));
            }
        }
        let bindings = self.sample_bindings();
        self.steps.iter().enumerate().try_for_each(|(index, step)| {
            check_step(step, &judges, &bindings).map_err(|error| format!("step {}: {error}", index + 1))
        })
    }
}

fn check_step(step: &Step, judges: &BTreeSet<&str>, bindings: &Bindings) -> Result<(), String> {
    let known = |name: &str| judges.contains(name).then_some(()).ok_or(format!("unknown judge {name}"));
    match step {
        Step::Open { judge, expect: status, .. } | Step::Closed { judge, status } => {
            known(judge)?;
            code_of(status).map(|_| ())
        }
        Step::Drop { judge } => known(judge),
        Step::Quiet { to, .. } => known(to),
        Step::Judge { from, message, .. } => {
            known(from)?;
            parse_judge(message, bindings).map(|_| ())
        }
        Step::Bridge { to, message } => {
            known(to)?;
            parse_bridge(message, bindings).map(|_| ())
        }
        Step::Fetch { judge, reference, expect, .. } => {
            known(judge)?;
            code_of(expect)?;
            expand(reference, bindings).map(|_| ())
        }
        Step::Api(_) | Step::Wait { .. } | Step::RestartBridge {} => Ok(()),
    }
}

pub fn parse_judge(message: &Value, bindings: &Bindings) -> Result<JudgeMessage, String> {
    let expanded = expand(message, bindings)?;
    serde_json::from_value(expanded).map_err(|error| format!("not a JudgeMessage: {error}"))
}

pub fn parse_bridge(message: &Value, bindings: &Bindings) -> Result<BridgeMessage, String> {
    let expanded = expand(message, bindings)?;
    serde_json::from_value(expanded).map_err(|error| format!("not a BridgeMessage: {error}"))
}
