//!
//! An IDL that omits `fixed` and `isOptional`, the way Codama's JavaScript
//! serializes them when they hold their default.
//!
//! codama 0.9 typed both `bool` with `#[serde(default)]`, so absent meant
//! `false`. codama 0.13 types them `Option<bool>`, so absent means `None`, and
//! the macro reads that as `.unwrap_or(false)`. Every other fixture in this
//! repository states both flags, so without this one the corpus never generates
//! a parser from the absent form and that equality is only covered by the
//! synthetic cases in `field_presence_tests`.
//!

use shipstern_proc_macro::include_shipstern_parser;

include_shipstern_parser!("../idls/omitted_presence_flags.json");

/// The account decodes with the option present.
#[test]
fn account_round_trips_with_the_option_present() {
    let mut data = vec![0xb7_u8];

    data.push(1);
    data.extend_from_slice(&7_u64.to_le_bytes());
    data.extend_from_slice(&9_u32.to_le_bytes());

    let parsed = omitted_presence_flags::OmittedPresenceFlagsAccount::try_unpack(&data)
        .expect("holder should decode");

    // The IDL declares one account, so this destructure is exhaustive.
    let omitted_presence_flags::account::Account::Holder(holder) = parsed.account;

    assert_eq!(holder.maybe_amount, Some(7));
    assert_eq!(holder.tail, 9);
}

///
/// The same account with the option absent.
///
/// A prefixed option is one byte of prefix and nothing else when it is `None`,
/// which is what an omitted `fixed` has to mean. A parser that read the absent
/// flag as `true` would pad the `None` out to the item's width and then read
/// `tail` from the wrong offset.
///
#[test]
fn account_round_trips_with_the_option_absent() {
    let mut data = vec![0xb7_u8];

    data.push(0);
    data.extend_from_slice(&9_u32.to_le_bytes());

    let parsed = omitted_presence_flags::OmittedPresenceFlagsAccount::try_unpack(&data)
        .expect("holder should decode");

    // The IDL declares one account, so this destructure is exhaustive.
    let omitted_presence_flags::account::Account::Holder(holder) = parsed.account;

    assert_eq!(holder.maybe_amount, None);
    assert_eq!(
        holder.tail, 9,
        "tail must sit directly after the one-byte None prefix",
    );
}

///
/// The instruction's second account is declared without `isOptional`, so it is
/// required and the parser must reject a call that omits it.
///
#[test]
fn instruction_accounts_are_required_when_the_flag_is_omitted() {
    let path = shipstern_core::instruction::Path::new_single(0);

    let mut data = vec![0xb7_u8];

    data.push(0);

    let one_account = [shipstern_core::Pubkey::new([1; 32])];

    assert!(
        omitted_presence_flags::resolve_instruction_default(&one_account, &data, &path).is_err(),
        "a missing account must fail while isOptional is absent, which reads as false",
    );

    let two_accounts = [
        shipstern_core::Pubkey::new([1; 32]),
        shipstern_core::Pubkey::new([2; 32]),
    ];

    omitted_presence_flags::resolve_instruction_default(&two_accounts, &data, &path)
        .expect("both accounts present should resolve");
}
