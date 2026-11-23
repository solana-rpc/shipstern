use prost::Message;
use shipstern_core::instruction::Path;
use shipstern_proc_macro::include_shipstern_parser;

include_shipstern_parser!("../idls/map_and_set.json");
include_shipstern_parser!("../idls/constant_bytes_account.json");
include_shipstern_parser!("../idls/big_endian_enum_tag.json");

// Fields 1000 (varint) and 1001 (length-delimited), as a newer schema might add.
const UNKNOWN_VARINT: [u8; 3] = [0xc0, 0x3e, 0x01];
const UNKNOWN_BYTES: [u8; 5] = [0xca, 0x3e, 0x02, 0xaa, 0xbb];

fn put_instructions() -> map_and_set::Instructions {
    let mut wire = vec![1];
    wire.extend(0_u32.to_le_bytes());
    wire.extend(0_u32.to_le_bytes());
    wire.extend(42_u64.to_le_bytes());

    map_and_set::resolve_instruction_default(&[], &wire, &Path::new_single(0)).unwrap()
}

#[test]
fn instructions_skip_unknown_fields() {
    for foreign in [&UNKNOWN_VARINT[..], &UNKNOWN_BYTES[..]] {
        let expected = put_instructions();
        let mut decoded = put_instructions();

        decoded.merge(foreign).expect("unknown field is skipped");

        assert_eq!(decoded, expected);
    }
}

#[test]
fn instructions_still_merge_known_fields() {
    let value = put_instructions();
    let mut bytes = value.encode_to_vec();
    bytes.extend(UNKNOWN_VARINT);

    let mut decoded = put_instructions();
    decoded.merge(bytes.as_slice()).unwrap();

    assert_eq!(decoded, value);
}

#[test]
fn account_wrapper_skips_unknown_fields() {
    let mut data = vec![1];
    data.extend_from_slice(&42_u64.to_le_bytes());

    let expected = constant_bytes_account::ConstantBytesAccountAccount::try_unpack(&data).unwrap();
    let mut decoded = expected.clone();

    decoded
        .merge(&UNKNOWN_VARINT[..])
        .expect("unknown field is skipped");

    assert_eq!(decoded, expected);
}

#[test]
fn enum_wrapper_skips_unknown_fields() {
    let decoded = big_endian_enum_tag::Mode::decode(&UNKNOWN_VARINT[..]).expect("skipped");

    assert_eq!(decoded, big_endian_enum_tag::Mode::default());
}

#[test]
fn enum_wrapper_still_round_trips() {
    let value = big_endian_enum_tag::Mode::default();

    assert_eq!(
        big_endian_enum_tag::Mode::decode(value.encode_to_vec().as_slice()).unwrap(),
        value
    );
}
