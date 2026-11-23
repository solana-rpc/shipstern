//!
//! An `omitted` field behind a constant or size discriminator.
//!
//! The omitted field is still on the wire. A constant discriminator strips only its own bytes,
//! and a size discriminator strips none, so the omitted version byte has to be read.
//!

use shipstern_proc_macro::include_shipstern_parser;

include_shipstern_parser!("../idls/omitted_constant_discriminator.json");

use omitted_constant_discriminator::{account::Account, OmittedConstantDiscriminatorAccount};

#[test]
fn constant_discriminator_leaves_the_version_byte_to_the_struct() {
    let mut data = vec![0xb7, 1];
    data.extend_from_slice(&0x0102_0304_0506_0708_u64.to_le_bytes());

    let parsed = OmittedConstantDiscriminatorAccount::try_unpack(&data).expect("decodes");
    let Account::ConstantVersioned(account) = parsed.account else {
        panic!("expected constantVersioned");
    };

    assert_eq!(account.version, 1);
    assert_eq!(account.amount, 0x0102_0304_0506_0708);
}

#[test]
fn size_discriminator_strips_nothing() {
    let mut data = vec![1];
    data.extend_from_slice(&0x0102_0304_0506_0708_u64.to_le_bytes());

    let parsed = OmittedConstantDiscriminatorAccount::try_unpack(&data).expect("decodes");
    let Account::SizedVersioned(account) = parsed.account else {
        panic!("expected sizedVersioned");
    };

    assert_eq!(account.version, 1);
    assert_eq!(account.amount, 0x0102_0304_0506_0708);
}
