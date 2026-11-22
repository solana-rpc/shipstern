//!
//! An `omitted` field that is not the discriminator.
//!
//! `defaultValueStrategy: omitted` only hides a field from the generated inputs. Its default
//! value is still on the wire, so a constant version byte after the discriminator has to be
//! read, or every field behind it decodes from the wrong offset.
//!

use borsh::BorshDeserialize;
use shipstern_proc_macro::include_shipstern_parser;

include_shipstern_parser!("../idls/omitted_constant_field.json");

#[test]
fn account_reads_the_version_byte_after_the_discriminator() {
    let mut data = vec![0xb7, 1];
    data.extend_from_slice(&0x0102_0304_0506_0708_u64.to_le_bytes());
    data.push(0xee);

    let parsed = omitted_constant_field::OmittedConstantFieldAccount::try_unpack(&data)
        .expect("account should decode");
    let omitted_constant_field::account::Account::Versioned(account) = parsed.account;

    assert_eq!(account.version, 1);
    assert_eq!(account.amount, 0x0102_0304_0506_0708);
    assert_eq!(account.tail, 0xee);
}

#[test]
fn instruction_reads_the_version_byte_after_the_discriminator() {
    let mut wire = vec![0x07, 1];
    wire.extend_from_slice(&0x0102_0304_0506_0708_u64.to_le_bytes());

    let path = shipstern_core::instruction::Path::new_single(0);
    let parsed = omitted_constant_field::resolve_instruction_default(&[], &wire, &path).unwrap();
    let omitted_constant_field::instruction::Instruction::Touch { args, .. } = parsed.instruction;

    assert_eq!(args.version, 1);
    assert_eq!(args.amount, 0x0102_0304_0506_0708);
}

#[test]
fn defined_struct_reads_its_omitted_field() {
    let mut wire = vec![2];
    wire.extend_from_slice(&0x0a0b_0c0d_u32.to_le_bytes());

    let parsed = omitted_constant_field::VersionedPayload::try_from_slice(&wire).unwrap();

    assert_eq!(parsed.version, 2);
    assert_eq!(parsed.value, 0x0a0b_0c0d);
}

#[test]
fn fixed_option_pads_none_to_the_omitted_field_too() {
    let none = [0, 0, 0, 0, 0, 0, 0xee];

    let parsed = omitted_constant_field::FixedVersionedOption::try_from_slice(&none).unwrap();

    assert_eq!(parsed.value, None);
    assert_eq!(parsed.tail, 0xee);
}
