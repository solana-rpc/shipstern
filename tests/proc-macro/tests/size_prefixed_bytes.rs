use borsh::BorshDeserialize;
use prost::Message;
use shipstern_proc_macro::include_shipstern_parser;

include_shipstern_parser!("../idls/size_prefixed_bytes.json");

#[test]
fn preserves_the_full_protocol_cpi_payload() {
    let wire = hex::decode("071000000000000000b7cc0e45114cdf690100000000000000").unwrap();
    let path = shipstern_core::instruction::Path::new_single(0);
    let parsed = size_prefixed_bytes::resolve_instruction_default(&[], &wire, &path).unwrap();
    let size_prefixed_bytes::instruction::Instruction::ProtocolCpi { args, .. } =
        parsed.instruction
    else {
        panic!("expected ProtocolCpi")
    };
    assert_eq!(
        args.data,
        hex::decode("b7cc0e45114cdf690100000000000000").unwrap()
    );
    assert_eq!(borsh::to_vec(&args).unwrap(), wire[1..]);
    // Protobuf still contains field 1 as bytes, without the on-chain length prefix.
    let mut protobuf = vec![0x0a, 16];
    protobuf.extend_from_slice(&wire[9..]);
    assert_eq!(args.encode_to_vec(), protobuf);
    assert!(size_prefixed_bytes::PROTOBUF_SCHEMA.contains("bytes data = 1;"));
}

#[test]
fn respects_prefix_width_endian_and_the_following_field() {
    macro_rules! check {
        ($ty:ident, $prefix:expr) => {{
            let value = size_prefixed_bytes::$ty {
                data: vec![0xab, 0xcd],
                sentinel: 0xee,
            };
            let mut wire = $prefix.to_vec();
            wire.extend_from_slice(&[0xab, 0xcd, 0xee]);
            assert_eq!(borsh::to_vec(&value).unwrap(), wire);
            assert_eq!(
                size_prefixed_bytes::$ty::try_from_slice(&wire).unwrap(),
                value
            );
            for end in 0..wire.len() {
                assert!(size_prefixed_bytes::$ty::try_from_slice(&wire[..end]).is_err());
            }
        }};
    }
    check!(U8LeBytes, [2]);
    check!(U16LeBytes, 2_u16.to_le_bytes());
    check!(U16BeBytes, 2_u16.to_be_bytes());
    check!(U32LeBytes, 2_u32.to_le_bytes());
    check!(U32BeBytes, 2_u32.to_be_bytes());
    check!(U64LeBytes, 2_u64.to_le_bytes());
    check!(U64BeBytes, 2_u64.to_be_bytes());
    check!(U128LeBytes, 2_u128.to_le_bytes());
    check!(U128BeBytes, 2_u128.to_be_bytes());
    check!(ShortU16LeBytes, [2]);
}

#[test]
fn rejects_impossible_lengths_without_allocating_from_them() {
    use size_prefixed_bytes::{ShortU16LeBytes, U128LeBytes, U64LeBytes, U8LeBytes};
    assert!(U64LeBytes::try_from_slice(&u64::MAX.to_le_bytes()).is_err());
    assert!(U128LeBytes::try_from_slice(&u128::MAX.to_le_bytes()).is_err());
    let mut wire = 0_u64.to_le_bytes().to_vec();
    wire.push(0xee);
    assert!(U64LeBytes::try_from_slice(&wire).unwrap().data.is_empty());
    assert!(borsh::to_vec(&U8LeBytes {
        data: vec![0; 256],
        sentinel: 0
    })
    .is_err());
    for bad in [&[0x80][..], &[0x80, 0], &[0x80, 0x80, 4], &[
        0xff, 0xff, 0x83,
    ]] {
        assert!(ShortU16LeBytes::try_from_slice(bad).is_err());
    }
    for (length, prefix) in [
        (0, &[0][..]),
        (127, &[0x7f]),
        (128, &[0x80, 1]),
        (16_383, &[0xff, 0x7f]),
        (16_384, &[0x80, 0x80, 1]),
        (65_535, &[0xff, 0xff, 3]),
    ] {
        let value = ShortU16LeBytes {
            data: vec![7; length],
            sentinel: 0xee,
        };
        let wire = borsh::to_vec(&value).unwrap();
        assert_eq!(&wire[..prefix.len()], prefix);
        assert_eq!(ShortU16LeBytes::try_from_slice(&wire).unwrap(), value);
    }
    assert!(borsh::to_vec(&ShortU16LeBytes {
        data: vec![0; 65_536],
        sentinel: 0
    })
    .is_err());
}

