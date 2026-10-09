use std::{fs, path::Path};

use judge_proto::shape::{
    FieldShape, MessageShape, MethodShape, SHAPE_FORMAT, Shape, bless_problems, breaking_changes,
    declaration_order_problems, shape_of,
};

fn stored_shape() -> Shape {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../proto/compat/v1.shape.json");
    serde_json::from_str(&fs::read_to_string(path).expect("run the bless_shape example once")).unwrap()
}

fn field(name: &str, kind: &str) -> FieldShape {
    FieldShape { name: name.into(), kind: kind.into(), ..FieldShape::default() }
}

fn one_message(fields: &[(i32, FieldShape)], reserved: (&[i32], &[&str])) -> Shape {
    let message = MessageShape {
        fields: fields.iter().cloned().collect(),
        reserved_numbers: reserved.0.to_vec(),
        reserved_names: reserved.1.iter().map(|name| name.to_string()).collect(),
    };
    Shape { format: SHAPE_FORMAT, messages: [(".p.M".to_string(), message)].into(), ..Shape::default() }
}

#[test]
fn the_current_contract_only_adds_to_the_frozen_v1_shape() {
    let current = shape_of(judge_proto::FILE_DESCRIPTOR_SET).unwrap();
    assert_eq!(breaking_changes(&stored_shape(), &current), Vec::<String>::new());
}

#[test]
fn fields_are_declared_in_number_order() {
    assert_eq!(declaration_order_problems(judge_proto::FILE_DESCRIPTOR_SET).unwrap(), Vec::<String>::new());
}

#[test]
fn adding_a_field_is_not_breaking() {
    let old = one_message(&[(1, field("a", "TYPE_UINT32"))], (&[], &[]));
    let new = one_message(&[(1, field("a", "TYPE_UINT32")), (2, field("b", "TYPE_STRING"))], (&[], &[]));
    assert!(breaking_changes(&old, &new).is_empty());
}

#[test]
fn retyping_renaming_or_dropping_a_field_is_breaking() {
    let old = one_message(&[(1, field("a", "TYPE_UINT32"))], (&[], &[]));
    for new in [
        one_message(&[(1, field("a", "TYPE_UINT64"))], (&[], &[])),
        one_message(&[(1, field("renamed", "TYPE_UINT32"))], (&[], &[])),
        one_message(&[], (&[], &[])),
    ] {
        assert_eq!(breaking_changes(&old, &new).len(), 1, "{new:?}");
    }
}

#[test]
fn a_removed_field_is_fine_once_its_number_and_name_are_reserved() {
    let old = one_message(&[(1, field("a", "TYPE_UINT32"))], (&[], &[]));
    let reserved = one_message(&[], (&[1], &["a"]));
    assert!(breaking_changes(&old, &reserved).is_empty());
    let reused = one_message(&[(1, field("other", "TYPE_UINT32"))], (&[], &[]));
    assert_eq!(breaking_changes(&reserved, &reused).len(), 1);
}

#[test]
fn renumbering_an_enum_value_is_breaking() {
    let enum_of = |number: i32| Shape {
        enums: [(".p.E".to_string(), [(number, "E_A".to_string())].into())].into(),
        ..Shape::default()
    };
    assert_eq!(breaking_changes(&enum_of(1), &enum_of(2)).len(), 1);
    assert!(breaking_changes(&enum_of(1), &enum_of(1)).is_empty());
}

#[test]
fn the_protocol_minor_matches_proto_version_json() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../proto/version.json");
    let version: serde_json::Value = serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap();
    assert_eq!(version["package"], "qhhoj.judge.v1");
    assert_eq!(version["minor"], judge_proto::PROTOCOL_MINOR);
}

fn with_rpc(method: MethodShape) -> Shape {
    let methods = [("Session".to_string(), method)].into();
    Shape { format: SHAPE_FORMAT, services: [(".p.S".to_string(), methods)].into(), ..Shape::default() }
}

