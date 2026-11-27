//!
//! A program named after a Rust keyword gets a module of the same snake_case name,
//! which must be a raw identifier.
//!

use shipstern_proc_macro::include_shipstern_parser;

include_shipstern_parser!("../idls/keyword_program_name.json");

use r#move::{r#type, Type};

#[test]
fn program_named_after_a_keyword_round_trips() {
    // Tag 1 (`rate`), then `value` = 7.
    let data = [1, 7, 0, 0, 0];

    let ty: Type = borsh::from_slice(&data).expect("tag 1 is rate");
    let r#type::Kind::Rate(rate) = &ty.kind else {
        panic!("expected rate, got {ty:?}");
    };

    assert_eq!(rate.value, 7);
}
