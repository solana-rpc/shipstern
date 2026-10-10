// Regression: inline Codama structs (e.g. Pinocchio fixed strings) must become
// protobuf messages, including inside an Option.

use shipstern_proc_macro::include_shipstern_parser;
use shipstern_test_utils::check_protobuf_format;

include_shipstern_parser!("../idls/inline_struct.json");

#[test]
fn check_protobuf_schema() {
    check_protobuf_format(inline_struct::PROTOBUF_SCHEMA);

    assert!(inline_struct::PROTOBUF_SCHEMA.contains("message SetMetadataArgsMetadataName"));
    assert!(inline_struct::PROTOBUF_SCHEMA.contains("message SetMetadataArgsOptionalMetadata"));
    assert!(inline_struct::PROTOBUF_SCHEMA.contains("message SetMetadataArgsArrayMetadata"));
    assert!(inline_struct::PROTOBUF_SCHEMA.contains("message SetMetadataArgsTupleMetadataTuple"));
    assert!(inline_struct::PROTOBUF_SCHEMA
        .contains("message SetMetadataArgsTupleMetadataTupleItem1Inner"));
    assert!(inline_struct::PROTOBUF_SCHEMA
        .contains("message SetMetadataArgsOptionalArrayMetadataOption"));
    assert!(inline_struct::PROTOBUF_SCHEMA.contains("message SetMetadataArgsFooOption"));
    assert!(inline_struct::PROTOBUF_SCHEMA.contains("message SetMetadataArgsFooOption2"));
    assert!(inline_struct::PROTOBUF_SCHEMA.contains("message DefinedOptional"));
    assert!(inline_struct::PROTOBUF_SCHEMA.contains("message DefinedTupleArrayItemTuple"));
    assert!(inline_struct::PROTOBUF_SCHEMA.contains("message DefinedNestedOption"));
    assert!(inline_struct::PROTOBUF_SCHEMA.contains("message FooOption"));
    assert!(inline_struct::PROTOBUF_SCHEMA.contains("message FooOption2"));
    assert!(inline_struct::PROTOBUF_SCHEMA.contains("message FirstAliasOption"));
}

#[test]
fn wrapped_defined_types_preserve_their_labels() {
    let args = inline_struct::instruction::UseDefinedAliasesArgs::default();

    assert!(args.defined_optional.is_none());
    assert!(args.defined_tuple_array.is_empty());
    assert!(args.defined_nested.is_none());
    assert!(args.first_alias.is_none());
}

#[test]
fn generated_helpers_do_not_shadow_defined_types_or_forward_aliases() {
    let nested_alias = inline_struct::FirstAliasOption { value: Some(7) };
    let canonical_type = inline_struct::FooOption { code: 1 };
    let generated_helper = inline_struct::FooOption2 { value: vec![2, 3] };

    assert_eq!(
        (
            nested_alias.value,
            canonical_type.code,
            generated_helper.value
        ),
        (Some(7), 1, vec![2, 3]),
    );
}

#[test]
fn inline_structs_are_generated_as_messages() {
    let args = inline_struct::instruction::SetMetadataArgs::default();

    assert_eq!(args.metadata_name.len, 0);
    assert!(args.optional_metadata.is_none());
    assert!(args.array_metadata.is_empty());
    assert_eq!(args.tuple_metadata.item_0.code, 0);
    assert!(args.optional_array_metadata.is_none());
    assert!(args.foo.is_none());
    assert_eq!(args.foo_option.code, 0);
}

