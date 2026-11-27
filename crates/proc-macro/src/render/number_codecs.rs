//! Borsh field codecs for numbers whose Codama wire format is not what borsh writes
//! natively. Only the wire codec changes: fields point `deserialize_with` and
//! `serialize_with` at helpers emitted once per program module by [`helpers`].
use codama_nodes::NumberFormat;
use proc_macro2::{Span, TokenStream};
use quote::quote;
use syn::LitStr;

use crate::intermediate_representation::{
    FieldIr, FieldTypeIr, LabelIr, OptionPrefixIr, ScalarIr, SchemaIr,
};

/// Whether borsh's own little-endian codec would misread this scalar's wire format.
pub fn has_codec(field_type: &FieldTypeIr) -> bool {
    matches!(
        field_type,
        FieldTypeIr::Scalar(ScalarIr::ShortU16 | ScalarIr::BigEndian(_))
    )
}

/// The Rust type a big-endian number is held in.
fn big_endian_type(format: NumberFormat) -> &'static str {
    match format {
        NumberFormat::U16 => "u16",
        NumberFormat::U32 => "u32",
        NumberFormat::U64 => "u64",
        NumberFormat::U128 => "u128",
        NumberFormat::I16 => "i16",
        NumberFormat::I32 => "i32",
        NumberFormat::I64 => "i64",
        NumberFormat::I128 => "i128",
        NumberFormat::F32 => "f32",
        NumberFormat::F64 => "f64",
        other => unreachable!("{other:?} has no big-endian form"),
    }
}

/// The marker type implementing `ShipsternNumberCodec` for this scalar.
fn codec_type(scalar: &ScalarIr, path_prefix: &str) -> String {
    match scalar {
        ScalarIr::ShortU16 => format!("{path_prefix}ShipsternShortU16Codec"),
        ScalarIr::BigEndian(format) => format!(
            "{path_prefix}ShipsternBigEndianCodec<{}>",
            big_endian_type(*format)
        ),
        other => unreachable!("no number codec for {other:?}"),
    }
}

