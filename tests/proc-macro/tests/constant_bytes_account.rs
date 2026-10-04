use shipstern_proc_macro::include_shipstern_parser;

include_shipstern_parser!("../idls/constant_bytes_account.json");

#[test]
fn parses_account_after_byte_constant_discriminator() {
    let mut data = vec![1];
    data.extend_from_slice(&42_u64.to_le_bytes());

    let parsed = constant_bytes_account::ConstantBytesAccountAccount::try_unpack(&data)
        .expect("byte-valued constant discriminator should match");

    let constant_bytes_account::account::Account::Vault(vault) = parsed.account else {
        panic!("expected Vault");
    };

    assert_eq!(vault.amount, 42);
}

#[test]
fn rejects_an_unknown_byte_constant_discriminator() {
    let mut data = vec![2];
    data.extend_from_slice(&42_u64.to_le_bytes());

    assert!(constant_bytes_account::ConstantBytesAccountAccount::try_unpack(&data).is_err());
}

#[test]
fn reports_a_truncated_matching_account() {
    assert!(constant_bytes_account::ConstantBytesAccountAccount::try_unpack(&[1, 42]).is_err());
}

#[test]
fn exposes_the_byte_constant_discriminator() {
    assert_eq!(
        constant_bytes_account::Vault::DISCRIMINATOR,
        [0x01_u8].as_slice()
    );
    assert_eq!(constant_bytes_account::Vault::DISCRIMINATOR_OFFSET, 0);
}

#[test]
fn exposes_a_discriminator_at_a_nonzero_offset() {
    assert_eq!(
        constant_bytes_account::OffsetVault::DISCRIMINATOR,
        [0xbe_u8, 0xef].as_slice()
    );
    assert_eq!(constant_bytes_account::OffsetVault::DISCRIMINATOR_OFFSET, 8);
}

///
/// Build the on-wire buffer straight from the constants and confirm the parser
/// picks that variant: `prefix ++ DISCRIMINATOR ++ borsh body`.
///
/// This is what guards the emit rule: a constant exists only for a
/// discriminator the parser can actually match on.
///
#[test]
fn round_trips_a_discriminator_at_a_nonzero_offset() {
    let mut data = vec![0_u8; constant_bytes_account::OffsetVault::DISCRIMINATOR_OFFSET];

    data.extend_from_slice(constant_bytes_account::OffsetVault::DISCRIMINATOR);
    data.push(7);

    let parsed = constant_bytes_account::ConstantBytesAccountAccount::try_unpack(&data)
        .expect("discriminator built from the constants should match");

    let constant_bytes_account::account::Account::OffsetVault(vault) = parsed.account else {
        panic!("expected OffsetVault");
    };

    assert_eq!(vault.flag, 7);
}

///
/// The two accounts must not both match one buffer. Otherwise the match order,
/// not the constant, decides the result and the round-trip proves nothing.
///
#[test]
fn the_two_discriminators_do_not_overlap() {
    let mut vault_data = vec![1];
    vault_data.extend_from_slice(&42_u64.to_le_bytes());

    assert!(vault_data
        .get(
            constant_bytes_account::OffsetVault::DISCRIMINATOR_OFFSET
                ..constant_bytes_account::OffsetVault::DISCRIMINATOR_OFFSET
                    + constant_bytes_account::OffsetVault::DISCRIMINATOR.len()
        )
        .is_none_or(|slice| slice != constant_bytes_account::OffsetVault::DISCRIMINATOR));

    let mut offset_data = vec![0_u8; constant_bytes_account::OffsetVault::DISCRIMINATOR_OFFSET];

    offset_data.extend_from_slice(constant_bytes_account::OffsetVault::DISCRIMINATOR);
    offset_data.push(7);

    assert_ne!(
        &offset_data[..constant_bytes_account::Vault::DISCRIMINATOR.len()],
        constant_bytes_account::Vault::DISCRIMINATOR
    );
}

/// The account wrapper derives serde behind the `serde` feature, like the instruction wrapper.
#[test]
fn account_wrapper_round_trips_through_json() {
    let mut data = vec![1];
    data.extend_from_slice(&42_u64.to_le_bytes());

    let parsed = constant_bytes_account::ConstantBytesAccountAccount::try_unpack(&data)
        .expect("byte-valued constant discriminator should match");

    let json = serde_json::to_string(&parsed).expect("serialize");
    let back: constant_bytes_account::ConstantBytesAccountAccount =
        serde_json::from_str(&json).expect("deserialize");

    assert_eq!(back, parsed);
}
