//!
//! Number fields written big-endian (`endian: be`).
//!
//! The wire bytes are built by hand. Read as little-endian, a multi-byte value
//! decodes byte-swapped without any error, so each assertion pins the value.
//!

use borsh::BorshDeserialize;
use prost::Message;
use shipstern_proc_macro::include_shipstern_parser;

include_shipstern_parser!("../idls/big_endian_numbers.json");

use big_endian_numbers::{
    BigEndianBigPrefixOptional, BigEndianFixed, BigEndianFixedOptional, BigEndianList,
    BigEndianNumbers, BigEndianOptional, LittleEndianControl,
};

const TAIL: u8 = 0xee;

fn numbers_wire() -> Vec<u8> {
    let mut wire = vec![0x7a];
    wire.extend_from_slice(&0x1234_u16.to_be_bytes());
    wire.extend_from_slice(&0x1234_5678_u32.to_be_bytes());
    wire.extend_from_slice(&0x0102_0304_0506_0708_u64.to_be_bytes());
    wire.extend_from_slice(&0x0102_0304_0506_0708_090a_0b0c_0d0e_0f10_u128.to_be_bytes());
    wire.extend_from_slice(&(-2_i16).to_be_bytes());
    wire.extend_from_slice(&(-0x1234_5678_i32).to_be_bytes());
    wire.extend_from_slice(&(-0x0102_0304_0506_0708_i64).to_be_bytes());
    wire.extend_from_slice(&(-0x0102_0304_0506_0708_090a_0b0c_0d0e_0f10_i128).to_be_bytes());
    wire.extend_from_slice(&1.5_f32.to_be_bytes());
    wire.extend_from_slice(&(-2.25_f64).to_be_bytes());
    wire.push(TAIL);
    wire
}

#[test]
fn scalar_fields_decode_big_endian() {
    let wire = numbers_wire();

    let parsed = BigEndianNumbers::try_from_slice(&wire).expect("numbers should decode");

    assert_eq!(parsed.a_u8, 0x7a);
    assert_eq!(parsed.a_u16, 0x1234);
    assert_eq!(parsed.a_u32, 0x1234_5678);
    assert_eq!(parsed.a_u64, 0x0102_0304_0506_0708);
    assert_eq!(parsed.a_u128, 0x0102_0304_0506_0708_090a_0b0c_0d0e_0f10);
    assert_eq!(parsed.a_i16, -2);
    assert_eq!(parsed.a_i32, -0x1234_5678);
    assert_eq!(parsed.a_i64, -0x0102_0304_0506_0708);
    assert_eq!(parsed.a_i128, -0x0102_0304_0506_0708_090a_0b0c_0d0e_0f10);
    assert_eq!(parsed.a_f32, 1.5);
    assert_eq!(parsed.a_f64, -2.25);
    assert_eq!(parsed.tail, TAIL);
    assert_eq!(borsh::to_vec(&parsed).unwrap(), wire);
}

#[test]
fn big_endian_floats_keep_nan() {
    let mut wire = numbers_wire();
    let f32_at = wire.len() - 1 - 8 - 4;
    wire[f32_at..f32_at + 4].copy_from_slice(&f32::NAN.to_be_bytes());

    let parsed = BigEndianNumbers::try_from_slice(&wire).unwrap();

    assert!(parsed.a_f32.is_nan());
}

#[test]
fn proto_carries_the_decoded_value() {
    let parsed = BigEndianNumbers::try_from_slice(&numbers_wire()).unwrap();

    assert_eq!(
        BigEndianNumbers::decode(parsed.encode_to_vec().as_slice()).unwrap(),
        parsed
    );
    assert!(big_endian_numbers::PROTOBUF_SCHEMA.contains("uint32 a_u32 = 3;"));
}

#[test]
fn little_endian_fields_are_unchanged() {
    let mut wire = 0x1234_5678_u32.to_le_bytes().to_vec();
    wire.push(TAIL);

    let parsed = LittleEndianControl::try_from_slice(&wire).unwrap();

    assert_eq!(parsed.a, 0x1234_5678);
}

#[test]
fn optional_field_with_a_u8_prefix() {
    let mut some = vec![1];
    some.extend_from_slice(&0x0a0b_0c0d_u32.to_be_bytes());
    some.push(TAIL);

    let parsed = BigEndianOptional::try_from_slice(&some).unwrap();

    assert_eq!(parsed.value, Some(0x0a0b_0c0d));
    assert_eq!(parsed.tail, TAIL);
    assert_eq!(borsh::to_vec(&parsed).unwrap(), some);

    let parsed = BigEndianOptional::try_from_slice(&[0, TAIL]).unwrap();

    assert_eq!(parsed.value, None);
    assert_eq!(parsed.tail, TAIL);
}

#[test]
fn optional_field_with_a_big_endian_prefix() {
    let mut some = 1_u16.to_be_bytes().to_vec();
    some.extend_from_slice(&0x0a0b_u16.to_be_bytes());
    some.push(TAIL);

    let parsed = BigEndianBigPrefixOptional::try_from_slice(&some).unwrap();

    assert_eq!(parsed.value, Some(0x0a0b));
    assert_eq!(parsed.tail, TAIL);
    assert_eq!(borsh::to_vec(&parsed).unwrap(), some);

    let parsed = BigEndianBigPrefixOptional::try_from_slice(&[0, 0, TAIL]).unwrap();

    assert_eq!(parsed.value, None);
    assert_eq!(parsed.tail, TAIL);
}

#[test]
fn fixed_option_skips_the_padding_when_none() {
    let mut none = vec![0, 0, 0, 0, 0];
    none.push(TAIL);

    let parsed = BigEndianFixedOptional::try_from_slice(&none).unwrap();

    assert_eq!(parsed.value, None);
    assert_eq!(parsed.tail, TAIL);

    let mut some = vec![1];
    some.extend_from_slice(&0x0a0b_0c0d_u32.to_be_bytes());
    some.push(TAIL);

    let parsed = BigEndianFixedOptional::try_from_slice(&some).unwrap();

    assert_eq!(parsed.value, Some(0x0a0b_0c0d));
    assert_eq!(borsh::to_vec(&parsed).unwrap(), some);
}

#[test]
fn repeated_field_reads_each_item_big_endian() {
    let mut wire = 2_u32.to_le_bytes().to_vec();
    wire.extend_from_slice(&1_u32.to_be_bytes());
    wire.extend_from_slice(&0x0a0b_0c0d_u32.to_be_bytes());
    wire.push(TAIL);

    let parsed = BigEndianList::try_from_slice(&wire).unwrap();

    assert_eq!(parsed.values, [1, 0x0a0b_0c0d]);
    assert_eq!(parsed.tail, TAIL);
    assert_eq!(borsh::to_vec(&parsed).unwrap(), wire);
    assert!(BigEndianList::try_from_slice(&u32::MAX.to_le_bytes()).is_err());
}

#[test]
fn fixed_array_field_reads_n_items_big_endian() {
    let mut wire = 0x0102_u16.to_be_bytes().to_vec();
    wire.extend_from_slice(&0x0304_u16.to_be_bytes());
    wire.push(TAIL);

    let parsed = BigEndianFixed::try_from_slice(&wire).unwrap();

    assert_eq!(parsed.values, [0x0102, 0x0304]);
    assert_eq!(parsed.tail, TAIL);
    assert_eq!(borsh::to_vec(&parsed).unwrap(), wire);
}
