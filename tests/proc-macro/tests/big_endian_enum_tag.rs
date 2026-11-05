use shipstern_proc_macro::include_shipstern_parser;

include_shipstern_parser!("../idls/big_endian_enum_tag.json");

use big_endian_enum_tag::{mode::Kind, Mode};

#[test]
fn big_endian_enum_tag_round_trips() {
    // Tag 1 (`rate`) as a big-endian u16, then `value` = 7.
    let data = [0x00, 0x01, 7, 0, 0, 0];

    let mode: Mode = borsh::from_slice(&data).expect("tag 1 is rate");
    let Kind::Rate(rate) = &mode.kind else {
        panic!("expected rate, got {mode:?}");
    };

    assert_eq!(rate.value, 7);
    assert_eq!(borsh::to_vec(&mode).expect("encodes"), data);
}
