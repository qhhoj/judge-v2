use std::{fs, path::Path};

use judge_proto::shape::{
    FieldShape, MessageShape, Shape, breaking_changes, declaration_order_problems, shape_of,
};

fn stored_shape() -> Shape {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../proto/compat/v1.shape.json");
    serde_json::from_str(&fs::read_to_string(path).expect("run the bless_shape example once")).unwrap()
}

fn field(name: &str, kind: &str) -> FieldShape {
    FieldShape { name: name.into(), kind: kind.into(), repeated: false }
}

fn one_message(fields: &[(i32, FieldShape)], reserved: (&[i32], &[&str])) -> Shape {
    let message = MessageShape {
        fields: fields.iter().cloned().collect(),
        reserved_numbers: reserved.0.to_vec(),
        reserved_names: reserved.1.iter().map(|name| name.to_string()).collect(),
    };
    Shape { messages: [(".p.M".to_string(), message)].into(), ..Shape::default() }
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