#[test]
fn nested_inline_structs_round_trip_borsh() {
    use borsh::BorshDeserialize;

    let args = inline_struct::instruction::SetMetadataArgs {
        metadata_name: inline_struct::SetMetadataArgsMetadataName {
            len: 3,
            value: vec![b'a'; 25],
        },
        optional_metadata: Some(inline_struct::SetMetadataArgsOptionalMetadata {
            len: 2,
            value: vec![b'b'; 5],
        }),
        array_metadata: vec![
            inline_struct::SetMetadataArgsArrayMetadata {
                code: 1,
                value: vec![b'c'; 3],
            },
            inline_struct::SetMetadataArgsArrayMetadata {
                code: 2,
                value: vec![b'd'; 3],
            },
        ],
        tuple_metadata: inline_struct::SetMetadataArgsTupleMetadataTuple {
            item_0: inline_struct::SetMetadataArgsTupleMetadataTupleItem0 { code: 7 },
            item_1: vec![
                inline_struct::SetMetadataArgsTupleMetadataTupleItem1Inner {
                    items: Some(inline_struct::SetMetadataArgsTupleMetadataTupleItem1Item {
                        value: 8,
                    }),
                },
                inline_struct::SetMetadataArgsTupleMetadataTupleItem1Inner { items: None },
            ],
        },
        optional_array_metadata: Some(inline_struct::SetMetadataArgsOptionalArrayMetadataOption {
            value: vec![
                inline_struct::SetMetadataArgsOptionalArrayMetadataValue { value: 9 },
                inline_struct::SetMetadataArgsOptionalArrayMetadataValue { value: 10 },
            ],
        }),
        foo: None,
        foo_option: inline_struct::SetMetadataArgsFooOption2 { code: 11 },
    };

    let encoded = borsh::to_vec(&args).unwrap();
    let decoded = inline_struct::instruction::SetMetadataArgs::try_from_slice(&encoded).unwrap();

    assert_eq!(decoded, args);
}

#[test]
fn bare_tuple_deserializes_exact_borsh_wire() {
    use borsh::BorshDeserialize;

    // A fixed `None` option includes a tag plus zero padding matching its
    // six-byte item. The tuple still begins with `code = 7`, not an option tag.
    let encoded = [
        &[0u8][..],
        &[0u8; 25][..],
        &[0u8][..],
        &[0u8; 6][..],
        &[1, b'a', b'a', b'a', 2, b'b', b'b', b'b'][..],
        &[7, 1, 8, 0, 0, 0, 0, 42][..],
    ]
    .concat();
    let decoded = inline_struct::instruction::SetMetadataArgs::try_from_slice(&encoded).unwrap();

    let expected = inline_struct::instruction::SetMetadataArgs {
        metadata_name: inline_struct::SetMetadataArgsMetadataName {
            len: 0,
            value: vec![0; 25],
        },
        optional_metadata: None,
        array_metadata: vec![
            inline_struct::SetMetadataArgsArrayMetadata {
                code: 1,
                value: vec![b'a'; 3],
            },
            inline_struct::SetMetadataArgsArrayMetadata {
                code: 2,
                value: vec![b'b'; 3],
            },
        ],
        tuple_metadata: inline_struct::SetMetadataArgsTupleMetadataTuple {
            item_0: inline_struct::SetMetadataArgsTupleMetadataTupleItem0 { code: 7 },
            item_1: vec![
                inline_struct::SetMetadataArgsTupleMetadataTupleItem1Inner {
                    items: Some(inline_struct::SetMetadataArgsTupleMetadataTupleItem1Item {
                        value: 8,
                    }),
                },
                inline_struct::SetMetadataArgsTupleMetadataTupleItem1Inner { items: None },
            ],
        },
        optional_array_metadata: None,
        foo: None,
        foo_option: inline_struct::SetMetadataArgsFooOption2 { code: 42 },
    };
    let reencoded = borsh::to_vec(&expected).unwrap();

    assert_eq!((decoded, reencoded), (expected, encoded));
}

/// SPL Token keeps the old key when it sets a `COption` to `None`, so the bytes
/// after a `None` tag must not have to be zero.
#[test]
fn fixed_none_skips_leftover_padding() {
    use borsh::BorshDeserialize;

    let wire = |padding: [u8; 6]| {
        [
            &[0u8][..],
            &[0u8; 25][..],
            &[0u8][..],
            &padding[..],
            &[1, b'a', b'a', b'a', 2, b'b', b'b', b'b'][..],
            &[7, 1, 8, 0, 0, 0, 0, 42][..],
        ]
        .concat()
    };

    let clean = inline_struct::instruction::SetMetadataArgs::try_from_slice(&wire([0; 6])).unwrap();
    let leftover = inline_struct::instruction::SetMetadataArgs::try_from_slice(&wire([9; 6]))
        .expect("None with leftover bytes should decode");

    assert_eq!(leftover, clean);
}
