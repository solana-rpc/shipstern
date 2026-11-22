//!
//! Fixed-size byte fields far larger than a test or worker thread's stack.
//!
//! Codama allows a fixed type up to 10 MiB. The decoders must buffer such a field on the
//! heap, or a multi-MiB field aborts the process with a stack overflow.
//!

use borsh::BorshDeserialize;
use shipstern_proc_macro::include_shipstern_parser;

include_shipstern_parser!("../idls/large_fixed_bytes.json");

use large_fixed_bytes::{BigFixedArray, BigFixedOption, BigList, BigOptional, BigPlain};

const SIZE: usize = 8 * 1024 * 1024;
const TAIL: u8 = 0xee;

fn blob(fill: u8) -> Vec<u8> { vec![fill; SIZE] }

#[test]
fn plain_field() {
    let mut wire = blob(7);
    wire.push(TAIL);

    let parsed = BigPlain::try_from_slice(&wire).unwrap();

    assert_eq!(parsed.data, blob(7));
    assert_eq!(parsed.tail, TAIL);
}

#[test]
fn optional_field() {
    let mut wire = vec![1];
    wire.extend(blob(7));
    wire.push(TAIL);

    let parsed = BigOptional::try_from_slice(&wire).unwrap();

    assert_eq!(parsed.data, Some(blob(7)));
    assert_eq!(parsed.tail, TAIL);
}

#[test]
fn fixed_option_reads_some_and_pads_none() {
    let mut some = 1_u32.to_le_bytes().to_vec();
    some.extend(blob(7));
    some.push(TAIL);

    assert_eq!(
        BigFixedOption::try_from_slice(&some).unwrap().data,
        Some(blob(7))
    );

    let mut none = vec![0; 4 + SIZE];
    none.push(TAIL);

    let parsed = BigFixedOption::try_from_slice(&none).unwrap();

    assert_eq!(parsed.data, None);
    assert_eq!(parsed.tail, TAIL);
}

#[test]
fn repeated_field() {
    let mut wire = 1_u32.to_le_bytes().to_vec();
    wire.extend(blob(7));
    wire.push(TAIL);

    let parsed = BigList::try_from_slice(&wire).unwrap();

    assert_eq!(parsed.data, vec![blob(7)]);
    assert_eq!(parsed.tail, TAIL);
}

#[test]
fn fixed_array_field() {
    let mut wire = blob(1);
    wire.extend(blob(2));
    wire.push(TAIL);

    let parsed = BigFixedArray::try_from_slice(&wire).unwrap();

    assert_eq!(parsed.data, vec![blob(1), blob(2)]);
    assert_eq!(parsed.tail, TAIL);
}
