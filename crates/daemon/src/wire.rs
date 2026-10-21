//! The only place that knows proto types (plan A's `judge_proto::v1`).

use crate::outbox::{AssignmentRef, Body, Envelope};
use judge_grader::generate::{
    GenerationFailure, GenerationPlan, Kind, PlanLimits, PlanProgram, PlanTest, ProgramLimits, Role, Stage,
};
use judge_grader::judge::{Outcome, RetryReason};
use judge_grader::report::CaseReport;
use judge_problem::init::FileIo;
use judge_problem::verdict::Status;
pub use judge_proto::v1;
pub use judge_proto::v1::{BridgeMessage, JudgeMessage, bridge_message, judge_message};

pub const MAX_INLINE_SOURCE: usize = 1 << 20;
/// What this judge grades beyond plain programs (`Hello.features`). SCRATCH is not supported.
pub const FEATURES: [&str; 11] = [
    "interactive",
    "interactive.checker",
    "file_io",
    "output_only",
    "signature",
    "checker.testlib",
    "checker.default",
    "checker.themis",
    "text",
    "generate.polygon",
    "generate.compile",
];

#[derive(Debug, thiserror::Error)]
#[error("protocol: {0}")]
pub struct WireError(pub String);

#[derive(Debug, Clone, PartialEq)]
pub struct Assignment {
    pub r: AssignmentRef,
    pub problem_code: String,
    pub data_version: u64,
    pub language_key: String,
    pub source: Vec<u8>,
    pub source_is_file: bool,
    pub source_size: u64,
    pub time_limit_s: f64,
    pub memory_kib: u64,
    pub short_circuit: bool,
    pub pretests_only: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResumeDecision {
    Continue(u32),
    Drop,
}

#[derive(Debug)]
pub enum BridgeEvent {
    /// `max_cases_per_message` / `max_log_bytes` come from `Welcome.limits` (0 when the bridge sent none).
    Welcome {
        session_id: String,
        resume: Vec<(AssignmentRef, ResumeDecision)>,
        ping_interval_ms: u32,
        max_cases_per_message: u32,
        max_log_bytes: u32,
    },
    Ping(u64),
    Assign(Assignment),
    Generate(Box<GenerationPlan>),
    CancelGeneration(String),
    Abort(AssignmentRef),
    Ack(AssignmentRef, u32),
    Disconnect,
    Error(String),
    /// A oneof member this minor does not know (a newer bridge): ignored (spec §3.3).
    Unknown,
}

/// What `Hello` announces besides slots and in-flight runs. `protocol_minor` is
/// `judge_proto::PROTOCOL_MINOR` in the daemon.
#[derive(Debug, Clone, Default)]
pub struct HelloInfo {
    pub protocol_minor: u32,
    pub build: String,
    pub runtimes: Vec<(String, String, String)>,
    pub features: Vec<String>,
}

fn ref_of(r: Option<v1::AssignmentRef>) -> Result<AssignmentRef, WireError> {
    let r = r.ok_or_else(|| WireError("missing ref".into()))?;
    Ok(AssignmentRef { submission_id: r.submission_id, generation: r.generation })
}

fn proto_ref(r: &AssignmentRef) -> Option<v1::AssignmentRef> {
    Some(v1::AssignmentRef { submission_id: r.submission_id.clone(), generation: r.generation })
}

fn assignment(a: v1::Assign) -> Result<Assignment, WireError> {
    if a.source.len() > MAX_INLINE_SOURCE {
        return Err(WireError("inline source over 1 MiB".into()));
    }
    Ok(Assignment {
        r: ref_of(a.r#ref)?,
        problem_code: a.problem_code,
        data_version: a.data_version,
        language_key: a.language_key,
        source: a.source,
        source_is_file: a.source_is_file,
        source_size: a.source_size,
        time_limit_s: a.time_limit_s,
        memory_kib: a.memory_limit_kib,
        short_circuit: a.short_circuit,
        pretests_only: a.pretests_only,
    })
}

fn resume(r: v1::Resume) -> Result<(AssignmentRef, ResumeDecision), WireError> {
    let decision = if r.action == v1::resume::Action::Continue as i32 {
        ResumeDecision::Continue(r.from_seq)
    } else {
        ResumeDecision::Drop
    };
    Ok((ref_of(r.r#ref)?, decision))
}

pub fn from_bridge(message: BridgeMessage) -> Result<BridgeEvent, WireError> {
    use bridge_message::Body as B;
    let Some(body) = message.body else { return Ok(BridgeEvent::Unknown) };
    Ok(match body {
        B::Welcome(w) => {
            let limits = w.limits.unwrap_or_default();
            BridgeEvent::Welcome {
                session_id: w.session_id,
                resume: w.resume.into_iter().map(resume).collect::<Result<_, _>>()?,
                ping_interval_ms: w.ping_interval_ms,
                max_cases_per_message: limits.max_cases_per_message,
                max_log_bytes: limits.max_log_bytes,
            }
        }
        B::Ping(p) => BridgeEvent::Ping(p.nonce),
        B::Assign(a) => BridgeEvent::Assign(assignment(a)?),
        B::Abort(a) => BridgeEvent::Abort(ref_of(a.r#ref)?),
        B::Ack(a) => BridgeEvent::Ack(ref_of(a.r#ref)?, a.seq),
        B::Disconnect(_) => BridgeEvent::Disconnect,
        B::Error(e) => BridgeEvent::Error(e.message),
        B::Generate(plan) => BridgeEvent::Generate(Box::new(generation_plan(plan))),
        B::CancelGeneration(cancel) => BridgeEvent::CancelGeneration(cancel.generation_id),
    })
}

fn program_limits(limits: Option<v1::ProgramLimits>) -> ProgramLimits {
    let limits = limits.unwrap_or_default();
    ProgramLimits { time_s: limits.time_s, wall_s: limits.wall_s, memory_kib: limits.memory_kib }
}

fn plan_limits(limits: Option<v1::GenerationLimits>) -> PlanLimits {
    let limits = limits.unwrap_or_default();
    PlanLimits {
        generator: program_limits(limits.generator),
        validator: program_limits(limits.validator),
        solution: program_limits(limits.solution),
        max_file_bytes: limits.max_file_bytes,
        max_total_bytes: limits.max_total_bytes,
        max_upload_bytes: limits.max_upload_bytes,
    }
}

fn role(value: i32) -> Role {
    use v1::generation_program::Role as R;
    match R::try_from(value) {
        Ok(R::Generator) => Role::Generator,
        Ok(R::Validator) => Role::Validator,
        Ok(R::Solution) => Role::Solution,
        Ok(R::Checker) => Role::Checker,
        Ok(R::Interactor) => Role::Interactor,
        _ => Role::Unspecified,
    }
}

fn plan_program(program: v1::GenerationProgram) -> PlanProgram {
    PlanProgram {
        role: role(program.role),
        name: program.name,
        language_key: program.language_key,
        source_path: program.source_path,
    }
}

fn plan_test(test: v1::GenerationTest) -> PlanTest {
    PlanTest {
        index: test.index,
        input_name: test.input_name,
        answer_name: test.answer_name,
        group: test.group,
        manual_path: test.manual_path,
        generator: test.generator,
        args: test.args,
    }
}

/// The plan as the grader runs it; whether it can be run is `check_plan`'s question, not the session's.
pub fn generation_plan(message: v1::GenerateTests) -> GenerationPlan {
    // the solution's files are used only when both are named
    let file_io = (!message.input_file.is_empty() && !message.output_file.is_empty())
        .then_some(FileIo { input: message.input_file, output: message.output_file });
    GenerationPlan {
        id: message.generation_id,
        bundle_size: message.bundle_size,
        deadline_s: message.deadline_s,
        limits: plan_limits(message.limits),
        programs: message.programs.into_iter().map(plan_program).collect(),
        tests: message.tests.into_iter().map(plan_test).collect(),
        file_io,
    }
}

fn status(s: Status) -> i32 {
    (match s {
        Status::Ac => v1::Status::Ac,
        Status::Wa => v1::Status::Wa,
        Status::Tle => v1::Status::Tle,
        Status::Mle => v1::Status::Mle,
        Status::Ole => v1::Status::Ole,
        Status::Rte => v1::Status::Rte,
        Status::Ir => v1::Status::Ir,
        Status::Sc => v1::Status::Sc,
    }) as i32
}

fn case(c: &CaseReport) -> v1::Case {
    v1::Case {
        position: c.position,
        status: status(c.status),
        time_s: c.time_s,
        memory_kib: c.memory_kib,
        points: c.points,
        total: c.total,
        batch: c.batch.unwrap_or(0),
        feedback: c.feedback.clone(),
        extended_feedback: c.extended_feedback.clone(),
        output_prefix: c.output_prefix.clone(),
    }
}

fn reason(r: RetryReason) -> v1::RetryReason {
    match r {
        RetryReason::DataUnavailable => v1::RetryReason::DataUnavailable,
        RetryReason::RuntimeMissing => v1::RetryReason::RuntimeMissing,
        RetryReason::SandboxFailure => v1::RetryReason::SandboxFailure,
        RetryReason::Unsupported => v1::RetryReason::Unsupported,
    }
}

fn outcome_code(outcome: &Outcome) -> (v1::Outcome, i32) {
    match outcome {
        Outcome::Graded => (v1::Outcome::Graded, 0),
        Outcome::CompileError(_) => (v1::Outcome::CompileError, 0),
        Outcome::InternalError(_) => (v1::Outcome::InternalError, 0),
        Outcome::Aborted => (v1::Outcome::Aborted, 0),
        Outcome::Retry(r) => (v1::Outcome::Retry, reason(*r) as i32),
    }
}

fn body(r: &AssignmentRef, seq: u32, b: &Body) -> judge_message::Body {
    use judge_message::Body as J;
    let r#ref = proto_ref(r);
    match b {
        Body::Accepted => J::Accepted(v1::Accepted { r#ref, seq }),
        Body::Compiled { log } => J::Compiled(v1::Compiled { r#ref, seq, log: log.clone() }),
        Body::Started { pretested, case_count } => {
            J::Started(v1::GradingStarted { r#ref, seq, pretested: *pretested, case_count: *case_count })
        }
        Body::Cases(cases) => {
            J::Cases(v1::CaseResults { r#ref, seq, cases: cases.iter().map(case).collect() })
        }
        Body::Final { outcome, log, data_version_used } => {
            let (code, why) = outcome_code(outcome);
            J::Final(v1::Final {
                r#ref,
                seq,
                outcome: code as i32,
                log: log.clone(),
                reason: why,
                data_version_used: *data_version_used,
            })
        }
    }
}

pub fn to_judge(r: &AssignmentRef, envelope: &Envelope) -> JudgeMessage {
    JudgeMessage { body: Some(body(r, envelope.seq, &envelope.body)) }
}

pub fn seq_of(message: &JudgeMessage) -> Option<u32> {
    use judge_message::Body as J;
    match message.body.as_ref()? {
        J::Accepted(m) => Some(m.seq),
        J::Compiled(m) => Some(m.seq),
        J::Started(m) => Some(m.seq),
        J::Cases(m) => Some(m.seq),
        J::Final(m) => Some(m.seq),
        _ => None,
    }
}

fn plain(body: judge_message::Body) -> JudgeMessage {
    JudgeMessage { body: Some(body) }
}

pub fn hello(info: &HelloInfo, slots: u32, in_flight: &[(AssignmentRef, u32)]) -> JudgeMessage {
    plain(judge_message::Body::Hello(v1::Hello {
        protocol_minor: info.protocol_minor,
        build: info.build.clone(),
        slots,
        runtimes: info
            .runtimes
            .iter()
            .map(|(k, n, v)| v1::Runtime { language_key: k.clone(), name: n.clone(), version: v.clone() })
            .collect(),
        features: info.features.clone(),
        in_flight: in_flight
            .iter()
            .map(|(r, seq)| v1::InFlight { r#ref: proto_ref(r), last_seq_sent: *seq })
            .collect(),
    }))
}

pub fn pong(nonce: u64, busy: u32, slots: u32) -> JudgeMessage {
    plain(judge_message::Body::Pong(v1::Pong { nonce, busy, load: busy as f64 / slots.max(1) as f64 }))
}

pub fn slots_full(r: &AssignmentRef) -> JudgeMessage {
    let message = format!("no free slot for {}", r.submission_id);
    plain(judge_message::Body::Error(v1::JudgeError {
        code: v1::ErrorCode::SlotsFull as i32,
        message,
        r#ref: proto_ref(r),
    }))
}

/// A generation has no `AssignmentRef`, so the bridge finds the generation in the text.
pub fn generation_slots_full(generation_id: &str) -> JudgeMessage {
    plain(judge_message::Body::Error(v1::JudgeError {
        code: v1::ErrorCode::SlotsFull as i32,
        message: format!("no free slot for generation {generation_id}"),
        r#ref: None,
    }))
}

fn stage(stage: Stage) -> v1::generation_failure::Stage {
    use v1::generation_failure::Stage as S;
    match stage {
        Stage::Compile => S::Compile,
        Stage::Generate => S::Generate,
        Stage::Validate => S::Validate,
        Stage::Solve => S::Solve,
        Stage::Job => S::Job,
    }
}

fn kind(kind: Kind) -> v1::generation_failure::Kind {
    use v1::generation_failure::Kind as K;
    match kind {
        Kind::CompileError => K::CompileError,
        Kind::Crashed => K::Crashed,
        Kind::TimeLimit => K::TimeLimit,
        Kind::MemoryLimit => K::MemoryLimit,
        Kind::OutputLimit => K::OutputLimit,
        Kind::Rejected => K::Rejected,
        Kind::MissingFile => K::MissingFile,
        Kind::UnsupportedLanguage => K::UnsupportedLanguage,
        Kind::JobTimeLimit => K::JobTimeLimit,
        Kind::Internal => K::Internal,
    }
}

fn failure(failure: &GenerationFailure) -> v1::GenerationFailure {
    v1::GenerationFailure {
        stage: stage(failure.stage) as i32,
        kind: kind(failure.kind) as i32,
        program: failure.program.clone(),
        test_index: failure.test_index,
        detail: failure.detail.clone(),
    }
}

/// `GenerationDone` for a finished generation; the failure's detail is sent as it is (clip it first).
pub fn generation_done(generation_id: &str, result: &Result<(), GenerationFailure>) -> JudgeMessage {
    plain(judge_message::Body::GenerationDone(v1::GenerationDone {
        generation_id: generation_id.to_string(),
        ok: result.is_ok(),
        failure: result.as_ref().err().map(failure),
    }))
}

pub fn draining() -> JudgeMessage {
    plain(judge_message::Body::Draining(v1::Draining {}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use judge_grader::report::CaseReport;
    use judge_problem::verdict::Status;

    fn r() -> AssignmentRef {
        AssignmentRef { submission_id: "0192-uuid".into(), generation: 4 }
    }

    #[test]
    fn cases_keep_batch_as_one_based_or_zero() {
        let case = |batch| CaseReport {
            position: 1,
            status: Status::Wa,
            time_s: 0.5,
            memory_kib: 9,
            points: 0.0,
            total: 10.0,
            batch,
            feedback: "f".into(),
            extended_feedback: String::new(),
            output_prefix: "p".into(),
        };
        let message =
            to_judge(&r(), &Envelope { seq: 1, body: Body::Cases(vec![case(None), case(Some(2))]) });
        let Some(judge_message::Body::Cases(cases)) = message.body else { panic!() };
        assert_eq!(cases.cases.iter().map(|c| c.batch).collect::<Vec<_>>(), [0, 2]);
        assert_eq!(cases.cases[0].status, v1::Status::Wa as i32);
    }

    fn assign(source: Vec<u8>, with_ref: bool) -> BridgeMessage {
        BridgeMessage {
            body: Some(bridge_message::Body::Assign(v1::Assign {
                r#ref: with_ref.then(|| v1::AssignmentRef { submission_id: "s".into(), generation: 1 }),
                problem_code: "e2e_sum".into(),
                data_version: 2,
                language_key: "PY3".into(),
                source,
                time_limit_s: 1.0,
                memory_limit_kib: 262_144,
                ..Default::default()
            })),
        }
    }

    #[test]
    fn an_unknown_oneof_member_is_ignored() {
        use prost::Message;
        // Field 99, length-delimited, empty: a member a later minor might add.
        let message = BridgeMessage::decode(&[0x9a, 0x06, 0x00][..]).unwrap();
        assert!(matches!(from_bridge(message), Ok(BridgeEvent::Unknown)));
    }

    fn generate_message() -> v1::GenerateTests {
        v1::GenerateTests {
            generation_id: "g1".into(),
            bundle_size: 10,
            deadline_s: 60,
            limits: Some(v1::GenerationLimits {
                solution: Some(v1::ProgramLimits { time_s: 4.0, wall_s: 13.0, memory_kib: 262_144 }),
                max_file_bytes: 7,
                ..Default::default()
            }),
            programs: vec![
                v1::GenerationProgram {
                    name: "gen".into(),
                    role: v1::generation_program::Role::Generator as i32,
                    language_key: "CPP17".into(),
                    source_path: "files/gen.cpp".into(),
                },
                v1::GenerationProgram { name: "odd".into(), role: 99, ..Default::default() },
            ],
            tests: vec![v1::GenerationTest {
                index: 3,
                input_name: "03.inp".into(),
                answer_name: "03.out".into(),
                group: "2".into(),
                generator: "gen".into(),
                args: vec!["5".into()],
                ..Default::default()
            }],
            input_file: "in.txt".into(),
            output_file: String::new(),
        }
    }

    #[test]
    fn a_generation_plan_keeps_its_fields_and_file_io_needs_both_names() {
        let plan = generation_plan(generate_message());
        assert_eq!((plan.id.as_str(), plan.bundle_size, plan.deadline_s), ("g1", 10, 60));
        assert_eq!((plan.limits.solution.time_s, plan.limits.solution.memory_kib), (4.0, 262_144));
        assert_eq!(plan.limits.max_file_bytes, 7);
        assert_eq!(plan.programs[0].role, Role::Generator);
        assert_eq!(plan.programs[1].role, Role::Unspecified, "an unknown role is the plan check's to refuse");
        let test = &plan.tests[0];
        assert_eq!(
            (test.index, test.group.as_str(), test.generator.as_str(), test.args.len()),
            (3, "2", "gen", 1)
        );
        assert!(plan.file_io.is_none());
        let both = v1::GenerateTests { output_file: "out.txt".into(), ..generate_message() };
        let io = generation_plan(both).file_io.unwrap();
        assert_eq!((io.input.as_str(), io.output.as_str()), ("in.txt", "out.txt"));
    }

    #[test]
    fn generate_and_cancel_are_bridge_events() {
        let generate = BridgeMessage { body: Some(bridge_message::Body::Generate(generate_message())) };
        assert!(matches!(from_bridge(generate), Ok(BridgeEvent::Generate(plan)) if plan.id == "g1"));
        let cancel = v1::CancelGeneration { generation_id: "g1".into() };
        let cancel = BridgeMessage { body: Some(bridge_message::Body::CancelGeneration(cancel)) };
        assert!(matches!(from_bridge(cancel), Ok(BridgeEvent::CancelGeneration(id)) if id == "g1"));
    }

    #[test]
    fn generation_done_carries_ok_or_the_failure() {
        let ok = generation_done("g1", &Ok(()));
        let Some(judge_message::Body::GenerationDone(ok)) = ok.body else { panic!() };
        assert_eq!((ok.generation_id.as_str(), ok.ok, ok.failure), ("g1", true, None));
        let failure =
            GenerationFailure::new(Stage::Solve, Kind::TimeLimit, "slow").of_program("sol").at_test(4);
        let Some(judge_message::Body::GenerationDone(done)) = generation_done("g1", &Err(failure)).body
        else {
            panic!()
        };
        let failure = done.failure.unwrap();
        assert!(!done.ok);
        assert_eq!(failure.stage, v1::generation_failure::Stage::Solve as i32);
        assert_eq!(failure.kind, v1::generation_failure::Kind::TimeLimit as i32);
        assert_eq!(
            (failure.program.as_str(), failure.test_index, failure.detail.as_str()),
            ("sol", 4, "slow")
        );
    }

    #[test]
    fn assign_is_validated() {
        assert!(matches!(from_bridge(assign(vec![b'x'; 1 << 20], true)), Ok(BridgeEvent::Assign(_))));
        assert!(from_bridge(assign(vec![b'x'; (1 << 20) + 1], true)).is_err(), "inline source over 1 MiB");
        assert!(from_bridge(assign(b"x".to_vec(), false)).is_err(), "missing ref");
    }
}
