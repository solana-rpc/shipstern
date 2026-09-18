//! Borsh field codecs for byte strings whose Codama length prefix is not native u32 LE.
//! The public Rust field stays `Vec<u8>`/`String` and the protobuf field stays
//! `bytes`/`string`; only the wire codec changes.
//!
//! Like every other non-native borsh encoding here, an affected field carries only
//! `#[borsh(deserialize_with = ..., serialize_with = ...)]` pointing at a generic helper
//! emitted once per program module by [`helpers`].
use codama_nodes::{Endianness, NumberFormat};
use proc_macro2::{Span, TokenStream};
use quote::quote;
use syn::LitStr;

use crate::intermediate_representation::{
    FieldIr, FieldTypeIr, LabelIr, OptionPrefixIr, ScalarIr, SchemaIr,
};

/// The Codama length prefix of a size-prefixed field, `None` for every other field type.
fn prefix_of(field_type: &FieldTypeIr) -> Option<(NumberFormat, Endianness)> {
    match field_type {
        FieldTypeIr::Scalar(
            ScalarIr::SizePrefixedBytes { format, endian }
            | ScalarIr::SizePrefixedString { format, endian },
        ) => Some((*format, *endian)),
        _ => None,
    }
}

///
/// Borsh attrs for a size-prefixed field. Empty for every other field type.
///
/// Example output:
///
/// ```rust, ignore
/// #[borsh(
///     deserialize_with = "borsh_deserialize_size_prefixed::<ShipsternNumberSizePrefix<8, false>, _, _>",
///     serialize_with = "borsh_serialize_size_prefixed::<ShipsternNumberSizePrefix<8, false>, _, _>"
/// )]
/// ```
///
pub fn attrs(field: &FieldIr, path_prefix: &str) -> TokenStream {
    let Some((format, endian)) = prefix_of(&field.field_type) else {
        return quote! {};
    };

    let prefix = prefix_type(format, endian, path_prefix, &field.name);

    let (deserialize, serialize) = match &field.label {
        LabelIr::Singular => (
            format!("{path_prefix}borsh_deserialize_size_prefixed::<{prefix}, _, _>"),
            format!("{path_prefix}borsh_serialize_size_prefixed::<{prefix}, _, _>"),
        ),

        LabelIr::Optional(encoding) => {
            // A variable-size payload can never be the item of a fixed Codama option, so there
            // is no `None` padding to write. `build_option_encoding` already rejects that
            // pairing; panic rather than debug_assert, because a release-profile proc-macro
            // would otherwise silently emit a codec that skips the padding.
            assert!(
                encoding.none_padding.is_none(),
                "size-prefixed field `{}` cannot be the item of a fixed Codama option",
                field.name
            );

            match &encoding.prefix {
                OptionPrefixIr::FixedWidth {
                    byte_len,
                    one_value,
                    big_endian,
                } => {
                    let args = format!("{prefix}, _, {byte_len}, {one_value}, {big_endian}, _");

                    (
                        format!("{path_prefix}borsh_deserialize_opt_size_prefixed::<{args}>"),
                        format!("{path_prefix}borsh_serialize_opt_size_prefixed::<{args}>"),
                    )
                },
                OptionPrefixIr::ShortU16 => (
                    format!(
                        "{path_prefix}borsh_deserialize_short_u16_opt_size_prefixed::<{prefix}, \
                         _, _>"
                    ),
                    format!(
                        "{path_prefix}borsh_serialize_short_u16_opt_size_prefixed::<{prefix}, _, \
                         _>"
                    ),
                ),
            }
        },

        LabelIr::Repeated => (
            format!("{path_prefix}borsh_deserialize_vec_size_prefixed::<{prefix}, _, _>"),
            format!("{path_prefix}borsh_serialize_vec_size_prefixed::<{prefix}, _, _>"),
        ),

        LabelIr::FixedArray(n) => (
            format!(
                "{path_prefix}borsh_deserialize_fixed_array_size_prefixed::<{prefix}, _, {n}, _>"
            ),
            format!(
                "{path_prefix}borsh_serialize_fixed_array_size_prefixed::<{prefix}, _, {n}, _>"
            ),
        ),
    };

    let deserialize = LitStr::new(&deserialize, Span::call_site());
    let serialize = LitStr::new(&serialize, Span::call_site());

    quote! {
        #[borsh(
            deserialize_with = #deserialize,
            serialize_with = #serialize
        )]
    }
}

