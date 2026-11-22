//!
//! Account and argument names that are Rust keywords. The generated field idents
//! must be raw, or suffixed for the paths that cannot be raw.
//!

use shipstern_proc_macro::include_shipstern_parser;
use shipstern_test_utils::check_protobuf_format;

include_shipstern_parser!("../idls/keyword_names.json");

#[test]
fn parses_instruction_with_keyword_names() {
    let path = shipstern_core::instruction::Path::new_single(0);
    let accounts: Vec<_> = (0..14u8)
        .map(|i| shipstern_core::Pubkey::new([i + 1; 32]))
        .collect();

    let mut data = vec![0xc4_u8];

    for i in 0..14u64 {
        data.extend_from_slice(&(i + 100).to_le_bytes());
    }

    let parsed = keyword_names::resolve_instruction_default(&accounts, &data, &path)
        .expect("keyword-named fields should parse");

    let keyword_names::instruction::Instruction::Touch { accounts, args } = parsed.instruction;

    assert_eq!(accounts.r#type, shipstern_core::Pubkey::new([1; 32]));
    assert_eq!(accounts.self_, shipstern_core::Pubkey::new([10; 32]));
    assert_eq!(args.r#type, 100);
    assert_eq!(args.self_, 109);
    assert!(format!("{args:?}").contains("type: 100"));
}

#[test]
fn parses_account_with_keyword_names() {
    let mut data = vec![0xc4_u8];

    for i in 0..14u64 {
        data.extend_from_slice(&(i + 100).to_le_bytes());
    }

    let parsed = keyword_names::KeywordNamesAccount::try_unpack(&data)
        .expect("keyword-named fields should parse");

    let keyword_names::account::Account::Holder(holder) = parsed.account;

    assert_eq!(holder.r#match, 101);
    assert_eq!(holder.crate_, 111);
}

#[test]
fn check_protobuf_schema() {
    check_protobuf_format(keyword_names::PROTOBUF_SCHEMA);

    insta::assert_snapshot!(keyword_names::PROTOBUF_SCHEMA);
}
