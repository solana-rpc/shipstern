//! Borsh field codecs for bytes whose Codama length prefix is not native u32 LE.
//! Keep the public field as Vec<u8> and the protobuf representation as bytes.
use codama_nodes::{Endianness, NumberFormat};
use proc_macro2::{Span, TokenStream};
use quote::{format_ident, quote};
use syn::LitStr;

use crate::intermediate_representation::{FieldIr, FieldTypeIr, LabelIr, OptionPrefixIr, ScalarIr};

pub(super) fn attrs(field: &FieldIr) -> TokenStream {
    let read = LitStr::new(
        &format!("Self::__shipstern_read_{}", field.name),
        Span::call_site(),
    );
    let write = LitStr::new(
        &format!("Self::__shipstern_write_{}", field.name),
        Span::call_site(),
    );
    quote! { #[borsh(deserialize_with = #read, serialize_with = #write)] }
}

pub(super) fn methods(field: &FieldIr, in_module: bool) -> TokenStream {
    let FieldTypeIr::Scalar(ScalarIr::SizePrefixedBytes { format, endian }) = &field.field_type
    else {
        return quote! {};
    };
    let read_name = format_ident!("__shipstern_read_{}", field.name);
    let write_name = format_ident!("__shipstern_write_{}", field.name);
    let (read_length, write_length) = prefix_codec(*format, *endian);
    let (ty, read, write) = match &field.label {
        LabelIr::Singular => (
            quote!(Vec<u8>),
            quote!(read_bytes(reader)),
            quote!(write_bytes(value, writer)),
        ),
        LabelIr::Optional(encoding) => {
            let parent = if in_module { quote!(super::) } else { quote!() };
            let (read_tag, write_tag) = match &encoding.prefix {
                OptionPrefixIr::FixedWidth {
                    byte_len,
                    one_value,
                    big_endian,
                } => {
                    let byte_len = crate::utils::unsuffixed(*byte_len as u64);
                    let one_value = proc_macro2::Literal::u128_unsuffixed(*one_value);
                    (
                        quote!(#parent borsh_deserialize_option_prefix::<#byte_len, #one_value, #big_endian, _>(reader)?),
                        quote!(#parent borsh_serialize_option_prefix::<#byte_len, #one_value, #big_endian, _>(value.is_some(), writer)?;),
                    )
                },
                OptionPrefixIr::ShortU16 => (
                    quote!(#parent borsh_deserialize_short_u16_option_prefix(reader)?),
                    quote!(#parent borsh_serialize_short_u16_option_prefix(value.is_some(), writer)?;),
                ),
            };
            // Variable-size byte strings cannot be the item of a fixed Codama option.
            debug_assert!(encoding.none_padding.is_none());
            (
                quote!(Option<Vec<u8>>),
                quote! { if #read_tag { Ok(Some(read_bytes(reader)?)) } else { Ok(None) } },
                quote! { #write_tag if let Some(value) = value { write_bytes(value, writer)?; } Ok(()) },
            )
        },
        LabelIr::Repeated | LabelIr::FixedArray(_) => {
            let (count, write_count) = match &field.label {
                LabelIr::FixedArray(n) => {
                    let n = crate::utils::unsuffixed(*n as u64);
                    (quote!(#n), quote! {
                        if value.len() != #n {
                            return Err(::borsh::io::Error::new(::borsh::io::ErrorKind::InvalidInput, "invalid fixed byte-array count"));
                        }
                    })
                },
                _ => (
                    quote!(<u32 as ::borsh::BorshDeserialize>::deserialize_reader(
                        reader
                    )?),
                    quote! {
                        let count = u32::try_from(value.len()).map_err(|_| ::borsh::io::Error::new(::borsh::io::ErrorKind::InvalidInput, "byte-array count overflow"))?;
                        ::borsh::BorshSerialize::serialize(&count, writer)?;
                    },
                ),
            };
            (
                quote!(Vec<Vec<u8>>),
                quote! {
                    let count = #count;
                    let mut values = Vec::new();
                    for _ in 0..count { values.push(read_bytes(reader)?); }
                    Ok(values)
                },
                quote! {
                    #write_count
                    for value in value { write_bytes(value, writer)?; }
                    Ok(())
                },
            )
        },
    };
    let write_ty = match &field.label {
        LabelIr::Singular => quote!([u8]),
        LabelIr::Optional(_) => quote!(Option<Vec<u8>>),
        LabelIr::Repeated | LabelIr::FixedArray(_) => quote!([Vec<u8>]),
    };
    quote! {
        fn #read_name<R: ::borsh::io::Read>(reader: &mut R) -> ::borsh::io::Result<#ty> {
            fn read_bytes<R: ::borsh::io::Read>(reader: &mut R) -> ::borsh::io::Result<Vec<u8>> {
                let length: u64 = #read_length;
                // Grow only as actual input arrives, never allocate from the untrusted prefix.
                let mut bytes = Vec::new();
                ::borsh::io::Read::read_to_end(&mut ::borsh::io::Read::take(reader, length), &mut bytes)?;
                if bytes.len() as u64 != length {
                    return Err(::borsh::io::Error::new(::borsh::io::ErrorKind::UnexpectedEof, "truncated size-prefixed bytes"));
                }
                Ok(bytes)
            }
            #read
        }
        fn #write_name<W: ::borsh::io::Write>(value: &#write_ty, writer: &mut W) -> ::borsh::io::Result<()> {
            fn write_bytes<W: ::borsh::io::Write>(value: &[u8], writer: &mut W) -> ::borsh::io::Result<()> {
                #write_length
                writer.write_all(value)
            }
            #write
        }
    }
}

fn prefix_codec(format: NumberFormat, endian: Endianness) -> (TokenStream, TokenStream) {
    if format == NumberFormat::ShortU16 {
        return (
            quote! {{
                let mut length = 0u16;
                for index in 0..3 {
                    let byte = <u8 as ::borsh::BorshDeserialize>::deserialize_reader(reader)?;
                    if (index == 2 && byte > 3) || (index > 0 && byte == 0) {
                        return Err(::borsh::io::Error::new(::borsh::io::ErrorKind::InvalidData, "invalid short_u16 byte length"));
                    }
                    length |= u16::from(byte & 0x7f) << (index * 7);
                    if byte & 0x80 == 0 { break; }
                }
                u64::from(length)
            }},
            quote! {
                let mut length = u16::try_from(value.len()).map_err(|_| ::borsh::io::Error::new(::borsh::io::ErrorKind::InvalidInput, "byte length overflow"))?;
                loop {
                    let byte = (length & 0x7f) as u8;
                    length >>= 7;
                    writer.write_all(&[if length == 0 { byte } else { byte | 0x80 }])?;
                    if length == 0 { break; }
                }
            },
        );
    }
    let (ty, size) = match format {
        NumberFormat::U8 => (quote!(u8), 1),
        NumberFormat::U16 => (quote!(u16), 2),
        NumberFormat::U32 => (quote!(u32), 4),
        NumberFormat::U64 => (quote!(u64), 8),
        NumberFormat::U128 => (quote!(u128), 16),
        other => panic!("unsupported byte length prefix: {other:?}; expected an unsigned integer"),
    };
    let size = crate::utils::unsuffixed(size);
    let (from, to) = if endian == Endianness::Be {
        (format_ident!("from_be_bytes"), format_ident!("to_be_bytes"))
    } else {
        (format_ident!("from_le_bytes"), format_ident!("to_le_bytes"))
    };
    (
        quote! {{
            let mut prefix = [0u8; #size];
            reader.read_exact(&mut prefix)?;
            u64::try_from(#ty::#from(prefix)).map_err(|_| ::borsh::io::Error::new(::borsh::io::ErrorKind::InvalidData, "byte length exceeds u64"))?
        }},
        quote! {
            let length = #ty::try_from(value.len()).map_err(|_| ::borsh::io::Error::new(::borsh::io::ErrorKind::InvalidInput, "byte length overflow"))?;
            writer.write_all(&length.#to())?;
        },
    )
}