/// The marker type implementing `ShipsternSizePrefix` for this Codama prefix.
fn prefix_type(
    format: NumberFormat,
    endian: Endianness,
    path_prefix: &str,
    field_name: &str,
) -> String {
    if format == NumberFormat::ShortU16 {
        assert!(
            endian == Endianness::Le,
            "field `{field_name}`: a shortU16 byte length prefix has no endianness; drop `endian: \
             be`"
        );

        return format!("{path_prefix}ShipsternShortU16SizePrefix");
    }

    let byte_len = match format {
        NumberFormat::U8 => 1,
        NumberFormat::U16 => 2,
        NumberFormat::U32 => 4,
        NumberFormat::U64 => 8,
        NumberFormat::U128 => 16,
        other => panic!(
            "field `{field_name}`: unsupported byte length prefix: {other:?}; expected an \
             unsigned integer"
        ),
    };

    let big_endian = endian == Endianness::Be;

    format!("{path_prefix}ShipsternNumberSizePrefix<{byte_len}, {big_endian}>")
}

/// The generic codecs the field attrs point at, emitted once per program module.
/// Empty when no field in the schema carries a non-native length prefix.
pub fn helpers(schema_ir: &SchemaIr) -> TokenStream {
    let fields: Vec<&FieldIr> = schema_ir
        .types
        .iter()
        .flat_map(|type_ir| &type_ir.fields)
        .filter(|field| prefix_of(&field.field_type).is_some())
        .collect();

    if fields.is_empty() {
        return quote! {};
    }

    // Emit only what the schema reaches; an unused helper is a dead_code warning downstream.
    let uses = |predicate: &dyn Fn(&FieldIr) -> bool| fields.iter().any(|field| predicate(field));

    let is_short_u16 = |field: &FieldIr| {
        matches!(
            prefix_of(&field.field_type),
            Some((NumberFormat::ShortU16, _))
        )
    };

    let number_prefix = uses(&|field| !is_short_u16(field)).then(number_size_prefix);
    let short_u16_prefix = uses(&is_short_u16).then(short_u16_size_prefix);

    let optional = uses(&|field| {
        matches!(&field.label, LabelIr::Optional(encoding)
            if matches!(encoding.prefix, OptionPrefixIr::FixedWidth { .. }))
    })
    .then(optional_size_prefixed);

    let short_u16_optional = uses(&|field| {
        matches!(&field.label, LabelIr::Optional(encoding)
            if encoding.prefix == OptionPrefixIr::ShortU16)
    })
    .then(short_u16_optional_size_prefixed);

    let repeated = uses(&|field| matches!(field.label, LabelIr::Repeated)).then(vec_size_prefixed);

    let fixed_array = uses(&|field| matches!(field.label, LabelIr::FixedArray(_)))
        .then(fixed_array_size_prefixed);

    let singular = singular_size_prefixed();

    quote! {
        #singular
        #number_prefix
        #short_u16_prefix
        #optional
        #short_u16_optional
        #repeated
        #fixed_array
    }
}

