//! The only place that knows proto types (plan A's `judge_proto::v1`).

use crate::outbox::{AssignmentRef, Body, Envelope};
use judge_grader::judge::{Outcome, RetryReason};
use judge_grader::report::CaseReport;
use judge_problem::verdict::Status;
pub use judge_proto::v1;
pub use judge_proto::v1::{BridgeMessage, JudgeMessage, bridge_message, judge_message};

pub const MAX_INLINE_SOURCE: usize = 1 << 20;
/// What this judge grades beyond plain programs (`Hello.features`). SCRATCH is not supported.
pub const FEATURES: [&str; 7] =
    ["interactive", "file_io", "output_only", "signature", "checker.testlib", "checker.default", "text"];

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
    })
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

pub fn draining() -> JudgeMessage {
    plain(judge_message::Body::Draining(v1::Draining {}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use judge_grader::judge::{Outcome, RetryReason};
    use judge_grader::report::CaseReport;
    use judge_problem::verdict::Status;

    fn r() -> AssignmentRef {
        AssignmentRef { submission_id: "0192-uuid".into(), generation: 4 }
    }

    #[test]
    fn every_assignment_message_carries_its_ref_and_seq() {
        let message =
            to_judge(&r(), &Envelope { seq: 7, body: Body::Started { pretested: true, case_count: 12 } });
        assert_eq!(seq_of(&message), Some(7));
        let Some(judge_message::Body::Started(started)) = message.body else { panic!() };
        assert_eq!((started.pretested, started.case_count, started.seq), (true, 12, 7));
        assert_eq!(started.r#ref.as_ref().unwrap().submission_id, "0192-uuid");
        assert_eq!(started.r#ref.as_ref().unwrap().generation, 4);
        let accepted = to_judge(&r(), &Envelope { seq: 1, body: Body::Accepted });
        assert_eq!(seq_of(&accepted), Some(1));
        assert_eq!(seq_of(&pong(1, 0, 1)), None);
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

    #[test]
    fn final_maps_outcome_and_reason() {
        let body = Body::Final {
            outcome: Outcome::Retry(RetryReason::DataUnavailable),
            log: String::new(),
            data_version_used: 0,
        };
        let Some(judge_message::Body::Final(f)) = to_judge(&r(), &Envelope { seq: 3, body }).body else {
            panic!()
        };
        assert_eq!(
            (f.outcome, f.reason),
            (v1::Outcome::Retry as i32, v1::RetryReason::DataUnavailable as i32)
        );
        let ce =
            Body::Final { outcome: Outcome::CompileError("x".into()), log: "x".into(), data_version_used: 5 };
        let Some(judge_message::Body::Final(f)) = to_judge(&r(), &Envelope { seq: 3, body: ce }).body else {
            panic!()
        };
        assert_eq!(
            (f.outcome, f.log.as_str(), f.data_version_used),
            (v1::Outcome::CompileError as i32, "x", 5)
        );
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

    #[test]
    fn assign_is_validated() {
        let Ok(BridgeEvent::Assign(a)) = from_bridge(assign(b"print(1)".to_vec(), true)) else { panic!() };
        assert_eq!((a.problem_code.as_str(), a.data_version, a.memory_kib), ("e2e_sum", 2, 262_144));
        assert!(from_bridge(assign(vec![b'x'; (1 << 20) + 1], true)).is_err(), "inline source over 1 MiB");
        assert!(from_bridge(assign(b"x".to_vec(), false)).is_err(), "missing ref");
    }

    #[test]
    fn welcome_resume_actions() {
        let welcome = v1::Welcome {
            resume: vec![
                v1::Resume {
                    r#ref: Some(v1::AssignmentRef { submission_id: "a".into(), generation: 1 }),
                    action: v1::resume::Action::Continue as i32,
                    from_seq: 4,
                },
                v1::Resume {
                    r#ref: Some(v1::AssignmentRef { submission_id: "b".into(), generation: 1 }),
                    action: v1::resume::Action::Drop as i32,
                    from_seq: 0,
                },
            ],
            session_id: "sess-1".into(),
            ping_interval_ms: 10_000,
            limits: Some(v1::Limits { max_cases_per_message: 32, max_log_bytes: 1000, ..Default::default() }),
            ..Default::default()
        };
        let Ok(BridgeEvent::Welcome { resume, session_id, max_cases_per_message, max_log_bytes, .. }) =
            from_bridge(BridgeMessage { body: Some(bridge_message::Body::Welcome(welcome)) })
        else {
            panic!()
        };
        assert_eq!(session_id, "sess-1");
        assert_eq!((max_cases_per_message, max_log_bytes), (32, 1000));
        assert_eq!(resume[0].1, ResumeDecision::Continue(4));
        assert_eq!(resume[1].1, ResumeDecision::Drop);
    }

    #[test]
    fn hello_lists_slots_features_and_in_flight() {
        let info = HelloInfo {
            protocol_minor: 1,
            build: "0.1.0".into(),
            runtimes: vec![("PY3".into(), "Python 3".into(), "3.13".into())],
            features: FEATURES.map(String::from).to_vec(),
        };
        let Some(judge_message::Body::Hello(h)) = hello(&info, 3, &[(r(), 9)]).body else { panic!() };
        assert_eq!((h.protocol_minor, h.slots, h.runtimes.len(), h.in_flight[0].last_seq_sent), (1, 3, 1, 9));
        assert_eq!(h.in_flight[0].r#ref.as_ref().unwrap().submission_id, "0192-uuid");
        assert!(h.features.contains(&"signature".to_string()));
        assert!(!h.features.iter().any(|f| f.contains("scratch")), "SCRATCH is not graded");
    }
}
