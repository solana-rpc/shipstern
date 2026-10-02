// Regression: metaplex token_metadata has MapTypeNode fields, which used to panic
// `map_type()`. Accounts only, no instructions.

use shipstern_proc_macro::include_shipstern_parser;
use shipstern_test_utils::check_protobuf_format;

include_shipstern_parser!("../idls/metaplex.json");

#[test]
fn check_protobuf_schema() {
    check_protobuf_format(token_metadata::PROTOBUF_SCHEMA);

    insta::assert_snapshot!(
        shipstern_test_utils::normalize_protobuf_schema_for_snapshot(
            token_metadata::PROTOBUF_SCHEMA
        )
    );
}

#[test]
fn account_dispatch_index_is_some() {
    assert!(
        token_metadata::ACCOUNT_DISPATCH_MESSAGE_INDEX.is_some(),
        "expected AccountDispatch message index for an accounts-only IDL"
    );
}

#[test]
fn check_json_serialization() {
    // account
    let asset = token_metadata::AssetData::default();
    let json_str = serde_json::to_string(&asset).expect("failed to json serialize");
    let _: token_metadata::AssetData =
        serde_json::from_str(&json_str).expect("failed to json deserialize");

    // instruction
    let args = token_metadata::CreateMasterEditionArgs::default();
    let json_str = serde_json::to_string(&args).expect("failed to json serialize");
    let _: token_metadata::CreateMasterEditionArgs =
        serde_json::from_str(&json_str).expect("failed to json deserialize");
}