/// The two traits every size-prefixed codec is generic over -- the length prefix and the
/// payload behind it -- plus the singular codec the label helpers all delegate to.
fn singular_size_prefixed() -> TokenStream {
    quote! {
        /// A Codama length prefix that borsh does not write natively.
        trait ShipsternSizePrefix {
            fn read_length<R: ::borsh::io::Read>(
                reader: &mut R,
            ) -> ::core::result::Result<u64, ::borsh::io::Error>;

            fn write_length<W: ::borsh::io::Write>(
                length: usize,
                writer: &mut W,
            ) -> ::core::result::Result<(), ::borsh::io::Error>;
        }

        /// A payload carried behind a length prefix: `Vec<u8>` for bytes, `String` for a
        /// UTF-8 string.
        trait ShipsternSizePrefixed: Sized {
            fn from_prefixed_bytes(
                bytes: Vec<u8>,
            ) -> ::core::result::Result<Self, ::borsh::io::Error>;

            fn as_prefixed_bytes(&self) -> &[u8];
        }

        impl ShipsternSizePrefixed for Vec<u8> {
            fn from_prefixed_bytes(
                bytes: Vec<u8>,
            ) -> ::core::result::Result<Self, ::borsh::io::Error> {
                ::core::result::Result::Ok(bytes)
            }

            fn as_prefixed_bytes(&self) -> &[u8] {
                self
            }
        }

        impl ShipsternSizePrefixed for String {
            fn from_prefixed_bytes(
                bytes: Vec<u8>,
            ) -> ::core::result::Result<Self, ::borsh::io::Error> {
                String::from_utf8(bytes).map_err(|_| ::borsh::io::Error::new(
                    ::borsh::io::ErrorKind::InvalidData,
                    "size-prefixed string is not valid UTF-8",
                ))
            }

            fn as_prefixed_bytes(&self) -> &[u8] {
                self.as_bytes()
            }
        }

        /// Borsh: read a length-prefixed payload without trusting the declared length.
        fn borsh_deserialize_size_prefixed<
            P: ShipsternSizePrefix,
            T: ShipsternSizePrefixed,
            R: ::borsh::io::Read,
        >(
            reader: &mut R,
        ) -> ::core::result::Result<T, ::borsh::io::Error> {
            let length = P::read_length(reader)?;

            // Grow only as actual input arrives, never allocate from the untrusted prefix.
            let mut bytes = Vec::new();
            ::borsh::io::Read::read_to_end(
                &mut ::borsh::io::Read::take(reader, length),
                &mut bytes,
            )?;

            if bytes.len() as u64 != length {
                return ::core::result::Result::Err(::borsh::io::Error::new(
                    ::borsh::io::ErrorKind::UnexpectedEof,
                    "truncated size-prefixed bytes",
                ));
            }

            T::from_prefixed_bytes(bytes)
        }

        fn borsh_serialize_size_prefixed<
            P: ShipsternSizePrefix,
            T: ShipsternSizePrefixed,
            W: ::borsh::io::Write,
        >(
            value: &T,
            writer: &mut W,
        ) -> ::core::result::Result<(), ::borsh::io::Error> {
            let bytes = value.as_prefixed_bytes();

            P::write_length(bytes.len(), writer)?;
            writer.write_all(bytes)
        }
    }
}

/// An unsigned integer length prefix, `PREFIX_BYTES` wide.
fn number_size_prefix() -> TokenStream {
    quote! {
        struct ShipsternNumberSizePrefix<const PREFIX_BYTES: usize, const BIG_ENDIAN: bool>;

        impl<const PREFIX_BYTES: usize, const BIG_ENDIAN: bool> ShipsternSizePrefix
            for ShipsternNumberSizePrefix<PREFIX_BYTES, BIG_ENDIAN>
        {
            fn read_length<R: ::borsh::io::Read>(
                reader: &mut R,
            ) -> ::core::result::Result<u64, ::borsh::io::Error> {
                let mut bytes = [0u8; PREFIX_BYTES];
                reader.read_exact(&mut bytes)?;

                let mut length = 0u128;
                for (index, byte) in bytes.iter().enumerate() {
                    let shift = if BIG_ENDIAN {
                        (PREFIX_BYTES - index - 1) * 8
                    } else {
                        index * 8
                    };

                    length |= u128::from(*byte) << shift;
                }

                u64::try_from(length).map_err(|_| ::borsh::io::Error::new(
                    ::borsh::io::ErrorKind::InvalidData,
                    "byte length exceeds u64",
                ))
            }

            fn write_length<W: ::borsh::io::Write>(
                length: usize,
                writer: &mut W,
            ) -> ::core::result::Result<(), ::borsh::io::Error> {
                let length = length as u128;

                if PREFIX_BYTES < 16 && length >> (PREFIX_BYTES * 8) != 0 {
                    return ::core::result::Result::Err(::borsh::io::Error::new(
                        ::borsh::io::ErrorKind::InvalidInput,
                        "byte length overflow",
                    ));
                }

                let mut bytes = [0u8; PREFIX_BYTES];
                for (index, byte) in bytes.iter_mut().enumerate() {
                    let shift = if BIG_ENDIAN {
                        (PREFIX_BYTES - index - 1) * 8
                    } else {
                        index * 8
                    };

                    *byte = ((length >> shift) & 0xff) as u8;
                }

                writer.write_all(&bytes)
            }
        }
    }
}

