use std::fs;

use judge_proto::golden::{decode_to_json, encode, fixture_names, golden_dir, read_fixture};

fn stored_bytes(name: &str) -> Vec<u8> {
    fs::read(golden_dir().join(format!("{name}.bin"))).expect("run the write_golden example")
}

#[test]
fn every_stored_binary_decodes_to_its_json() {
    for name in fixture_names().unwrap() {
        let fixture = read_fixture(&name).unwrap();
        let decoded = decode_to_json(&fixture.type_name, &stored_bytes(&name)).unwrap();
        let expected = decode_to_json(&fixture.type_name, &encode(&fixture).unwrap()).unwrap();
        assert_eq!(decoded, expected, "{name}");
        assert_ne!(decoded, serde_json::json!({}), "{name} decodes to an empty message");
    }
}

#[test]
fn a_present_zero_contest_number_is_kept_apart_from_an_absent_one() {
    let live = read_fixture("bridge-assign-contest-live").unwrap();
    let decoded = decode_to_json("BridgeMessage", &encode(&live).unwrap()).unwrap();
    assert_eq!(decoded["assign"]["meta"]["inContest"], 0);
    let outside = read_fixture("bridge-assign-inline").unwrap();
    let decoded = decode_to_json("BridgeMessage", &encode(&outside).unwrap()).unwrap();
    assert!(decoded["assign"]["meta"].get("inContest").is_none());
}
