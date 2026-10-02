// Regression: loopscale.json's Pod* aliases are referenced before they're
// defined, which needs the two-pass `build_defined_types` (else E0412).

use shipstern_proc_macro::include_shipstern_parser;
use shipstern_test_utils::check_protobuf_format;

include_shipstern_parser!("../idls/loopscale.json");

#[test]
fn check_protobuf_schema() {
    check_protobuf_format(loopscale::PROTOBUF_SCHEMA);

    insta::assert_snapshot!(
        shipstern_test_utils::normalize_protobuf_schema_for_snapshot(loopscale::PROTOBUF_SCHEMA)
    );
}

#[test]
fn instruction_dispatch_index_is_some() {
    assert!(
        loopscale::INSTRUCTION_DISPATCH_MESSAGE_INDEX.is_some(),
        "expected InstructionDispatch message index for a program with instructions"
    );
}

#[test]
fn check_json_serialization() {
    // account
    let asset = loopscale::AssetData::default();
    let json_str = serde_json::to_string(&asset).expect("failed to json serialize");
    let _: loopscale::AssetData =
        serde_json::from_str(&json_str).expect("failed to json deserialize");

    // instruction
    let params = loopscale::CreateStrategyParams::default();
    let json_str = serde_json::to_string(&params).expect("failed to json serialize");
    let _: loopscale::CreateStrategyParams =
        serde_json::from_str(&json_str).expect("failed to json deserialize");
}
