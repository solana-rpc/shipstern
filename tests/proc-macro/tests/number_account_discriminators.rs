//!
//! Account field discriminators that are numbers, as Shank-style IDLs write them.
//! The parser used to drop these accounts, so `try_unpack` never matched them.
//!

use shipstern_proc_macro::include_shipstern_parser;

include_shipstern_parser!("../idls/number_account_discriminators.json");

use number_account_discriminators::{account::Account, NumberAccountDiscriminatorsAccount};

/// An omitted `u8` key is not in the struct, so the body starts after it.
#[test]
fn omitted_u8_key_selects_the_account() {
    let mut data = vec![183_u8, 1];

    data.extend_from_slice(&7_u64.to_le_bytes());
    data.extend_from_slice(&9_u32.to_le_bytes());

    let parsed = NumberAccountDiscriminatorsAccount::try_unpack(&data).expect("holder");

    let Account::Holder(holder) = parsed.account else {
        panic!("expected Holder, got {:?}", parsed.account);
    };

    assert_eq!(holder.maybe_amount, Some(7));
    assert_eq!(holder.tail, 9);
}

/// A `u16` key kept in the struct is matched at full width and decoded with the body.
#[test]
fn struct_u16_key_selects_the_account() {
    let mut data = 513_u16.to_le_bytes().to_vec();

    data.push(0);
    data.extend_from_slice(&5_u32.to_le_bytes());

    let parsed = NumberAccountDiscriminatorsAccount::try_unpack(&data).expect("keyed");

    let Account::Keyed(keyed) = parsed.account else {
        panic!("expected Keyed, got {:?}", parsed.account);
    };

    assert_eq!(keyed.key, 513);
    assert_eq!(keyed.maybe_amount, None);
    assert_eq!(keyed.tail, 5);
}

/// Any other key is still unknown.
#[test]
fn other_keys_are_unknown() {
    assert!(NumberAccountDiscriminatorsAccount::try_unpack(&[184, 0, 0, 0, 0, 0]).is_err());
}