#[test]
fn composes_with_option_and_array_encodings() {
    use size_prefixed_bytes::Containers;
    let value = Containers {
        optional: Some(vec![1]),
        wide_optional: Some(vec![2]),
        repeated: vec![vec![3], vec![]],
        fixed: vec![vec![4], vec![5]],
        sentinel: 0xee,
    };
    let bytes = |v: &[u8]| {
        let mut out = (v.len() as u64).to_le_bytes().to_vec();
        out.extend_from_slice(v);
        out
    };
    let mut wire = vec![1];
    wire.extend(bytes(&[1]));
    wire.extend([0, 1]);
    wire.extend(bytes(&[2]));
    wire.extend(2_u32.to_le_bytes());
    for v in [&[3][..], &[], &[4], &[5]] {
        wire.extend(bytes(v));
    }
    wire.push(0xee);
    assert_eq!(borsh::to_vec(&value).unwrap(), wire);
    assert_eq!(Containers::try_from_slice(&wire).unwrap(), value);
    let proto = value.encode_to_vec();
    assert_eq!(Containers::decode(proto.as_slice()).unwrap(), value);
    let empty = Containers {
        optional: None,
        wide_optional: None,
        repeated: vec![],
        fixed: vec![vec![], vec![]],
        sentinel: 0,
    };
    assert_eq!(
        Containers::try_from_slice(&borsh::to_vec(&empty).unwrap()).unwrap(),
        empty
    );
    let wrong_count = Containers {
        fixed: vec![],
        ..empty
    };
    assert!(borsh::to_vec(&wrong_count).is_err());
}

#[test]
fn honors_the_prefix_on_strings_too() {
    use size_prefixed_bytes::{ShortU16LeString, U64BeString, U8LeString};
    macro_rules! check {
        ($ty:ident, $prefix:expr) => {{
            let value = $ty {
                data: "hé".to_string(),
                sentinel: 0xee,
            };
            let mut wire = $prefix.to_vec();
            wire.extend_from_slice(&[0x68, 0xc3, 0xa9, 0xee]);
            assert_eq!(borsh::to_vec(&value).unwrap(), wire);
            assert_eq!($ty::try_from_slice(&wire).unwrap(), value);

            for end in 0..wire.len() {
                assert!($ty::try_from_slice(&wire[..end]).is_err());
            }

            // The payload is a string, so invalid UTF-8 is rejected, not lossily decoded.
            let mut invalid = wire.clone();
            invalid[$prefix.len()] = 0xff;
            assert!($ty::try_from_slice(&invalid).is_err());
        }};
    }
    check!(U8LeString, [3]);
    check!(U64BeString, 3_u64.to_be_bytes());
    check!(ShortU16LeString, [3]);

    assert!(size_prefixed_bytes::PROTOBUF_SCHEMA.contains("string data = 1;"));
}

#[test]
fn resolves_the_helpers_from_the_instruction_module() {
    let bytes = |v: &[u8]| {
        let mut out = (v.len() as u64).to_le_bytes().to_vec();
        out.extend_from_slice(v);
        out
    };

    let mut wire = vec![0x08, 1];
    wire.extend(bytes(&[1, 2]));
    wire.extend(bytes(&[3]));
    wire.extend(bytes(&[]));
    wire.extend(bytes(b"label"));

    let path = shipstern_core::instruction::Path::new_single(0);
    let parsed = size_prefixed_bytes::resolve_instruction_default(&[], &wire, &path).unwrap();
    let size_prefixed_bytes::instruction::Instruction::ContainerCpi { args, .. } =
        parsed.instruction
    else {
        panic!("expected ContainerCpi")
    };

    assert_eq!(args.optional, Some(vec![1, 2]));
    assert_eq!(args.fixed, vec![vec![3], vec![]]);
    assert_eq!(args.label, "label");
    assert_eq!(borsh::to_vec(&args).unwrap(), wire[1..]);
}