/// The Solana `short_u16` (compact-u16) length prefix: 1-3 bytes, minimal encoding only.
fn short_u16_size_prefix() -> TokenStream {
    quote! {
        struct ShipsternShortU16SizePrefix;

        impl ShipsternSizePrefix for ShipsternShortU16SizePrefix {
            fn read_length<R: ::borsh::io::Read>(
                reader: &mut R,
            ) -> ::core::result::Result<u64, ::borsh::io::Error> {
                let mut length = 0u16;

                for index in 0..3 {
                    let byte = <u8 as ::borsh::BorshDeserialize>::deserialize_reader(reader)?;

                    if (index == 2 && byte > 3) || (index > 0 && byte == 0) {
                        return ::core::result::Result::Err(::borsh::io::Error::new(
                            ::borsh::io::ErrorKind::InvalidData,
                            "invalid short_u16 byte length",
                        ));
                    }

                    length |= u16::from(byte & 0x7f) << (index * 7);

                    if byte & 0x80 == 0 {
                        break;
                    }
                }

                ::core::result::Result::Ok(u64::from(length))
            }

            fn write_length<W: ::borsh::io::Write>(
                length: usize,
                writer: &mut W,
            ) -> ::core::result::Result<(), ::borsh::io::Error> {
                let mut length = u16::try_from(length).map_err(|_| ::borsh::io::Error::new(
                    ::borsh::io::ErrorKind::InvalidInput,
                    "byte length overflow",
                ))?;

                loop {
                    let byte = (length & 0x7f) as u8;
                    length >>= 7;

                    writer.write_all(&[if length == 0 { byte } else { byte | 0x80 }])?;

                    if length == 0 {
                        break;
                    }
                }

                ::core::result::Result::Ok(())
            }
        }
    }
}

/// A size-prefixed payload behind a fixed-width Codama option prefix. There is no `None`
/// padding arm: a variable-size payload can never be the item of a fixed option.
fn optional_size_prefixed() -> TokenStream {
    quote! {
        fn borsh_deserialize_opt_size_prefixed<
            P: ShipsternSizePrefix,
            T: ShipsternSizePrefixed,
            const PREFIX_BYTES: usize,
            const PREFIX_ONE: u128,
            const BIG_ENDIAN: bool,
            R: ::borsh::io::Read,
        >(
            reader: &mut R,
        ) -> ::core::result::Result<::core::option::Option<T>, ::borsh::io::Error> {
            if borsh_deserialize_option_prefix::<PREFIX_BYTES, PREFIX_ONE, BIG_ENDIAN, _>(reader)? {
                ::core::result::Result::Ok(::core::option::Option::Some(
                    borsh_deserialize_size_prefixed::<P, T, _>(reader)?,
                ))
            } else {
                ::core::result::Result::Ok(::core::option::Option::None)
            }
        }

        fn borsh_serialize_opt_size_prefixed<
            P: ShipsternSizePrefix,
            T: ShipsternSizePrefixed,
            const PREFIX_BYTES: usize,
            const PREFIX_ONE: u128,
            const BIG_ENDIAN: bool,
            W: ::borsh::io::Write,
        >(
            value: &::core::option::Option<T>,
            writer: &mut W,
        ) -> ::core::result::Result<(), ::borsh::io::Error> {
            borsh_serialize_option_prefix::<PREFIX_BYTES, PREFIX_ONE, BIG_ENDIAN, _>(
                value.is_some(),
                writer,
            )?;

            match value {
                ::core::option::Option::Some(value) => {
                    borsh_serialize_size_prefixed::<P, T, _>(value, writer)
                },
                ::core::option::Option::None => ::core::result::Result::Ok(()),
            }
        }
    }
}