///
/// Borsh attrs for a number with a non-native codec. Empty for every other field type.
///
/// Example output:
///
/// ```rust, ignore
/// #[borsh(
///     deserialize_with = "borsh_deserialize_number::<ShipsternShortU16Codec, _>",
///     serialize_with = "borsh_serialize_number::<ShipsternShortU16Codec, _>"
/// )]
/// ```
///
pub fn attrs(field: &FieldIr, path_prefix: &str) -> TokenStream {
    let FieldTypeIr::Scalar(scalar) = &field.field_type else {
        return quote! {};
    };

    if !has_codec(&field.field_type) {
        return quote! {};
    }

    let codec = codec_type(scalar, path_prefix);

    let (deserialize, serialize) = match &field.label {
        LabelIr::Singular => (
            format!("{path_prefix}borsh_deserialize_number::<{codec}, _>"),
            format!("{path_prefix}borsh_serialize_number::<{codec}, _>"),
        ),

        LabelIr::Optional(encoding) => {
            let padding = encoding.none_padding.unwrap_or(0);

            match &encoding.prefix {
                OptionPrefixIr::FixedWidth {
                    byte_len,
                    one_value,
                    big_endian,
                } => {
                    let args =
                        format!("{codec}, {byte_len}, {one_value}, {big_endian}, {padding}, _");

                    (
                        format!("{path_prefix}borsh_deserialize_opt_number::<{args}>"),
                        format!("{path_prefix}borsh_serialize_opt_number::<{args}>"),
                    )
                },
                OptionPrefixIr::ShortU16 => (
                    format!(
                        "{path_prefix}borsh_deserialize_short_u16_opt_number::<{codec}, \
                         {padding}, _>"
                    ),
                    format!(
                        "{path_prefix}borsh_serialize_short_u16_opt_number::<{codec}, {padding}, \
                         _>"
                    ),
                ),
            }
        },

        LabelIr::Repeated => (
            format!("{path_prefix}borsh_deserialize_vec_number::<{codec}, _>"),
            format!("{path_prefix}borsh_serialize_vec_number::<{codec}, _>"),
        ),

        LabelIr::FixedArray(n) => (
            format!("{path_prefix}borsh_deserialize_fixed_array_number::<{codec}, {n}, _>"),
            format!("{path_prefix}borsh_serialize_fixed_array_number::<{codec}, {n}, _>"),
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

/// The generic codecs the field attrs point at, emitted once per program module.
/// Empty when no field in the schema carries a non-native number.
pub fn helpers(schema_ir: &SchemaIr) -> TokenStream {
    let fields: Vec<&FieldIr> = schema_ir
        .types
        .iter()
        .flat_map(|type_ir| &type_ir.fields)
        .filter(|field| has_codec(&field.field_type))
        .collect();

    if fields.is_empty() {
        return quote! {};
    }

    // Emit only what the schema reaches; an unused helper is a dead_code warning downstream.
    let uses = |predicate: &dyn Fn(&FieldIr) -> bool| fields.iter().any(|field| predicate(field));

    let short_u16 =
        uses(&|field| matches!(field.field_type, FieldTypeIr::Scalar(ScalarIr::ShortU16)))
            .then(short_u16_codec);

    let big_endian = uses(&|field| {
        matches!(
            field.field_type,
            FieldTypeIr::Scalar(ScalarIr::BigEndian(_))
        )
    })
    .then(big_endian_codec);

    let optional = uses(&|field| {
        matches!(&field.label, LabelIr::Optional(encoding)
            if matches!(encoding.prefix, OptionPrefixIr::FixedWidth { .. }))
    })
    .then(optional_number);

    let short_u16_optional = uses(&|field| {
        matches!(&field.label, LabelIr::Optional(encoding)
            if encoding.prefix == OptionPrefixIr::ShortU16)
    })
    .then(short_u16_optional_number);

    let repeated = uses(&|field| matches!(field.label, LabelIr::Repeated)).then(vec_number);

    let fixed_array =
        uses(&|field| matches!(field.label, LabelIr::FixedArray(_))).then(fixed_array_number);

    let singular = singular_number();

    quote! {
        #singular
        #short_u16
        #big_endian
        #optional
        #short_u16_optional
        #repeated
        #fixed_array
    }
}

/// The codec trait every number helper is generic over, plus the singular codec the label
/// helpers all delegate to.
fn singular_number() -> TokenStream {
    quote! {
        /// A Codama number format that borsh does not write natively.
        trait ShipsternNumberCodec {
            type Value;

            fn read<R: ::borsh::io::Read>(
                reader: &mut R,
            ) -> ::core::result::Result<Self::Value, ::borsh::io::Error>;

            fn write<W: ::borsh::io::Write>(
                value: &Self::Value,
                writer: &mut W,
            ) -> ::core::result::Result<(), ::borsh::io::Error>;
        }

        fn borsh_deserialize_number<C: ShipsternNumberCodec, R: ::borsh::io::Read>(
            reader: &mut R,
        ) -> ::core::result::Result<C::Value, ::borsh::io::Error> {
            C::read(reader)
        }

        fn borsh_serialize_number<C: ShipsternNumberCodec, W: ::borsh::io::Write>(
            value: &C::Value,
            writer: &mut W,
        ) -> ::core::result::Result<(), ::borsh::io::Error> {
            C::write(value, writer)
        }
    }
}

/// The Solana `short_u16` (compact-u16) number: 1-3 bytes, minimal encoding only.
fn short_u16_codec() -> TokenStream {
    quote! {
        struct ShipsternShortU16Codec;

        impl ShipsternNumberCodec for ShipsternShortU16Codec {
            type Value = u16;

            fn read<R: ::borsh::io::Read>(
                reader: &mut R,
            ) -> ::core::result::Result<u16, ::borsh::io::Error> {
                let mut value = 0u16;

                for index in 0..3 {
                    let byte = <u8 as ::borsh::BorshDeserialize>::deserialize_reader(reader)?;

                    if (index == 2 && byte > 3) || (index > 0 && byte == 0) {
                        return ::core::result::Result::Err(::borsh::io::Error::new(
                            ::borsh::io::ErrorKind::InvalidData,
                            "invalid short_u16 number",
                        ));
                    }

                    value |= u16::from(byte & 0x7f) << (index * 7);

                    if byte & 0x80 == 0 {
                        break;
                    }
                }

                ::core::result::Result::Ok(value)
            }

            fn write<W: ::borsh::io::Write>(
                value: &u16,
                writer: &mut W,
            ) -> ::core::result::Result<(), ::borsh::io::Error> {
                let mut value = *value;

                loop {
                    let byte = (value & 0x7f) as u8;
                    value >>= 7;

                    writer.write_all(&[if value == 0 { byte } else { byte | 0x80 }])?;

                    if value == 0 {
                        break;
                    }
                }

                ::core::result::Result::Ok(())
            }
        }
    }
}

/// Big-endian numbers, floats included: a float round-trips through its bits, so NaN and
/// infinity decode instead of being rejected.
fn big_endian_codec() -> TokenStream {
    let impls = [
        "u16", "u32", "u64", "u128", "i16", "i32", "i64", "i128", "f32", "f64",
    ]
    .map(|name| {
        let ty = quote::format_ident!("{name}");

        quote! {
            impl ShipsternNumberCodec for ShipsternBigEndianCodec<#ty> {
                type Value = #ty;

                fn read<R: ::borsh::io::Read>(
                    reader: &mut R,
                ) -> ::core::result::Result<#ty, ::borsh::io::Error> {
                    let mut bytes = [0u8; ::core::mem::size_of::<#ty>()];
                    reader.read_exact(&mut bytes)?;

                    ::core::result::Result::Ok(#ty::from_be_bytes(bytes))
                }

                fn write<W: ::borsh::io::Write>(
                    value: &#ty,
                    writer: &mut W,
                ) -> ::core::result::Result<(), ::borsh::io::Error> {
                    writer.write_all(&value.to_be_bytes())
                }
            }
        }
    });

    quote! {
        struct ShipsternBigEndianCodec<T>(::core::marker::PhantomData<T>);

        #(#impls)*
    }
}

/// A number behind a fixed-width Codama option prefix. `None` skips the padding a fixed
/// option reserves for the item.
fn optional_number() -> TokenStream {
    quote! {
        fn borsh_deserialize_opt_number<
            C: ShipsternNumberCodec,
            const PREFIX_BYTES: usize,
            const PREFIX_ONE: u128,
            const BIG_ENDIAN: bool,
            const NONE_PADDING: usize,
            R: ::borsh::io::Read,
        >(
            reader: &mut R,
        ) -> ::core::result::Result<::core::option::Option<C::Value>, ::borsh::io::Error> {
            if borsh_deserialize_option_prefix::<PREFIX_BYTES, PREFIX_ONE, BIG_ENDIAN, _>(reader)? {
                ::core::result::Result::Ok(::core::option::Option::Some(C::read(reader)?))
            } else {
                borsh_deserialize_option_none_padding::<NONE_PADDING, _>(reader)?;
                ::core::result::Result::Ok(::core::option::Option::None)
            }
        }

        fn borsh_serialize_opt_number<
            C: ShipsternNumberCodec,
            const PREFIX_BYTES: usize,
            const PREFIX_ONE: u128,
            const BIG_ENDIAN: bool,
            const NONE_PADDING: usize,
            W: ::borsh::io::Write,
        >(
            value: &::core::option::Option<C::Value>,
            writer: &mut W,
        ) -> ::core::result::Result<(), ::borsh::io::Error> {
            borsh_serialize_option_prefix::<PREFIX_BYTES, PREFIX_ONE, BIG_ENDIAN, _>(
                value.is_some(),
                writer,
            )?;

            match value {
                ::core::option::Option::Some(value) => C::write(value, writer),
                ::core::option::Option::None => {
                    borsh_serialize_option_none_padding::<NONE_PADDING, _>(writer)
                },
            }
        }
    }
}

/// A number behind a `short_u16` Codama option prefix.
fn short_u16_optional_number() -> TokenStream {
    quote! {
        fn borsh_deserialize_short_u16_opt_number<
            C: ShipsternNumberCodec,
            const NONE_PADDING: usize,
            R: ::borsh::io::Read,
        >(
            reader: &mut R,
        ) -> ::core::result::Result<::core::option::Option<C::Value>, ::borsh::io::Error> {
            if borsh_deserialize_short_u16_option_prefix(reader)? {
                ::core::result::Result::Ok(::core::option::Option::Some(C::read(reader)?))
            } else {
                borsh_deserialize_option_none_padding::<NONE_PADDING, _>(reader)?;
                ::core::result::Result::Ok(::core::option::Option::None)
            }
        }

        fn borsh_serialize_short_u16_opt_number<
            C: ShipsternNumberCodec,
            const NONE_PADDING: usize,
            W: ::borsh::io::Write,
        >(
            value: &::core::option::Option<C::Value>,
            writer: &mut W,
        ) -> ::core::result::Result<(), ::borsh::io::Error> {
            borsh_serialize_short_u16_option_prefix(value.is_some(), writer)?;

            match value {
                ::core::option::Option::Some(value) => C::write(value, writer),
                ::core::option::Option::None => {
                    borsh_serialize_option_none_padding::<NONE_PADDING, _>(writer)
                },
            }
        }
    }
}

/// A u32 LE-counted sequence of numbers.
fn vec_number() -> TokenStream {
    quote! {
        fn borsh_deserialize_vec_number<C: ShipsternNumberCodec, R: ::borsh::io::Read>(
            reader: &mut R,
        ) -> ::core::result::Result<Vec<C::Value>, ::borsh::io::Error> {
            let count = <u32 as ::borsh::BorshDeserialize>::deserialize_reader(reader)?;

            // Grow only as actual input arrives, never allocate from the untrusted count.
            let mut values = Vec::new();
            for _ in 0..count {
                values.push(C::read(reader)?);
            }

            ::core::result::Result::Ok(values)
        }

        fn borsh_serialize_vec_number<C: ShipsternNumberCodec, W: ::borsh::io::Write>(
            value: &[C::Value],
            writer: &mut W,
        ) -> ::core::result::Result<(), ::borsh::io::Error> {
            let count = u32::try_from(value.len()).map_err(|_| ::borsh::io::Error::new(
                ::borsh::io::ErrorKind::InvalidInput,
                "number count overflow",
            ))?;
            ::borsh::BorshSerialize::serialize(&count, writer)?;

            for value in value {
                C::write(value, writer)?;
            }

            ::core::result::Result::Ok(())
        }
    }
}

/// An uncounted run of exactly `N` numbers. The serializer rejects a `Vec` whose length is
/// not `N`.
fn fixed_array_number() -> TokenStream {
    quote! {
        fn borsh_deserialize_fixed_array_number<
            C: ShipsternNumberCodec,
            const N: usize,
            R: ::borsh::io::Read,
        >(
            reader: &mut R,
        ) -> ::core::result::Result<Vec<C::Value>, ::borsh::io::Error> {
            let mut values = Vec::new();
            for _ in 0..N {
                values.push(C::read(reader)?);
            }

            ::core::result::Result::Ok(values)
        }

        fn borsh_serialize_fixed_array_number<
            C: ShipsternNumberCodec,
            const N: usize,
            W: ::borsh::io::Write,
        >(
            value: &[C::Value],
            writer: &mut W,
        ) -> ::core::result::Result<(), ::borsh::io::Error> {
            if value.len() != N {
                return ::core::result::Result::Err(::borsh::io::Error::new(
                    ::borsh::io::ErrorKind::InvalidInput,
                    "invalid fixed number-array count",
                ));
            }

            for value in value {
                C::write(value, writer)?;
            }

            ::core::result::Result::Ok(())
        }
    }
}
