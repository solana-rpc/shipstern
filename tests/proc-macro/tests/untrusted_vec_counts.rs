// Regression: a `u32` list count read from the input must not size an allocation, and the
// serializers must not write a shape that decodes to other values.

use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
};

use borsh::BorshDeserialize;
use shipstern_proc_macro::include_shipstern_parser;

include_shipstern_parser!("../idls/untrusted_vec_counts.json");

use untrusted_vec_counts::{F32List, F64List, FixedBytesList, FixedShapes};

/// Records, per thread, the largest allocation request and the number of reallocs.
struct RequestTracker;

thread_local! {
    static LARGEST_REQUEST: Cell<usize> = const { Cell::new(0) };
    static REALLOCS: Cell<usize> = const { Cell::new(0) };
}

fn record(size: usize) {
    let _ = LARGEST_REQUEST.try_with(|largest| largest.set(largest.get().max(size)));
}

unsafe impl GlobalAlloc for RequestTracker {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        record(layout.size());

        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        record(layout.size());

        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        record(new_size);
        let _ = REALLOCS.try_with(|reallocs| reallocs.set(reallocs.get() + 1));

        unsafe { System.realloc(ptr, layout, new_size) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: RequestTracker = RequestTracker;

/// What one decode asked of the allocator on this thread.
#[derive(Debug)]
struct Requests {
    largest: usize,
    reallocs: usize,
}

fn decode<T: BorshDeserialize>(wire: &[u8]) -> (borsh::io::Result<T>, Requests) {
    LARGEST_REQUEST.with(|largest| largest.set(0));
    REALLOCS.with(|reallocs| reallocs.set(0));

    let result = T::try_from_slice(wire);

    let requests = Requests {
        largest: LARGEST_REQUEST.with(Cell::get),
        reallocs: REALLOCS.with(Cell::get),
    };

    (result, requests)
}

/// A count of `u32::MAX` with no items must fail without a request above the 4 KiB hint.
fn assert_rejects_count_without_items<T: BorshDeserialize + std::fmt::Debug>() {
    let (result, requests) = decode::<T>(&u32::MAX.to_le_bytes());

    assert!(result.is_err(), "decoded {result:?}");
    assert!(requests.largest <= 4096, "{requests:?}");
}

#[test]
fn fixed_bytes_list_does_not_allocate_from_the_count() {
    assert_rejects_count_without_items::<FixedBytesList>();
}

#[test]
fn f32_list_does_not_allocate_from_the_count() { assert_rejects_count_without_items::<F32List>(); }

#[test]
fn f64_list_does_not_allocate_from_the_count() { assert_rejects_count_without_items::<F64List>(); }

/// A list that fits the 4 KiB hint is allocated once, with no regrowth.
#[test]
fn typical_lists_decode_without_a_realloc() {
    {
        let value = FixedBytesList {
            items: vec![vec![7; 32]; 24],
            sentinel: 0xee,
        };
        let (result, requests) = decode::<FixedBytesList>(&borsh::to_vec(&value).unwrap());

        assert_eq!(result.unwrap(), value);
        assert_eq!(requests.reallocs, 0, "{requests:?}");
    }

    {
        let value = F64List {
            items: vec![0.5; 24],
            sentinel: 0xee,
        };
        let (result, requests) = decode::<F64List>(&borsh::to_vec(&value).unwrap());

        assert_eq!(result.unwrap(), value);
        assert_eq!(requests.reallocs, 0, "{requests:?}");
    }
}

#[test]
fn round_trips_lists_and_the_following_field() {
    {
        let value = FixedBytesList {
            items: vec![vec![1; 32], vec![2; 32]],
            sentinel: 0xee,
        };
        let wire = borsh::to_vec(&value).unwrap();

        assert_eq!(wire.len(), 4 + 2 * 32 + 1);
        assert_eq!(FixedBytesList::try_from_slice(&wire).unwrap(), value);
    }

    {
        let value = F32List {
            items: vec![1.5, f32::INFINITY],
            sentinel: 0xee,
        };
        let wire = borsh::to_vec(&value).unwrap();

        assert_eq!(wire.len(), 4 + 2 * 4 + 1);
        assert_eq!(F32List::try_from_slice(&wire).unwrap(), value);
    }

    {
        let value = F64List {
            items: vec![-2.25, f64::MAX],
            sentinel: 0xee,
        };
        let wire = borsh::to_vec(&value).unwrap();

        assert_eq!(wire.len(), 4 + 2 * 8 + 1);
        assert_eq!(F64List::try_from_slice(&wire).unwrap(), value);
    }
}

fn shapes() -> FixedShapes {
    FixedShapes {
        key: vec![1; 4],
        maybe_key: Some(vec![2; 4]),
        bytes_pair: vec![vec![3; 4], vec![4; 4]],
        number_pair: vec![5, 6],
        float_pair: vec![7.5, f32::NAN],
    }
}

#[test]
fn round_trips_fixed_shapes() {
    let value = shapes();
    let wire = borsh::to_vec(&value).unwrap();

    assert_eq!(wire.len(), 4 + 1 + 4 + 2 * 4 + 2 * 2 + 2 * 4);

    let decoded = FixedShapes::try_from_slice(&wire).unwrap();

    assert_eq!(borsh::to_vec(&decoded).unwrap(), wire);
}

/// A named change that gives a valid `FixedShapes` the wrong shape.
type ShapeChange = (&'static str, fn(&mut FixedShapes));

/// Each shape below would write bytes that decode to other values.
#[test]
fn serializers_reject_values_that_do_not_match_the_declared_shape() {
    let cases: [ShapeChange; 6] = [
        ("short key", |v| v.key.truncate(3)),
        ("long option item", |v| {
            v.maybe_key.as_mut().unwrap().push(0)
        }),
        ("short fixed-bytes item", |v| v.bytes_pair[1].truncate(3)),
        ("extra fixed-bytes item", |v| v.bytes_pair.push(vec![0; 4])),
        ("missing number item", |v| v.number_pair.truncate(1)),
        ("extra float item", |v| v.float_pair.push(0.0)),
    ];

    for (name, change) in cases {
        let mut value = shapes();
        change(&mut value);

        let error = borsh::to_vec(&value).expect_err(name);

        assert_eq!(error.kind(), borsh::io::ErrorKind::InvalidInput, "{name}");
    }

    let list = FixedBytesList {
        items: vec![vec![1; 31], vec![2; 33]],
        sentinel: 0xee,
    };
    let error = borsh::to_vec(&list).expect_err("uneven list items");

    assert_eq!(error.kind(), borsh::io::ErrorKind::InvalidInput);
}
