use shipstern_proc_macro::include_shipstern_parser;

include_shipstern_parser!("../idls/discriminator_guards.json");

///
/// A well-formed fixed-size field discriminator: declared width and decoded
/// default bytes agree, so the constant is emitted and the matcher honors it.
///
#[test]
fn well_formed_ix_const_is_matchable() {
    let disc = discriminator_guards::Instructions::WELL_FORMED_IX_DISCRIMINATOR;
    let offset = discriminator_guards::Instructions::WELL_FORMED_IX_DISCRIMINATOR_OFFSET;

    let mut data = vec![0_u8; offset];

    data.extend_from_slice(disc);

    let path = shipstern_core::instruction::Path::new_single(0);

    assert!(!matches!(
        discriminator_guards::resolve_instruction_default(&[], &data, &path),
        Err(shipstern_core::ParseError::DiscriminatorNotFound(_))
    ));
}

///
/// Arms that can never match get no constant: `widthMismatchIx` (8-byte field,
/// 4-byte default) and the `sizedOnly`, `emptyDiscriminator` and `widthMismatch`
/// accounts. That is checked at compile time, so this only checks the accounts
/// that can match still parse.
///
#[test]
fn guarded_accounts_still_parse() {
    // sizedOnly: 9 bytes total (u64 + u8), matched purely on length.
    let mut data = 7_u64.to_le_bytes().to_vec();

    data.push(1);

    let parsed = discriminator_guards::DiscriminatorGuardsAccount::try_unpack(&data)
        .expect("size discriminator should match");

    let discriminator_guards::account::Account::SizedOnly(sized) = parsed.account else {
        panic!("expected SizedOnly");
    };

    assert_eq!(sized.amount, 7);
    assert_eq!(sized.flag, 1);
}