/// A size-prefixed payload behind a `short_u16` Codama option prefix.
fn short_u16_optional_size_prefixed() -> TokenStream {
    quote! {
        fn borsh_deserialize_short_u16_opt_size_prefixed<
            P: ShipsternSizePrefix,
            T: ShipsternSizePrefixed,
            R: ::borsh::io::Read,
        >(
            reader: &mut R,
        ) -> ::core::result::Result<::core::option::Option<T>, ::borsh::io::Error> {
            if borsh_deserialize_short_u16_option_prefix(reader)? {
                ::core::result::Result::Ok(::core::option::Option::Some(
                    borsh_deserialize_size_prefixed::<P, T, _>(reader)?,
                ))
            } else {
                ::core::result::Result::Ok(::core::option::Option::None)
            }
        }

        fn borsh_serialize_short_u16_opt_size_prefixed<
            P: ShipsternSizePrefix,
            T: ShipsternSizePrefixed,
            W: ::borsh::io::Write,
        >(
            value: &::core::option::Option<T>,
            writer: &mut W,
        ) -> ::core::result::Result<(), ::borsh::io::Error> {
            borsh_serialize_short_u16_option_prefix(value.is_some(), writer)?;

            match value {
                ::core::option::Option::Some(value) => {
                    borsh_serialize_size_prefixed::<P, T, _>(value, writer)
                },
                ::core::option::Option::None => ::core::result::Result::Ok(()),
            }
        }
    }
}

/// A u32 LE-counted sequence of size-prefixed payloads.
fn vec_size_prefixed() -> TokenStream {
    quote! {
        fn borsh_deserialize_vec_size_prefixed<
            P: ShipsternSizePrefix,
            T: ShipsternSizePrefixed,
            R: ::borsh::io::Read,
        >(
            reader: &mut R,
        ) -> ::core::result::Result<Vec<T>, ::borsh::io::Error> {
            let count = <u32 as ::borsh::BorshDeserialize>::deserialize_reader(reader)?;

            // Grow only as actual input arrives, never allocate from the untrusted count.
            let mut values = Vec::new();
            for _ in 0..count {
                values.push(borsh_deserialize_size_prefixed::<P, T, _>(reader)?);
            }

            ::core::result::Result::Ok(values)
        }

        fn borsh_serialize_vec_size_prefixed<
            P: ShipsternSizePrefix,
            T: ShipsternSizePrefixed,
            W: ::borsh::io::Write,
        >(
            value: &[T],
            writer: &mut W,
        ) -> ::core::result::Result<(), ::borsh::io::Error> {
            let count = u32::try_from(value.len()).map_err(|_| ::borsh::io::Error::new(
                ::borsh::io::ErrorKind::InvalidInput,
                "byte-array count overflow",
            ))?;
            ::borsh::BorshSerialize::serialize(&count, writer)?;

            for value in value {
                borsh_serialize_size_prefixed::<P, T, _>(value, writer)?;
            }

            ::core::result::Result::Ok(())
        }
    }
}

///
/// An uncounted run of exactly `N` size-prefixed payloads.
///
/// The serializer rejects a `Vec` whose length is not `N`. `borsh_serialize_fixed_array` does
/// not: it writes however many elements it is handed. That one is the outlier and should be
/// aligned separately; until then both behaviours live in the same generated module.
///
fn fixed_array_size_prefixed() -> TokenStream {
    quote! {
        fn borsh_deserialize_fixed_array_size_prefixed<
            P: ShipsternSizePrefix,
            T: ShipsternSizePrefixed,
            const N: usize,
            R: ::borsh::io::Read,
        >(
            reader: &mut R,
        ) -> ::core::result::Result<Vec<T>, ::borsh::io::Error> {
            let mut values = Vec::with_capacity(N);
            for _ in 0..N {
                values.push(borsh_deserialize_size_prefixed::<P, T, _>(reader)?);
            }

            ::core::result::Result::Ok(values)
        }

        fn borsh_serialize_fixed_array_size_prefixed<
            P: ShipsternSizePrefix,
            T: ShipsternSizePrefixed,
            const N: usize,
            W: ::borsh::io::Write,
        >(
            value: &[T],
            writer: &mut W,
        ) -> ::core::result::Result<(), ::borsh::io::Error> {
            if value.len() != N {
                return ::core::result::Result::Err(::borsh::io::Error::new(
                    ::borsh::io::ErrorKind::InvalidInput,
                    "invalid fixed byte-array count",
                ));
            }

            for value in value {
                borsh_serialize_size_prefixed::<P, T, _>(value, writer)?;
            }

            ::core::result::Result::Ok(())
        }
    }
}
