//!
//! Plain, optional, repeated and fixed-array `shortU16` struct fields.
//!
//! A compact-u16 is 1 to 3 bytes on the wire. Read as a 2-byte `u16`, it
//! consumes the wrong bytes and shifts every field after it, so each struct
//! ends in a `tail` byte that only decodes right when the codec is exact.
//!

use borsh::BorshDeserialize;
use shipstern_proc_macro::include_shipstern_parser;

include_shipstern_parser!("../idls/short_u16_fields.json");

use short_u16_fields::{
    ShortU16Fixed, ShortU16List, ShortU16Optional, ShortU16Plain, ShortU16ShortPrefixOptional,
};

const TAIL: u8 = 0xee;

/// Wire bytes of the compact-u16 values the tests share, one per encoded length.
const ONE_BYTE: (u16, &[u8]) = (0x7f, &[0x7f]);
const TWO_BYTES: (u16, &[u8]) = (0x80, &[0x80, 0x01]);
const THREE_BYTES: (u16, &[u8]) = (0xffff, &[0xff, 0xff, 0x03]);

#[test]
fn plain_field_reads_one_to_three_bytes() {
    for (value, encoded) in [ONE_BYTE, TWO_BYTES, THREE_BYTES] {
        let mut wire = encoded.to_vec();
        wire.push(TAIL);

        let parsed = ShortU16Plain::try_from_slice(&wire).expect("plain field should decode");

        assert_eq!(parsed.count, value);
        assert_eq!(parsed.tail, TAIL);
        assert_eq!(borsh::to_vec(&parsed).unwrap(), wire);
    }
}

#[test]
fn plain_field_rejects_non_minimal_and_overflowing_encodings() {
    for bad in [&[0x80, 0x00, TAIL][..], &[0xff, 0xff, 0x04, TAIL], &[0x80]] {
        assert!(ShortU16Plain::try_from_slice(bad).is_err());
    }
}

#[test]
fn optional_field_with_a_u8_prefix() {
    let mut some = vec![1];
    some.extend_from_slice(TWO_BYTES.1);
    some.push(TAIL);

    let parsed = ShortU16Optional::try_from_slice(&some).unwrap();

    assert_eq!(parsed.value, Some(TWO_BYTES.0));
    assert_eq!(parsed.tail, TAIL);
    assert_eq!(borsh::to_vec(&parsed).unwrap(), some);

    let parsed = ShortU16Optional::try_from_slice(&[0, TAIL]).unwrap();

    assert_eq!(parsed.value, None);
    assert_eq!(parsed.tail, TAIL);
}

#[test]
fn optional_field_with_a_short_u16_prefix() {
    let mut some = vec![1];
    some.extend_from_slice(THREE_BYTES.1);
    some.push(TAIL);

    let parsed = ShortU16ShortPrefixOptional::try_from_slice(&some).unwrap();

    assert_eq!(parsed.value, Some(THREE_BYTES.0));
    assert_eq!(parsed.tail, TAIL);
    assert_eq!(borsh::to_vec(&parsed).unwrap(), some);

    let parsed = ShortU16ShortPrefixOptional::try_from_slice(&[0, TAIL]).unwrap();

    assert_eq!(parsed.value, None);
    assert_eq!(parsed.tail, TAIL);
}

#[test]
fn repeated_field_reads_each_item_with_its_own_width() {
    let mut wire = 3_u32.to_le_bytes().to_vec();
    for (_, encoded) in [ONE_BYTE, TWO_BYTES, THREE_BYTES] {
        wire.extend_from_slice(encoded);
    }
    wire.push(TAIL);

    let parsed = ShortU16List::try_from_slice(&wire).unwrap();

    assert_eq!(parsed.values, [ONE_BYTE.0, TWO_BYTES.0, THREE_BYTES.0]);
    assert_eq!(parsed.tail, TAIL);
    assert_eq!(borsh::to_vec(&parsed).unwrap(), wire);
}

#[test]
fn repeated_field_does_not_allocate_from_the_declared_count() {
    assert!(ShortU16List::try_from_slice(&u32::MAX.to_le_bytes()).is_err());
}

#[test]
fn fixed_array_field_reads_n_items_without_a_count() {
    let mut wire = TWO_BYTES.1.to_vec();
    wire.extend_from_slice(ONE_BYTE.1);
    wire.push(TAIL);

    let parsed = ShortU16Fixed::try_from_slice(&wire).unwrap();

    assert_eq!(parsed.values, [TWO_BYTES.0, ONE_BYTE.0]);
    assert_eq!(parsed.tail, TAIL);
    assert_eq!(borsh::to_vec(&parsed).unwrap(), wire);
}
