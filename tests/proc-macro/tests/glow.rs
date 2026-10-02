// Regression: glow.json declares an account and a defined type both named
// `MarginAccount`, which used to fail with E0428. Accounts only, no instructions.

use shipstern_proc_macro::include_shipstern_parser;
use shipstern_test_utils::check_protobuf_format;

include_shipstern_parser!("../idls/glow.json");

#[test]
fn check_protobuf_schema() {
    check_protobuf_format(margin::PROTOBUF_SCHEMA);

    insta::assert_snapshot!(margin::PROTOBUF_SCHEMA);
}

#[test]
fn account_dispatch_index_is_some() {
    assert!(
        margin::ACCOUNT_DISPATCH_MESSAGE_INDEX.is_some(),
        "expected AccountDispatch message index to be present for an accounts-only IDL"
    );
}

#[test]
fn check_json_serialization() {
    // account
    let state = margin::LiquidationState::default();
    let json_str = serde_json::to_string(&state).expect("failed to json serialize");
    let _: margin::LiquidationState =
        serde_json::from_str(&json_str).expect("failed to json deserialize");

    // instruction
    let invoke = margin::LiquidatorInvokeBegin::default();
    let json_str = serde_json::to_string(&invoke).expect("failed to json serialize");
    let _: margin::LiquidatorInvokeBegin =
        serde_json::from_str(&json_str).expect("failed to json deserialize");
}