fn bidi() -> MethodShape {
    MethodShape {
        input: ".p.In".into(),
        output: ".p.Out".into(),
        client_streaming: true,
        server_streaming: true,
    }
}

#[test]
fn the_frozen_shape_lists_the_rpcs_and_optional_fields() {
    let current = shape_of(judge_proto::FILE_DESCRIPTOR_SET).unwrap();
    let methods = &current.services[".qhhoj.judge.v1.JudgeLink"];
    assert!(methods["Session"].client_streaming && methods["Session"].server_streaming);
    assert!(methods["FetchSource"].server_streaming && !methods["FetchSource"].client_streaming);
    assert!(current.messages[".qhhoj.judge.v1.Meta"].fields[&1].optional);
    assert_eq!(current.messages[".qhhoj.judge.v1.JudgeMessage"].fields[&1].oneof.as_deref(), Some("body"));
}

#[test]
fn removing_renaming_or_restreaming_an_rpc_is_breaking() {
    let old = with_rpc(bidi());
    assert!(breaking_changes(&old, &old).is_empty());
    let renamed = Shape {
        services: [(".p.S".to_string(), [("Connect".to_string(), bidi())].into())].into(),
        ..old.clone()
    };
    let restreamed = with_rpc(MethodShape { client_streaming: false, ..bidi() });
    for new in [Shape { format: SHAPE_FORMAT, ..Shape::default() }, renamed, restreamed] {
        assert_eq!(breaking_changes(&old, &new).len(), 1, "{new:?}");
    }
}

#[test]
fn dropping_optional_or_leaving_a_oneof_is_breaking() {
    let optional = FieldShape { optional: true, ..field("a", "TYPE_SINT32") };
    let old = one_message(&[(1, optional)], (&[], &[]));
    let plain = one_message(&[(1, field("a", "TYPE_SINT32"))], (&[], &[]));
    assert_eq!(breaking_changes(&old, &plain).len(), 1);
    let grouped = FieldShape { oneof: Some("body".into()), ..field("a", "TYPE_SINT32") };
    let in_oneof = one_message(&[(1, grouped.clone())], (&[], &[]));
    assert_eq!(breaking_changes(&plain, &in_oneof).len(), 1);
    assert_eq!(breaking_changes(&in_oneof, &plain).len(), 1);
}

#[test]
fn a_shape_of_the_older_format_is_not_compared_on_optional_or_oneof() {
    let optional = FieldShape { optional: true, ..field("a", "TYPE_SINT32") };
    let mut old = one_message(&[(1, field("a", "TYPE_SINT32"))], (&[], &[]));
    old.format = 1;
    assert!(breaking_changes(&old, &one_message(&[(1, optional)], (&[], &[]))).is_empty());
}

#[test]
fn blessing_needs_a_version_bump_and_an_explicit_flag_for_breaking_changes() {
    let stored = Shape { minor: 1, ..one_message(&[(1, field("a", "TYPE_UINT32"))], (&[], &[])) };
    let additive = Shape {
        minor: 2,
        ..one_message(&[(1, field("a", "TYPE_UINT32")), (2, field("b", "TYPE_STRING"))], (&[], &[]))
    };
    let breaking = Shape { minor: 2, ..one_message(&[(1, field("a", "TYPE_UINT64"))], (&[], &[])) };
    assert!(bless_problems(&stored, &stored, false).is_ok(), "nothing changed");
    assert!(bless_problems(&stored, &additive, false).is_ok());
    let unbumped = Shape { minor: 1, ..additive.clone() };
    assert!(bless_problems(&stored, &unbumped, false).unwrap_err().contains("bump"));
    assert!(bless_problems(&stored, &breaking, false).unwrap_err().contains("--allow-breaking"));
    assert!(bless_problems(&stored, &breaking, true).is_ok());
    let unbumped_breaking = Shape { minor: 1, ..breaking };
    assert!(bless_problems(&stored, &unbumped_breaking, true).is_err(), "the flag does not waive the bump");
}
