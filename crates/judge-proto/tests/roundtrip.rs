use judge_proto::v1::{AssignmentRef, Case, CaseResults, JudgeMessage, Status, judge_message::Body};
use prost::Message;

fn case_results() -> JudgeMessage {
    let case = Case {
        position: 3,
        status: Status::Wa as i32,
        time_s: 0.25,
        memory_kib: 2048,
        points: 0.0,
        total: 10.0,
        batch: 2,
        feedback: "sai ở dòng 2 ✗".into(),
        ..Case::default()
    };
    JudgeMessage {
        body: Some(Body::Cases(CaseResults {
            r#ref: Some(AssignmentRef { submission_id: "0192-uuid".into(), generation: 4 }),
            seq: 5,
            cases: vec![case],
        })),
    }
}

#[test]
fn a_message_survives_binary_and_json_round_trips() {
    let message = case_results();
    let decoded = JudgeMessage::decode(message.encode_to_vec().as_slice()).unwrap();
    assert_eq!(decoded, message);
    let json = serde_json::to_string(&message).unwrap();
    assert_eq!(serde_json::from_str::<JudgeMessage>(&json).unwrap(), message);
}

#[test]
fn json_uses_proto3_names_and_enum_strings() {
    let json = serde_json::to_value(case_results()).unwrap();
    assert_eq!(json["cases"]["ref"]["submissionId"], "0192-uuid");
    assert_eq!(json["cases"]["cases"][0]["status"], "STATUS_WA");
    assert_eq!(json["cases"]["cases"][0]["memoryKib"], "2048");
}

#[test]
fn unknown_fields_are_skipped_not_rejected() {
    let mut bytes = case_results().encode_to_vec();
    // field 15 (unknown), wire type 0 (varint), value 1, appended at the top level
    bytes.extend_from_slice(&[0x78, 0x01]);
    assert_eq!(JudgeMessage::decode(bytes.as_slice()).unwrap(), case_results());
}
