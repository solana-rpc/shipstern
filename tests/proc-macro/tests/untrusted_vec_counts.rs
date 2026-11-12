// Regression: a `u32` element count read from the input must not size an allocation.
// Each case declares u32::MAX elements and supplies none, so decoding must fail without a large allocation.

use borsh::BorshDeserialize;
use shipstern_proc_macro::include_shipstern_parser;

include_shipstern_parser!("../idls/untrusted_vec_counts.json");

#[test]
fn rejects_counts_larger_than_the_input_without_allocating_from_them() {
    let wire = u32::MAX.to_le_bytes();

    assert!(untrusted_vec_counts::FixedBytesList::try_from_slice(&wire).is_err());
    assert!(untrusted_vec_counts::F32List::try_from_slice(&wire).is_err());
    assert!(untrusted_vec_counts::F64List::try_from_slice(&wire).is_err());
}

#[test]
fn round_trips_lists_and_the_following_field() {
    {
        let value = untrusted_vec_counts::FixedBytesList {
            items: vec![vec![1; 32], vec![2; 32]],
            sentinel: 0xee,
        };
        let wire = borsh::to_vec(&value).unwrap();

        assert_eq!(wire.len(), 4 + 2 * 32 + 1);
        assert_eq!(
            untrusted_vec_counts::FixedBytesList::try_from_slice(&wire).unwrap(),
            value
        );
    }

    {
        let value = untrusted_vec_counts::F32List {
            items: vec![1.5, f32::INFINITY],
            sentinel: 0xee,
        };
        let wire = borsh::to_vec(&value).unwrap();

        assert_eq!(wire.len(), 4 + 2 * 4 + 1);
        assert_eq!(
            untrusted_vec_counts::F32List::try_from_slice(&wire).unwrap(),
            value
        );
    }

    {
        let value = untrusted_vec_counts::F64List {
            items: vec![-2.25, f64::MAX],
            sentinel: 0xee,
        };
        let wire = borsh::to_vec(&value).unwrap();

        assert_eq!(wire.len(), 4 + 2 * 8 + 1);
        assert_eq!(
            untrusted_vec_counts::F64List::try_from_slice(&wire).unwrap(),
            value
        );
    }
}
