use codama_nodes::{EventNode, RootNode};
use proc_macro2::TokenStream;
use quote::{format_ident, quote};

#[derive(Debug, Clone)]
pub struct ParserConfig {
    pub cpi_event: CpiEventConfig,

    ///
    /// The envelope the IDL declared, validated once by
    /// [`crate::parse::program_envelope`].
    ///
    /// `None` when the IDL declares none, in which case `cpi_event` came from
    /// the macro arguments or the Anchor default and no event discriminator is
    /// rebased.
    ///
    pub idl_envelope: Option<crate::parse::ProgramEnvelope>,
}

#[derive(Debug, Clone)]
pub struct CpiEventConfig {
    pub discriminator: Vec<u8>,
    pub payload_offset: usize,
}

impl Default for ParserConfig {
    fn default() -> Self {
        let anchor_event_tag = super::ANCHOR_EVENT_IX_TAG.to_le_bytes();

        Self {
            cpi_event: CpiEventConfig {
                discriminator: anchor_event_tag.to_vec(),
                payload_offset: anchor_event_tag.len(),
            },
            idl_envelope: None,
        }
    }
}

pub fn shipstern_parser(
    idl: &RootNode,
    events: &[EventNode],
    config: &ParserConfig,
) -> TokenStream {
    let program_mod_ident = format_ident!("{}", crate::utils::to_snake_case(&idl.program.name));

    let program_pubkey = crate::render::program_pubkey(&idl.program.public_key);

    let schema_ir = crate::intermediate_representation::build_schema_ir(idl, events);

    let schema_types = crate::render::rust_types_from_ir(&schema_ir);
    let option_borsh_helpers = if schema_ir
        .types
        .iter()
        .flat_map(|type_ir| &type_ir.fields)
        .any(|field| {
            matches!(
                &field.label,
                crate::intermediate_representation::LabelIr::Optional(encoding)
                    if !encoding.uses_native_borsh()
            )
        }) {
        option_borsh_helpers()
    } else {
        quote! {}
    };

    let account_parser = crate::render::account_parser(&idl.program.name, &idl.program.accounts);

    let has_events = cfg!(feature = "program-events") && !events.is_empty();
    let has_instructions = !idl.program.instructions.is_empty();

    let instruction_parser = if has_instructions {
        crate::render::instruction_parser(
            &idl.program.name,
            &idl.program.instructions,
            has_events,
            &config.cpi_event,
        )
    } else {
        quote! {}
    };

    let event_parser = if has_events {
        crate::render::event_parser(
            &idl.program.name,
            events,
            has_instructions,
            config.idl_envelope.as_ref(),
        )
    } else {
        quote! {}
    };

    let program_name_pascal = crate::utils::to_pascal_case(&idl.program.name);

    let proto_schema = if cfg!(feature = "proto") {
        let output = crate::render::proto_schema_string(
            &schema_ir,
            &program_mod_ident.to_string(),
            &program_name_pascal,
        );

        let proto_lit = syn::LitStr::new(&output.schema, proc_macro2::Span::call_site());

        let account_dispatch_const = match output
            .account_dispatch_index
            .map(|i| crate::utils::unsuffixed(i as u64))
        {
            Some(account_idx) => quote! {
                /// 0-based index of the account dispatch message in the proto file descriptor.
                pub const ACCOUNT_DISPATCH_MESSAGE_INDEX: Option<usize> = Some(#account_idx);
            },
            None => quote! {
                /// 0-based index of the account dispatch message in the proto file descriptor.
                pub const ACCOUNT_DISPATCH_MESSAGE_INDEX: Option<usize> = None;
            },
        };

        let instruction_dispatch_const = match output
            .instruction_dispatch_index
            .map(|i| crate::utils::unsuffixed(i as u64))
        {
            Some(instruction_idx) => quote! {
                /// 0-based index of the instruction dispatch message in the proto file descriptor.
                pub const INSTRUCTION_DISPATCH_MESSAGE_INDEX: Option<usize> = Some(#instruction_idx);
            },
            None => quote! {
                /// 0-based index of the instruction dispatch message in the proto file descriptor.
                pub const INSTRUCTION_DISPATCH_MESSAGE_INDEX: Option<usize> = None;
            },
        };

        let program_event_output_const = match output
            .program_event_output_index
            .map(|i| crate::utils::unsuffixed(i as u64))
        {
            Some(idx) => quote! {
                /// 0-based index of the ProgramEventOutput wrapper message in the proto file descriptor.
                pub const PROGRAM_EVENT_OUTPUT_MESSAGE_INDEX: Option<usize> = Some(#idx);
            },
            None => quote! {
                /// 0-based index of the ProgramEventOutput wrapper message in the proto file descriptor.
                pub const PROGRAM_EVENT_OUTPUT_MESSAGE_INDEX: Option<usize> = None;
            },
        };

        quote! {
            /// Generated .proto schema for this program.
            pub const PROTOBUF_SCHEMA: &str = #proto_lit;

            #account_dispatch_const
            #instruction_dispatch_const
            #program_event_output_const
        }
    } else {
        quote! {}
    };

    quote! {
        pub mod #program_mod_ident {
            use shipstern_parser::prelude::*;

            pub use shipstern_core::Pubkey;

            #option_borsh_helpers

            /// Borsh: deserialize N fixed bytes into `Vec<u8>`.
            /// On-chain, fixed-size byte fields (pubkeys, u128, fixed arrays) have no
            /// length prefix, but `Vec<u8>` borsh expects `u32 len + bytes`.
            fn borsh_deserialize_fixed_bytes<const N: usize, R: ::borsh::io::Read>(
                reader: &mut R,
            ) -> ::core::result::Result<Vec<u8>, ::borsh::io::Error> {
                let mut buf = [0u8; N];

                reader.read_exact(&mut buf)?;

                ::core::result::Result::Ok(buf.to_vec())
            }

            fn borsh_serialize_fixed_bytes<const N: usize, W: ::borsh::io::Write>(
                val: &[u8],
                writer: &mut W,
            ) -> ::core::result::Result<(), ::borsh::io::Error> {
                writer.write_all(val)
            }

            fn borsh_deserialize_opt_fixed_bytes<const N: usize, R: ::borsh::io::Read>(
                reader: &mut R,
            ) -> ::core::result::Result<::core::option::Option<Vec<u8>>, ::borsh::io::Error> {
                let tag: u8 = ::borsh::BorshDeserialize::deserialize_reader(reader)?;

                match tag {
                    0 => ::core::result::Result::Ok(::core::option::Option::None),
                    1 => {
                        let mut buf = [0u8; N];

                        reader.read_exact(&mut buf)?;

                        ::core::result::Result::Ok(::core::option::Option::Some(buf.to_vec()))
                    }
                    _ => ::core::result::Result::Err(::borsh::io::Error::new(
                        ::borsh::io::ErrorKind::InvalidData,
                        "invalid option tag for fixed bytes",
                    )),
                }
            }

            fn borsh_serialize_opt_fixed_bytes<const N: usize, W: ::borsh::io::Write>(
                val: &::core::option::Option<Vec<u8>>,
                writer: &mut W,
            ) -> ::core::result::Result<(), ::borsh::io::Error> {
                match val {
                    ::core::option::Option::Some(v) => {
                        ::borsh::BorshSerialize::serialize(&1u8, writer)?;

                        writer.write_all(v)
                    }
                    ::core::option::Option::None => {
                        ::borsh::BorshSerialize::serialize(&0u8, writer)
                    }
                }
            }

            fn borsh_deserialize_vec_fixed_bytes<const N: usize, R: ::borsh::io::Read>(
                reader: &mut R,
            ) -> ::core::result::Result<Vec<Vec<u8>>, ::borsh::io::Error> {
                let len: u32 = ::borsh::BorshDeserialize::deserialize_reader(reader)?;

                let mut result = Vec::with_capacity(len as usize);

                for _ in 0..len {
                    let mut buf = [0u8; N];

                    reader.read_exact(&mut buf)?;
                    result.push(buf.to_vec());
                }

                ::core::result::Result::Ok(result)
            }

            fn borsh_serialize_vec_fixed_bytes<const N: usize, W: ::borsh::io::Write>(
                val: &[Vec<u8>],
                writer: &mut W,
            ) -> ::core::result::Result<(), ::borsh::io::Error> {
                ::borsh::BorshSerialize::serialize(&(val.len() as u32), writer)?;

                for v in val {
                    writer.write_all(v)?;
                }

                ::core::result::Result::Ok(())
            }

            /// Borsh: deserialize a fixed-count array (no length prefix on-chain).
            /// Uses standard BorshDeserialize for each element.
            fn borsh_deserialize_fixed_array<T: ::borsh::BorshDeserialize, const N: usize, R: ::borsh::io::Read>(
                reader: &mut R,
            ) -> ::core::result::Result<Vec<T>, ::borsh::io::Error> {
                let mut result = Vec::with_capacity(N);

                for _ in 0..N {
                    result.push(<T as ::borsh::BorshDeserialize>::deserialize_reader(reader)?);
                }

                ::core::result::Result::Ok(result)
            }

            fn borsh_serialize_fixed_array<T: ::borsh::BorshSerialize, const N: usize, W: ::borsh::io::Write>(
                val: &[T],
                writer: &mut W,
            ) -> ::core::result::Result<(), ::borsh::io::Error> {
                for v in val {
                    <T as ::borsh::BorshSerialize>::serialize(v, writer)?;
                }

                ::core::result::Result::Ok(())
            }

            /// Borsh: deserialize a fixed-count array of fixed-size byte fields (no length prefix).
            fn borsh_deserialize_fixed_array_fixed_bytes<const BYTE_SIZE: usize, const N: usize, R: ::borsh::io::Read>(
                reader: &mut R,
            ) -> ::core::result::Result<Vec<Vec<u8>>, ::borsh::io::Error> {
                let mut result = Vec::with_capacity(N);

                for _ in 0..N {
                    let mut buf = [0u8; BYTE_SIZE];

                    reader.read_exact(&mut buf)?;

                    result.push(buf.to_vec());
                }

                ::core::result::Result::Ok(result)
            }

            fn borsh_serialize_fixed_array_fixed_bytes<const BYTE_SIZE: usize, const N: usize, W: ::borsh::io::Write>(
                val: &[Vec<u8>],
                writer: &mut W,
            ) -> ::core::result::Result<(), ::borsh::io::Error> {
                for v in val {
                    writer.write_all(v)?;
                }

                ::core::result::Result::Ok(())
            }

            /// Borsh: deserialize a fixed-count array of f32 permissively (no length prefix, allows NaN).
            fn borsh_deserialize_fixed_array_f32_permissive<const N: usize, R: ::borsh::io::Read>(
                reader: &mut R,
            ) -> ::core::result::Result<Vec<f32>, ::borsh::io::Error> {
                let mut result = Vec::with_capacity(N);

                for _ in 0..N {
                    let mut buf = [0u8; 4];

                    reader.read_exact(&mut buf)?;

                    result.push(f32::from_le_bytes(buf));
                }

                ::core::result::Result::Ok(result)
            }

            fn borsh_serialize_fixed_array_f32_permissive<const N: usize, W: ::borsh::io::Write>(
                val: &[f32],
                writer: &mut W,
            ) -> ::core::result::Result<(), ::borsh::io::Error> {
                for v in val {
                    writer.write_all(&v.to_le_bytes())?;
                }

                ::core::result::Result::Ok(())
            }

            fn borsh_deserialize_fixed_array_f64_permissive<const N: usize, R: ::borsh::io::Read>(
                reader: &mut R,
            ) -> ::core::result::Result<Vec<f64>, ::borsh::io::Error> {
                let mut result = Vec::with_capacity(N);

                for _ in 0..N {
                    let mut buf = [0u8; 8];

                    reader.read_exact(&mut buf)?;

                    result.push(f64::from_le_bytes(buf));
                }

                ::core::result::Result::Ok(result)
            }

            fn borsh_serialize_fixed_array_f64_permissive<const N: usize, W: ::borsh::io::Write>(
                val: &[f64],
                writer: &mut W,
            ) -> ::core::result::Result<(), ::borsh::io::Error> {
                for v in val {
                    writer.write_all(&v.to_le_bytes())?;
                }

                ::core::result::Result::Ok(())
            }

            /// Borsh: deserialize f32 permissively (allows NaN/Infinity).
            /// Standard borsh rejects NaN for portability, but on-chain data may contain them.
            fn borsh_deserialize_f32_permissive<R: ::borsh::io::Read>(
                reader: &mut R,
            ) -> ::core::result::Result<f32, ::borsh::io::Error> {
                let mut buf = [0u8; 4];

                reader.read_exact(&mut buf)?;

                ::core::result::Result::Ok(f32::from_le_bytes(buf))
            }

            fn borsh_serialize_f32_permissive<W: ::borsh::io::Write>(
                val: &f32,
                writer: &mut W,
            ) -> ::core::result::Result<(), ::borsh::io::Error> {
                writer.write_all(&val.to_le_bytes())
            }

            fn borsh_deserialize_opt_f32_permissive<R: ::borsh::io::Read>(
                reader: &mut R,
            ) -> ::core::result::Result<::core::option::Option<f32>, ::borsh::io::Error> {
                let tag: u8 = ::borsh::BorshDeserialize::deserialize_reader(reader)?;

                match tag {
                    0 => ::core::result::Result::Ok(::core::option::Option::None),
                    1 => {
                        let mut buf = [0u8; 4];

                        reader.read_exact(&mut buf)?;

                        ::core::result::Result::Ok(::core::option::Option::Some(f32::from_le_bytes(buf)))
                    }
                    _ => ::core::result::Result::Err(::borsh::io::Error::new(
                        ::borsh::io::ErrorKind::InvalidData,
                        "invalid option tag for f32",
                    )),
                }
            }

            fn borsh_serialize_opt_f32_permissive<W: ::borsh::io::Write>(
                val: &::core::option::Option<f32>,
                writer: &mut W,
            ) -> ::core::result::Result<(), ::borsh::io::Error> {
                match val {
                    ::core::option::Option::Some(v) => {
                        ::borsh::BorshSerialize::serialize(&1u8, writer)?;
                        writer.write_all(&v.to_le_bytes())
                    }
                    ::core::option::Option::None => {
                        ::borsh::BorshSerialize::serialize(&0u8, writer)
                    }
                }
            }

            fn borsh_deserialize_vec_f32_permissive<R: ::borsh::io::Read>(
                reader: &mut R,
            ) -> ::core::result::Result<Vec<f32>, ::borsh::io::Error> {
                let len: u32 = ::borsh::BorshDeserialize::deserialize_reader(reader)?;

                let mut result = Vec::with_capacity(len as usize);

                for _ in 0..len {
                    let mut buf = [0u8; 4];

                    reader.read_exact(&mut buf)?;

                    result.push(f32::from_le_bytes(buf));
                }

                ::core::result::Result::Ok(result)
            }

            fn borsh_serialize_vec_f32_permissive<W: ::borsh::io::Write>(
                val: &[f32],
                writer: &mut W,
            ) -> ::core::result::Result<(), ::borsh::io::Error> {
                ::borsh::BorshSerialize::serialize(&(val.len() as u32), writer)?;

                for v in val {
                    writer.write_all(&v.to_le_bytes())?;
                }

                ::core::result::Result::Ok(())
            }

            /// Borsh: deserialize f64 permissively (allows NaN/Infinity).
            fn borsh_deserialize_f64_permissive<R: ::borsh::io::Read>(
                reader: &mut R,
            ) -> ::core::result::Result<f64, ::borsh::io::Error> {
                let mut buf = [0u8; 8];

                reader.read_exact(&mut buf)?;

                ::core::result::Result::Ok(f64::from_le_bytes(buf))
            }

            fn borsh_serialize_f64_permissive<W: ::borsh::io::Write>(
                val: &f64,
                writer: &mut W,
            ) -> ::core::result::Result<(), ::borsh::io::Error> {
                writer.write_all(&val.to_le_bytes())
            }

            fn borsh_deserialize_opt_f64_permissive<R: ::borsh::io::Read>(
                reader: &mut R,
            ) -> ::core::result::Result<::core::option::Option<f64>, ::borsh::io::Error> {
                let tag: u8 = ::borsh::BorshDeserialize::deserialize_reader(reader)?;

                match tag {
                    0 => ::core::result::Result::Ok(::core::option::Option::None),
                    1 => {
                        let mut buf = [0u8; 8];

                        reader.read_exact(&mut buf)?;

                        ::core::result::Result::Ok(::core::option::Option::Some(f64::from_le_bytes(buf)))
                    }
                    _ => ::core::result::Result::Err(::borsh::io::Error::new(
                        ::borsh::io::ErrorKind::InvalidData,
                        "invalid option tag for f64",
                    )),
                }
            }

            fn borsh_serialize_opt_f64_permissive<W: ::borsh::io::Write>(
                val: &::core::option::Option<f64>,
                writer: &mut W,
            ) -> ::core::result::Result<(), ::borsh::io::Error> {
                match val {
                    ::core::option::Option::Some(v) => {
                        ::borsh::BorshSerialize::serialize(&1u8, writer)?;

                        writer.write_all(&v.to_le_bytes())
                    }
                    ::core::option::Option::None => {
                        ::borsh::BorshSerialize::serialize(&0u8, writer)
                    }
                }
            }

            fn borsh_deserialize_vec_f64_permissive<R: ::borsh::io::Read>(
                reader: &mut R,
            ) -> ::core::result::Result<Vec<f64>, ::borsh::io::Error> {
                let len: u32 = ::borsh::BorshDeserialize::deserialize_reader(reader)?;

                let mut result = Vec::with_capacity(len as usize);

                for _ in 0..len {
                    let mut buf = [0u8; 8];

                    reader.read_exact(&mut buf)?;
                    result.push(f64::from_le_bytes(buf));
                }

                ::core::result::Result::Ok(result)
            }

            fn borsh_serialize_vec_f64_permissive<W: ::borsh::io::Write>(
                val: &[f64],
                writer: &mut W,
            ) -> ::core::result::Result<(), ::borsh::io::Error> {
                ::borsh::BorshSerialize::serialize(&(val.len() as u32), writer)?;

                for v in val {
                    writer.write_all(&v.to_le_bytes())?;
                }

                ::core::result::Result::Ok(())
            }

            pub const PROGRAM_ID: [u8; 32] = #program_pubkey;

            #proto_schema

            #schema_types
            #account_parser
            #instruction_parser
            #event_parser
        }
    }
}

fn option_borsh_helpers() -> TokenStream {
    quote! {
        fn borsh_deserialize_option_prefix<
            const PREFIX_BYTES: usize,
            const PREFIX_ONE: u128,
            const BIG_ENDIAN: bool,
            R: ::borsh::io::Read,
        >(
            reader: &mut R,
        ) -> ::core::result::Result<bool, ::borsh::io::Error> {
            let mut bytes = [0u8; PREFIX_BYTES];
            reader.read_exact(&mut bytes)?;

            let mut value = 0u128;
            for (index, byte) in bytes.iter().enumerate() {
                let shift = if BIG_ENDIAN {
                    (PREFIX_BYTES - index - 1) * 8
                } else {
                    index * 8
                };
                value |= u128::from(*byte) << shift;
            }

            if value == 0 {
                ::core::result::Result::Ok(false)
            } else if value == PREFIX_ONE {
                ::core::result::Result::Ok(true)
            } else {
                ::core::result::Result::Err(::borsh::io::Error::new(
                    ::borsh::io::ErrorKind::InvalidData,
                    "invalid Codama option prefix",
                ))
            }
        }

        fn borsh_serialize_option_prefix<
            const PREFIX_BYTES: usize,
            const PREFIX_ONE: u128,
            const BIG_ENDIAN: bool,
            W: ::borsh::io::Write,
        >(
            is_some: bool,
            writer: &mut W,
        ) -> ::core::result::Result<(), ::borsh::io::Error> {
            let value = if is_some { PREFIX_ONE } else { 0 };
            let mut bytes = [0u8; PREFIX_BYTES];

            for (index, byte) in bytes.iter_mut().enumerate() {
                let shift = if BIG_ENDIAN {
                    (PREFIX_BYTES - index - 1) * 8
                } else {
                    index * 8
                };
                *byte = ((value >> shift) & 0xff) as u8;
            }

            writer.write_all(&bytes)
        }

        fn borsh_deserialize_option_none_padding<
            const NONE_PADDING: usize,
            R: ::borsh::io::Read,
        >(
            reader: &mut R,
        ) -> ::core::result::Result<(), ::borsh::io::Error> {
            let mut padding = ::std::vec![0u8; NONE_PADDING];
            reader.read_exact(&mut padding)?;

            if padding.iter().all(|byte| *byte == 0) {
                ::core::result::Result::Ok(())
            } else {
                ::core::result::Result::Err(::borsh::io::Error::new(
                    ::borsh::io::ErrorKind::InvalidData,
                    "fixed Codama option padding must be zero",
                ))
            }
        }

        fn borsh_serialize_option_none_padding<
            const NONE_PADDING: usize,
            W: ::borsh::io::Write,
        >(
            writer: &mut W,
        ) -> ::core::result::Result<(), ::borsh::io::Error> {
            writer.write_all(&::std::vec![0u8; NONE_PADDING])
        }

        fn borsh_deserialize_option<
            T: ::borsh::BorshDeserialize,
            const PREFIX_BYTES: usize,
            const PREFIX_ONE: u128,
            const BIG_ENDIAN: bool,
            const NONE_PADDING: usize,
            R: ::borsh::io::Read,
        >(
            reader: &mut R,
        ) -> ::core::result::Result<::core::option::Option<T>, ::borsh::io::Error> {
            if borsh_deserialize_option_prefix::<PREFIX_BYTES, PREFIX_ONE, BIG_ENDIAN, _>(reader)? {
                ::core::result::Result::Ok(::core::option::Option::Some(
                    <T as ::borsh::BorshDeserialize>::deserialize_reader(reader)?,
                ))
            } else {
                borsh_deserialize_option_none_padding::<NONE_PADDING, _>(reader)?;
                ::core::result::Result::Ok(::core::option::Option::None)
            }
        }

        fn borsh_serialize_option<
            T: ::borsh::BorshSerialize,
            const PREFIX_BYTES: usize,
            const PREFIX_ONE: u128,
            const BIG_ENDIAN: bool,
            const NONE_PADDING: usize,
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
                    <T as ::borsh::BorshSerialize>::serialize(value, writer)
                }
                ::core::option::Option::None => {
                    borsh_serialize_option_none_padding::<NONE_PADDING, _>(writer)
                }
            }
        }

        fn borsh_deserialize_option_fixed_bytes<
            const BYTE_SIZE: usize,
            const PREFIX_BYTES: usize,
            const PREFIX_ONE: u128,
            const BIG_ENDIAN: bool,
            const NONE_PADDING: usize,
            R: ::borsh::io::Read,
        >(
            reader: &mut R,
        ) -> ::core::result::Result<::core::option::Option<Vec<u8>>, ::borsh::io::Error> {
            if borsh_deserialize_option_prefix::<PREFIX_BYTES, PREFIX_ONE, BIG_ENDIAN, _>(reader)? {
                let mut bytes = [0u8; BYTE_SIZE];
                reader.read_exact(&mut bytes)?;
                ::core::result::Result::Ok(::core::option::Option::Some(bytes.to_vec()))
            } else {
                borsh_deserialize_option_none_padding::<NONE_PADDING, _>(reader)?;
                ::core::result::Result::Ok(::core::option::Option::None)
            }
        }

        fn borsh_serialize_option_fixed_bytes<
            const BYTE_SIZE: usize,
            const PREFIX_BYTES: usize,
            const PREFIX_ONE: u128,
            const BIG_ENDIAN: bool,
            const NONE_PADDING: usize,
            W: ::borsh::io::Write,
        >(
            value: &::core::option::Option<Vec<u8>>,
            writer: &mut W,
        ) -> ::core::result::Result<(), ::borsh::io::Error> {
            borsh_serialize_option_prefix::<PREFIX_BYTES, PREFIX_ONE, BIG_ENDIAN, _>(
                value.is_some(),
                writer,
            )?;

            match value {
                ::core::option::Option::Some(value) if value.len() == BYTE_SIZE => {
                    writer.write_all(value)
                }
                ::core::option::Option::Some(_) => ::core::result::Result::Err(
                    ::borsh::io::Error::new(
                        ::borsh::io::ErrorKind::InvalidInput,
                        "invalid fixed-byte option length",
                    ),
                ),
                ::core::option::Option::None => {
                    borsh_serialize_option_none_padding::<NONE_PADDING, _>(writer)
                }
            }
        }

        fn borsh_deserialize_option_f32_permissive<
            const PREFIX_BYTES: usize,
            const PREFIX_ONE: u128,
            const BIG_ENDIAN: bool,
            const NONE_PADDING: usize,
            R: ::borsh::io::Read,
        >(
            reader: &mut R,
        ) -> ::core::result::Result<::core::option::Option<f32>, ::borsh::io::Error> {
            if borsh_deserialize_option_prefix::<PREFIX_BYTES, PREFIX_ONE, BIG_ENDIAN, _>(reader)? {
                let mut bytes = [0u8; 4];
                reader.read_exact(&mut bytes)?;
                ::core::result::Result::Ok(::core::option::Option::Some(f32::from_le_bytes(bytes)))
            } else {
                borsh_deserialize_option_none_padding::<NONE_PADDING, _>(reader)?;
                ::core::result::Result::Ok(::core::option::Option::None)
            }
        }

        fn borsh_serialize_option_f32_permissive<
            const PREFIX_BYTES: usize,
            const PREFIX_ONE: u128,
            const BIG_ENDIAN: bool,
            const NONE_PADDING: usize,
            W: ::borsh::io::Write,
        >(
            value: &::core::option::Option<f32>,
            writer: &mut W,
        ) -> ::core::result::Result<(), ::borsh::io::Error> {
            borsh_serialize_option_prefix::<PREFIX_BYTES, PREFIX_ONE, BIG_ENDIAN, _>(
                value.is_some(),
                writer,
            )?;

            match value {
                ::core::option::Option::Some(value) => writer.write_all(&value.to_le_bytes()),
                ::core::option::Option::None => {
                    borsh_serialize_option_none_padding::<NONE_PADDING, _>(writer)
                }
            }
        }

        fn borsh_deserialize_option_f64_permissive<
            const PREFIX_BYTES: usize,
            const PREFIX_ONE: u128,
            const BIG_ENDIAN: bool,
            const NONE_PADDING: usize,
            R: ::borsh::io::Read,
        >(
            reader: &mut R,
        ) -> ::core::result::Result<::core::option::Option<f64>, ::borsh::io::Error> {
            if borsh_deserialize_option_prefix::<PREFIX_BYTES, PREFIX_ONE, BIG_ENDIAN, _>(reader)? {
                let mut bytes = [0u8; 8];
                reader.read_exact(&mut bytes)?;
                ::core::result::Result::Ok(::core::option::Option::Some(f64::from_le_bytes(bytes)))
            } else {
                borsh_deserialize_option_none_padding::<NONE_PADDING, _>(reader)?;
                ::core::result::Result::Ok(::core::option::Option::None)
            }
        }

        fn borsh_serialize_option_f64_permissive<
            const PREFIX_BYTES: usize,
            const PREFIX_ONE: u128,
            const BIG_ENDIAN: bool,
            const NONE_PADDING: usize,
            W: ::borsh::io::Write,
        >(
            value: &::core::option::Option<f64>,
            writer: &mut W,
        ) -> ::core::result::Result<(), ::borsh::io::Error> {
            borsh_serialize_option_prefix::<PREFIX_BYTES, PREFIX_ONE, BIG_ENDIAN, _>(
                value.is_some(),
                writer,
            )?;

            match value {
                ::core::option::Option::Some(value) => writer.write_all(&value.to_le_bytes()),
                ::core::option::Option::None => {
                    borsh_serialize_option_none_padding::<NONE_PADDING, _>(writer)
                }
            }
        }

        fn borsh_deserialize_short_u16_option_prefix<R: ::borsh::io::Read>(
            reader: &mut R,
        ) -> ::core::result::Result<bool, ::borsh::io::Error> {
            let mut byte = [0u8; 1];
            reader.read_exact(&mut byte)?;

            match byte[0] {
                0 => ::core::result::Result::Ok(false),
                1 => ::core::result::Result::Ok(true),
                _ => ::core::result::Result::Err(::borsh::io::Error::new(
                    ::borsh::io::ErrorKind::InvalidData,
                    "invalid short_u16 Codama option prefix",
                )),
            }
        }

        fn borsh_serialize_short_u16_option_prefix<W: ::borsh::io::Write>(
            is_some: bool,
            writer: &mut W,
        ) -> ::core::result::Result<(), ::borsh::io::Error> {
            writer.write_all(&[u8::from(is_some)])
        }

        fn borsh_deserialize_short_u16_option<
            T: ::borsh::BorshDeserialize,
            const NONE_PADDING: usize,
            R: ::borsh::io::Read,
        >(
            reader: &mut R,
        ) -> ::core::result::Result<::core::option::Option<T>, ::borsh::io::Error> {
            if borsh_deserialize_short_u16_option_prefix(reader)? {
                ::core::result::Result::Ok(::core::option::Option::Some(
                    <T as ::borsh::BorshDeserialize>::deserialize_reader(reader)?,
                ))
            } else {
                borsh_deserialize_option_none_padding::<NONE_PADDING, _>(reader)?;
                ::core::result::Result::Ok(::core::option::Option::None)
            }
        }

        fn borsh_serialize_short_u16_option<
            T: ::borsh::BorshSerialize,
            const NONE_PADDING: usize,
            W: ::borsh::io::Write,
        >(
            value: &::core::option::Option<T>,
            writer: &mut W,
        ) -> ::core::result::Result<(), ::borsh::io::Error> {
            borsh_serialize_short_u16_option_prefix(value.is_some(), writer)?;

            match value {
                ::core::option::Option::Some(value) => {
                    <T as ::borsh::BorshSerialize>::serialize(value, writer)
                }
                ::core::option::Option::None => {
                    borsh_serialize_option_none_padding::<NONE_PADDING, _>(writer)
                }
            }
        }

        fn borsh_deserialize_short_u16_option_fixed_bytes<
            const BYTE_SIZE: usize,
            const NONE_PADDING: usize,
            R: ::borsh::io::Read,
        >(
            reader: &mut R,
        ) -> ::core::result::Result<::core::option::Option<Vec<u8>>, ::borsh::io::Error> {
            if borsh_deserialize_short_u16_option_prefix(reader)? {
                let mut bytes = [0u8; BYTE_SIZE];
                reader.read_exact(&mut bytes)?;
                ::core::result::Result::Ok(::core::option::Option::Some(bytes.to_vec()))
            } else {
                borsh_deserialize_option_none_padding::<NONE_PADDING, _>(reader)?;
                ::core::result::Result::Ok(::core::option::Option::None)
            }
        }

        fn borsh_serialize_short_u16_option_fixed_bytes<
            const BYTE_SIZE: usize,
            const NONE_PADDING: usize,
            W: ::borsh::io::Write,
        >(
            value: &::core::option::Option<Vec<u8>>,
            writer: &mut W,
        ) -> ::core::result::Result<(), ::borsh::io::Error> {
            borsh_serialize_short_u16_option_prefix(value.is_some(), writer)?;

            match value {
                ::core::option::Option::Some(value) if value.len() == BYTE_SIZE => {
                    writer.write_all(value)
                }
                ::core::option::Option::Some(_) => ::core::result::Result::Err(
                    ::borsh::io::Error::new(
                        ::borsh::io::ErrorKind::InvalidInput,
                        "invalid fixed-byte option length",
                    ),
                ),
                ::core::option::Option::None => {
                    borsh_serialize_option_none_padding::<NONE_PADDING, _>(writer)
                }
            }
        }

        fn borsh_deserialize_short_u16_option_f32_permissive<
            const NONE_PADDING: usize,
            R: ::borsh::io::Read,
        >(
            reader: &mut R,
        ) -> ::core::result::Result<::core::option::Option<f32>, ::borsh::io::Error> {
            if borsh_deserialize_short_u16_option_prefix(reader)? {
                let mut bytes = [0u8; 4];
                reader.read_exact(&mut bytes)?;
                ::core::result::Result::Ok(::core::option::Option::Some(f32::from_le_bytes(bytes)))
            } else {
                borsh_deserialize_option_none_padding::<NONE_PADDING, _>(reader)?;
                ::core::result::Result::Ok(::core::option::Option::None)
            }
        }

        fn borsh_serialize_short_u16_option_f32_permissive<
            const NONE_PADDING: usize,
            W: ::borsh::io::Write,
        >(
            value: &::core::option::Option<f32>,
            writer: &mut W,
        ) -> ::core::result::Result<(), ::borsh::io::Error> {
            borsh_serialize_short_u16_option_prefix(value.is_some(), writer)?;

            match value {
                ::core::option::Option::Some(value) => writer.write_all(&value.to_le_bytes()),
                ::core::option::Option::None => {
                    borsh_serialize_option_none_padding::<NONE_PADDING, _>(writer)
                }
            }
        }

        fn borsh_deserialize_short_u16_option_f64_permissive<
            const NONE_PADDING: usize,
            R: ::borsh::io::Read,
        >(
            reader: &mut R,
        ) -> ::core::result::Result<::core::option::Option<f64>, ::borsh::io::Error> {
            if borsh_deserialize_short_u16_option_prefix(reader)? {
                let mut bytes = [0u8; 8];
                reader.read_exact(&mut bytes)?;
                ::core::result::Result::Ok(::core::option::Option::Some(f64::from_le_bytes(bytes)))
            } else {
                borsh_deserialize_option_none_padding::<NONE_PADDING, _>(reader)?;
                ::core::result::Result::Ok(::core::option::Option::None)
            }
        }

        fn borsh_serialize_short_u16_option_f64_permissive<
            const NONE_PADDING: usize,
            W: ::borsh::io::Write,
        >(
            value: &::core::option::Option<f64>,
            writer: &mut W,
        ) -> ::core::result::Result<(), ::borsh::io::Error> {
            borsh_serialize_short_u16_option_prefix(value.is_some(), writer)?;

            match value {
                ::core::option::Option::Some(value) => writer.write_all(&value.to_le_bytes()),
                ::core::option::Option::None => {
                    borsh_serialize_option_none_padding::<NONE_PADDING, _>(writer)
                }
            }
        }
    }
}
