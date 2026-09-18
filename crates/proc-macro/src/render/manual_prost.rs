use std::collections::HashSet;

use proc_macro2::TokenStream;
use quote::{format_ident, quote};

use super::rust_types_from_ir::map_ir_type_to_native;
use crate::intermediate_representation::{
    FieldIr, FieldTypeIr, LabelIr, OneofIr, ScalarIr, TypeIr,
};

///
/// Generate a manual `prost::Message` impl for a oneof wrapper struct.
///
/// The struct has a single non-Option field (`field_ident`) of type `mod_ident::oneof_ident`
/// which derives `prost::Oneof`. We implement Message by delegating to the Oneof methods
/// (calling Oneof generated methods via `self.field_ident`).
///
pub(super) fn manual_prost_message_impl(
    parent_ident: &syn::Ident,
    field_ident: &syn::Ident,
    mod_ident: &syn::Ident,
    oneof_ident: &syn::Ident,
) -> TokenStream {
    quote! {
        impl ::core::fmt::Debug for #parent_ident {
            fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
                f.debug_struct(stringify!(#parent_ident))
                    .field(stringify!(#field_ident), &self.#field_ident)
                    .finish()
            }
        }

        impl ::prost::Message for #parent_ident {
            fn encode_raw(&self, buf: &mut impl ::prost::bytes::BufMut) {
                self.#field_ident.encode(buf);
            }

            fn merge_field(
                &mut self,
                tag: u32,
                wire_type: ::prost::encoding::WireType,
                buf: &mut impl ::prost::bytes::Buf,
                ctx: ::prost::encoding::DecodeContext,
            ) -> ::core::result::Result<(), ::prost::DecodeError> {
                // Oneof::merge() requires `&mut Option<Self>`, so we wrap our non-Option
                // field into Some, call merge, then unwrap back.
                let mut opt = ::core::option::Option::Some(self.#field_ident.clone());

                #mod_ident::#oneof_ident::merge(&mut opt, tag, wire_type, buf, ctx)?;

                if let ::core::option::Option::Some(v) = opt {
                    self.#field_ident = v;
                }

                ::core::result::Result::Ok(())
            }

            fn encoded_len(&self) -> usize {
                self.#field_ident.encoded_len()
            }

            fn clear(&mut self) {}
        }
    }
}

///
/// Generate a manual `prost::Oneof` impl for the `Instruction` enum with
/// struct variants (`Swap { accounts, args }` instead of `Swap(Swap)`).
///
/// Each variant encodes/decodes as a nested message with `accounts` at tag 1
/// and `args` at tag 2 — identical wire format to the wrapper struct approach.
///
pub(super) fn manual_prost_oneof_impl(
    oneof_ir: &OneofIr,
    mod_ident: &syn::Ident,
    oneof_ident: &syn::Ident,
) -> TokenStream {
    let encode_arms: Vec<TokenStream> = oneof_ir
        .variants
        .iter()
        .map(|v| {
            let v_ident = format_ident!("{}", v.variant_name);
            let tag = v.tag;

            quote! {
                #mod_ident::#oneof_ident::#v_ident { accounts, args } => {
                    let body_len =
                        ::prost::encoding::message::encoded_len(1, accounts)
                        + ::prost::encoding::message::encoded_len(2, args);

                    ::prost::encoding::encode_key(
                        #tag,
                        ::prost::encoding::WireType::LengthDelimited,
                        buf,
                    );
                    ::prost::encoding::encode_varint(body_len as u64, buf);
                    ::prost::encoding::message::encode(1, accounts, buf);
                    ::prost::encoding::message::encode(2, args, buf);
                }
            }
        })
        .collect();

    let merge_arms: Vec<TokenStream> = oneof_ir
        .variants
        .iter()
        .map(|v| {
            let v_ident = format_ident!("{}", v.variant_name);
            let msg_ident = format_ident!("{}", v.message_type);
            let tag = v.tag;

            quote! {
                #tag => {
                    let mut wrapper: #mod_ident::#msg_ident = match field.take() {
                        ::core::option::Option::Some(
                            #mod_ident::#oneof_ident::#v_ident { accounts, args }
                        ) => #mod_ident::#msg_ident { accounts, args },

                        _ => ::core::default::Default::default(),
                    };

                    ::prost::encoding::message::merge(wire_type, &mut wrapper, buf, ctx)?;

                    *field = ::core::option::Option::Some(
                        #mod_ident::#oneof_ident::#v_ident {
                            accounts: wrapper.accounts,
                            args: wrapper.args,
                        }
                    );

                    ::core::result::Result::Ok(())
                }
            }
        })
        .collect();

    let encoded_len_arms: Vec<TokenStream> = oneof_ir
        .variants
        .iter()
        .map(|v| {
            let v_ident = format_ident!("{}", v.variant_name);
            let tag = v.tag;

            quote! {
                #mod_ident::#oneof_ident::#v_ident { accounts, args } => {
                    let body_len =
                        ::prost::encoding::message::encoded_len(1, accounts)
                        + ::prost::encoding::message::encoded_len(2, args);

                    ::prost::encoding::key_len(#tag)
                        + ::prost::encoding::encoded_len_varint(body_len as u64)
                        + body_len
                }
            }
        })
        .collect();

    quote! {
        impl #mod_ident::#oneof_ident {
            pub fn encode(&self, buf: &mut impl ::prost::bytes::BufMut) {
                match self {
                    #(#encode_arms,)*
                }
            }

            pub fn merge(
                field: &mut ::core::option::Option<Self>,
                tag: u32,
                wire_type: ::prost::encoding::WireType,
                buf: &mut impl ::prost::bytes::Buf,
                ctx: ::prost::encoding::DecodeContext,
            ) -> ::core::result::Result<(), ::prost::DecodeError> {
                match tag {
                    #(#merge_arms,)*
                    _ => unreachable!(concat!("invalid ", stringify!(#oneof_ident), " tag: {}"), tag),
                }
            }

            #[inline]
            pub fn encoded_len(&self) -> usize {
                match self {
                    #(#encoded_len_arms,)*
                }
            }
        }
    }
}

/// Whether a scalar requires widening for proto encoding (on-chain type differs from proto type).
fn needs_widening(scalar: &ScalarIr) -> bool {
    matches!(
        scalar,
        ScalarIr::U8 | ScalarIr::U16 | ScalarIr::ShortU16 | ScalarIr::I8 | ScalarIr::I16
    )
}

/// The proto-compatible widened type for a scalar that needs widening.
fn widened_type(scalar: &ScalarIr) -> TokenStream {
    match scalar {
        ScalarIr::U8 | ScalarIr::U16 | ScalarIr::ShortU16 => quote!(u32),
        ScalarIr::I8 | ScalarIr::I16 => quote!(i32),
        _ => unreachable!("widened_type called on non-widened scalar"),
    }
}

/// Whether a scalar needs bytes-to-native-int conversion for proto encoding (u128/i128).
fn needs_bytes_conversion(scalar: &ScalarIr) -> bool {
    matches!(scalar, ScalarIr::U128 | ScalarIr::I128)
}

/// The native integer type for a scalar that needs bytes conversion.
fn bytes_native_type(scalar: &ScalarIr) -> TokenStream {
    match scalar {
        ScalarIr::U128 => quote!(u128),
        ScalarIr::I128 => quote!(i128),
        _ => unreachable!("bytes_native_type called on non-bytes-conversion scalar"),
    }
}

/// Whether a scalar is a public key that needs Pubkey ↔ PublicKeyProtoWrapper conversion for proto.
fn is_pubkey_scalar(scalar: &ScalarIr) -> bool { matches!(scalar, ScalarIr::PublicKey) }

/// Return the `prost::encoding` module path for a scalar type.
fn prost_encoding_mod(scalar: &ScalarIr) -> TokenStream {
    match scalar {
        ScalarIr::Bool => quote!(::prost::encoding::bool),
        ScalarIr::U8 | ScalarIr::U16 | ScalarIr::ShortU16 | ScalarIr::Uint32 => {
            quote!(::prost::encoding::uint32)
        },
        ScalarIr::Uint64 => quote!(::prost::encoding::uint64),
        ScalarIr::I8 | ScalarIr::I16 | ScalarIr::Int32 => quote!(::prost::encoding::int32),
        ScalarIr::Int64 => quote!(::prost::encoding::int64),
        ScalarIr::Float => quote!(::prost::encoding::float),
        ScalarIr::Double => quote!(::prost::encoding::double),
        ScalarIr::String | ScalarIr::SizePrefixedString { .. } => {
            quote!(::prost::encoding::string)
        },
        ScalarIr::Bytes
        | ScalarIr::SizePrefixedBytes { .. }
        | ScalarIr::FixedBytes(_)
        | ScalarIr::U128
        | ScalarIr::I128 => {
            quote!(::prost::encoding::bytes)
        },
        ScalarIr::PublicKey => quote!(::prost::encoding::message),
    }
}

/// Generate a manual `prost::Message` impl for a regular struct (not oneof wrappers).
///
/// This replaces `derive(prost::Message)` so that struct fields can use native Rust types
/// (e.g. `u8` instead of `u32`) while encoding/decoding as the widened proto type on the wire.
pub(super) fn manual_prost_struct_impl(
    t: &TypeIr,
    local_names: Option<&HashSet<&str>>,
) -> TokenStream {
    let ident = format_ident!("{}", t.name);
    let struct_name = &t.name;
    let in_module = local_names.is_some();

    let resolve_msg = |msg: &str| -> TokenStream {
        let msg_ident = format_ident!("{}", msg);

        if let Some(locals) = local_names {
            if locals.contains(msg) {
                quote!(#msg_ident)
            } else {
                quote!(super::#msg_ident)
            }
        } else {
            quote!(#msg_ident)
        }
    };

    let mut encode_stmts = Vec::new();
    let mut merge_arms = Vec::new();
    let mut encoded_len_stmts = Vec::new();
    let mut clear_stmts = Vec::new();
    let mut default_stmts = Vec::new();

    for f in &t.fields {
        emit_prost_field_codegen(
            f,
            in_module,
            &resolve_msg,
            &mut encode_stmts,
            &mut merge_arms,
            &mut encoded_len_stmts,
            &mut clear_stmts,
            &mut default_stmts,
        );
    }

    let debug_field_names: Vec<_> = t
        .fields
        .iter()
        .map(|f| format_ident!("{}", f.name))
        .collect();

    quote! {
        impl ::core::fmt::Debug for #ident {
            fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
                let mut s = f.debug_struct(stringify!(#ident));

                #(s.field(stringify!(#debug_field_names), &self.#debug_field_names);)*

                s.finish()
            }
        }

        impl ::core::default::Default for #ident {
            fn default() -> Self {
                Self {
                    #(#default_stmts),*
                }
            }
        }

        impl ::prost::Message for #ident {
            #[allow(unused_variables)]
            fn encode_raw(&self, buf: &mut impl ::prost::bytes::BufMut) {
                #(#encode_stmts)*
            }

            // DecodeError::new is doc(hidden) + deprecated but explicitly intended
            // for Message implementations, which is exactly our use case.
            #[allow(unused_variables, deprecated)]
            fn merge_field(
                &mut self,
                tag: u32,
                wire_type: ::prost::encoding::WireType,
                buf: &mut impl ::prost::bytes::Buf,
                ctx: ::prost::encoding::DecodeContext,
            ) -> ::core::result::Result<(), ::prost::DecodeError> {
                const STRUCT_NAME: &str = #struct_name;

                match tag {
                    #(#merge_arms,)*
                    _ => ::prost::encoding::skip_field(wire_type, tag, buf, ctx),
                }
            }

            fn encoded_len(&self) -> usize {
                0 #(#encoded_len_stmts)*
            }

            fn clear(&mut self) {
                #(#clear_stmts)*
            }
        }
    }
}

/// Build encode/merge/encoded_len/clear/default statements for a single field.
#[allow(clippy::too_many_arguments)]
fn emit_prost_field_codegen(
    f: &FieldIr,
    in_module: bool,
    resolve_msg: &dyn Fn(&str) -> TokenStream,
    encode_stmts: &mut Vec<TokenStream>,
    merge_arms: &mut Vec<TokenStream>,
    encoded_len_stmts: &mut Vec<TokenStream>,
    clear_stmts: &mut Vec<TokenStream>,
    default_stmts: &mut Vec<TokenStream>,
) {
    let fname = format_ident!("{}", f.name);
    let field_name_str = &f.name;
    let tag = f.tag;
    let is_message = matches!(&f.field_type, FieldTypeIr::Message(_));

    match (&f.label, &f.field_type) {
        // --- Singular message ---
        (LabelIr::Singular, _) if is_message => {
            encode_stmts.push(quote! {
                ::prost::encoding::message::encode(#tag, &self.#fname, buf);
            });

            merge_arms.push(quote! {
                #tag => {
                    ::prost::encoding::message::merge(wire_type, &mut self.#fname, buf, ctx)
                        .map_err(|mut error| { error.push(STRUCT_NAME, #field_name_str); error })
                }
            });

            encoded_len_stmts.push(quote! {
                + ::prost::encoding::message::encoded_len(#tag, &self.#fname)
            });

            clear_stmts.push(quote! { self.#fname.clear(); });

            let msg_ty = match &f.field_type {
                FieldTypeIr::Message(msg) => resolve_msg(msg),
                _ => unreachable!(),
            };

            default_stmts.push(quote! {
                #fname: <#msg_ty as ::core::default::Default>::default()
            });
        },

        // --- Singular pubkey (Pubkey ↔ PublicKeyProtoWrapper conversion) ---
        (LabelIr::Singular, FieldTypeIr::Scalar(s)) if is_pubkey_scalar(s) => {
            let pubkey_ty = map_ir_type_to_native(&f.field_type, in_module);
            let wrapper_ty = quote!(shipstern_core::PublicKeyProtoWrapper);

            encode_stmts.push(quote! {
                {
                    let wrapper = #wrapper_ty::new(self.#fname.0.to_vec());

                    ::prost::encoding::message::encode(#tag, &wrapper, buf);
                }
            });

            merge_arms.push(quote! {
                #tag => {
                    let mut wrapper = #wrapper_ty::default();

                    ::prost::encoding::message::merge(wire_type, &mut wrapper, buf, ctx)
                        .map_err(|mut error| { error.push(STRUCT_NAME, #field_name_str); error })?;

                    let arr: [u8; 32] = wrapper.value.try_into().map_err(|_|
                        ::prost::DecodeError::new("expected exactly 32 bytes for Pubkey")
                    )?;

                    self.#fname = #pubkey_ty::new(arr);

                    ::core::result::Result::Ok(())
                }
            });

            encoded_len_stmts.push(quote! {
                + {
                    let wrapper = #wrapper_ty::new(self.#fname.0.to_vec());

                    ::prost::encoding::message::encoded_len(#tag, &wrapper)
                }
            });

            clear_stmts.push(
                quote! { self.#fname = <#pubkey_ty as ::core::default::Default>::default(); },
            );
            default_stmts
                .push(quote! { #fname: <#pubkey_ty as ::core::default::Default>::default() });
        },

        // --- Singular scalar (widened) ---
        (LabelIr::Singular, FieldTypeIr::Scalar(s)) if needs_widening(s) => {
            let enc_mod = prost_encoding_mod(s);
            let wide_ty = widened_type(s);
            let native_ty = map_ir_type_to_native(&f.field_type, in_module);

            encode_stmts.push(quote! {
                if self.#fname != (0 as #native_ty) {
                    let tmp = self.#fname as #wide_ty;
                    #enc_mod::encode(#tag, &tmp, buf);
                }
            });

            merge_arms.push(quote! {
                #tag => {
                    let mut tmp = self.#fname as #wide_ty;

                    #enc_mod::merge(wire_type, &mut tmp, buf, ctx)
                        .map_err(|mut error| { error.push(STRUCT_NAME, #field_name_str); error })?;

                    self.#fname = tmp as #native_ty;

                    ::core::result::Result::Ok(())
                }
            });

            encoded_len_stmts.push(quote! {
                + if self.#fname != (0 as #native_ty) {
                    #enc_mod::encoded_len(#tag, &(self.#fname as #wide_ty))
                } else { 0 }
            });

            clear_stmts.push(quote! { self.#fname = 0; });
            default_stmts.push(quote! { #fname: 0 });
        },

        // --- Singular scalar (bytes conversion: u128/i128) ---
        (LabelIr::Singular, FieldTypeIr::Scalar(s)) if needs_bytes_conversion(s) => {
            let native_ty = bytes_native_type(s);

            encode_stmts.push(quote! {
                if self.#fname != (0 as #native_ty) {
                    let tmp: Vec<u8> = self.#fname.to_le_bytes().to_vec();

                    ::prost::encoding::bytes::encode(#tag, &tmp, buf);
                }
            });

            merge_arms.push(quote! {
                #tag => {
                    let mut tmp: Vec<u8> = Vec::new();

                    ::prost::encoding::bytes::merge(wire_type, &mut tmp, buf, ctx)
                        .map_err(|mut error| { error.push(STRUCT_NAME, #field_name_str); error })?;

                    let arr: [u8; 16] = tmp.try_into().map_err(|_|
                        ::prost::DecodeError::new(
                            concat!("expected exactly 16 bytes for ", stringify!(#native_ty))
                        )
                    )?;

                    self.#fname = #native_ty::from_le_bytes(arr);

                    ::core::result::Result::Ok(())
                }
            });

            encoded_len_stmts.push(quote! {
                + if self.#fname != (0 as #native_ty) {
                    ::prost::encoding::bytes::encoded_len(#tag, &self.#fname.to_le_bytes().to_vec())
                } else { 0 }
            });

            clear_stmts.push(quote! { self.#fname = 0; });
            default_stmts.push(quote! { #fname: 0 });
        },

        // --- Singular scalar (no widening) ---
        (LabelIr::Singular, FieldTypeIr::Scalar(s)) => {
            let enc_mod = prost_encoding_mod(s);
            let native_ty = map_ir_type_to_native(&f.field_type, in_module);

            let default_check = match s {
                ScalarIr::Bool => quote!(self.#fname != false),
                ScalarIr::String | ScalarIr::SizePrefixedString { .. } => {
                    quote!(!self.#fname.is_empty())
                },
                ScalarIr::Bytes | ScalarIr::SizePrefixedBytes { .. } | ScalarIr::FixedBytes(_) => {
                    quote!(!self.#fname.is_empty())
                },
                _ => quote!(self.#fname != (0 as #native_ty)),
            };

            let default_val = match s {
                ScalarIr::Bool => quote!(false),
                ScalarIr::String | ScalarIr::SizePrefixedString { .. } => quote!(String::new()),
                ScalarIr::Bytes | ScalarIr::SizePrefixedBytes { .. } | ScalarIr::FixedBytes(_) => {
                    quote!(Vec::new())
                },
                ScalarIr::Float => quote!(0f32),
                ScalarIr::Double => quote!(0f64),
                _ => quote!(0),
            };

            let clear_val = match s {
                ScalarIr::Bool => quote!(self.#fname = false;),
                ScalarIr::String | ScalarIr::SizePrefixedString { .. } => {
                    quote!(self.#fname.clear();)
                },
                ScalarIr::Bytes | ScalarIr::SizePrefixedBytes { .. } | ScalarIr::FixedBytes(_) => {
                    quote!(self.#fname.clear();)
                },
                ScalarIr::Float => quote!(self.#fname = 0f32;),
                ScalarIr::Double => quote!(self.#fname = 0f64;),
                _ => quote!(self.#fname = 0;),
            };

            encode_stmts.push(quote! {
                if #default_check {
                    #enc_mod::encode(#tag, &self.#fname, buf);
                }
            });

            merge_arms.push(quote! {
                #tag => {
                    #enc_mod::merge(wire_type, &mut self.#fname, buf, ctx)
                        .map_err(|mut error| { error.push(STRUCT_NAME, #field_name_str); error })
                }
            });

            encoded_len_stmts.push(quote! {
                + if #default_check {
                    #enc_mod::encoded_len(#tag, &self.#fname)
                } else { 0 }
            });

            clear_stmts.push(clear_val);
            default_stmts.push(quote! { #fname: #default_val });
        },

        // --- Optional message ---
        (LabelIr::Optional(_), _) if is_message => {
            encode_stmts.push(quote! {
                if let ::core::option::Option::Some(ref msg) = self.#fname {
                    ::prost::encoding::message::encode(#tag, msg, buf);
                }
            });

            merge_arms.push(quote! {
                #tag => {
                    ::prost::encoding::message::merge(
                        wire_type,
                        self.#fname.get_or_insert_with(::core::default::Default::default),
                        buf, ctx,
                    ).map_err(|mut error| { error.push(STRUCT_NAME, #field_name_str); error })
                }
            });

            encoded_len_stmts.push(quote! {
                + self.#fname.as_ref().map_or(0, |m|
                    ::prost::encoding::message::encoded_len(#tag, m)
                )
            });

            clear_stmts.push(quote! { self.#fname = ::core::option::Option::None; });
            default_stmts.push(quote! { #fname: ::core::option::Option::None });
        },

        // --- Optional pubkey (Pubkey ↔ PublicKeyProtoWrapper conversion) ---
        (LabelIr::Optional(_), FieldTypeIr::Scalar(s)) if is_pubkey_scalar(s) => {
            let pubkey_ty = map_ir_type_to_native(&f.field_type, in_module);
            let wrapper_ty = quote!(shipstern_core::PublicKeyProtoWrapper);

            encode_stmts.push(quote! {
                if let ::core::option::Option::Some(ref pk) = self.#fname {
                    let wrapper = #wrapper_ty::new(pk.0.to_vec());

                    ::prost::encoding::message::encode(#tag, &wrapper, buf);
                }
            });

            merge_arms.push(quote! {
                #tag => {
                    let mut wrapper = #wrapper_ty::default();

                    ::prost::encoding::message::merge(wire_type, &mut wrapper, buf, ctx)
                        .map_err(|mut error| { error.push(STRUCT_NAME, #field_name_str); error })?;

                    let arr: [u8; 32] = wrapper.value.try_into().map_err(|_|
                        ::prost::DecodeError::new("expected exactly 32 bytes for Pubkey")
                    )?;

                    self.#fname = ::core::option::Option::Some(#pubkey_ty::new(arr));

                    ::core::result::Result::Ok(())
                }
            });

            encoded_len_stmts.push(quote! {
                + self.#fname.as_ref().map_or(0, |pk| {
                    let wrapper = #wrapper_ty::new(pk.0.to_vec());

                    ::prost::encoding::message::encoded_len(#tag, &wrapper)
                })
            });

            clear_stmts.push(quote! { self.#fname = ::core::option::Option::None; });
            default_stmts.push(quote! { #fname: ::core::option::Option::None });
        },

        // --- Optional scalar (widened) ---
        (LabelIr::Optional(_), FieldTypeIr::Scalar(s)) if needs_widening(s) => {
            let enc_mod = prost_encoding_mod(s);
            let wide_ty = widened_type(s);
            let native_ty = map_ir_type_to_native(&f.field_type, in_module);

            encode_stmts.push(quote! {
                if let ::core::option::Option::Some(v) = self.#fname {
                    let tmp = v as #wide_ty;

                    #enc_mod::encode(#tag, &tmp, buf);
                }
            });

            merge_arms.push(quote! {
                #tag => {
                    let mut tmp: #wide_ty = self.#fname.unwrap_or_default() as #wide_ty;

                    #enc_mod::merge(wire_type, &mut tmp, buf, ctx)
                        .map_err(|mut error| { error.push(STRUCT_NAME, #field_name_str); error })?;

                    self.#fname = ::core::option::Option::Some(tmp as #native_ty);

                    ::core::result::Result::Ok(())
                }
            });

            encoded_len_stmts.push(quote! {
                + self.#fname.map_or(0, |v|
                    #enc_mod::encoded_len(#tag, &(v as #wide_ty))
                )
            });

            clear_stmts.push(quote! { self.#fname = ::core::option::Option::None; });
            default_stmts.push(quote! { #fname: ::core::option::Option::None });
        },

        // --- Optional scalar (bytes conversion: u128/i128) ---
        (LabelIr::Optional(_), FieldTypeIr::Scalar(s)) if needs_bytes_conversion(s) => {
            let native_ty = bytes_native_type(s);

            encode_stmts.push(quote! {
                if let ::core::option::Option::Some(v) = self.#fname {
                    let tmp: Vec<u8> = v.to_le_bytes().to_vec();

                    ::prost::encoding::bytes::encode(#tag, &tmp, buf);
                }
            });

            merge_arms.push(quote! {
                #tag => {
                    let mut tmp: Vec<u8> = Vec::new();

                    ::prost::encoding::bytes::merge(wire_type, &mut tmp, buf, ctx)
                        .map_err(|mut error| { error.push(STRUCT_NAME, #field_name_str); error })?;

                    let arr: [u8; 16] = tmp.try_into().map_err(|_|
                        ::prost::DecodeError::new(
                            concat!("expected exactly 16 bytes for ", stringify!(#native_ty))
                        )
                    )?;

                    self.#fname = ::core::option::Option::Some(#native_ty::from_le_bytes(arr));

                    ::core::result::Result::Ok(())
                }
            });

            encoded_len_stmts.push(quote! {
                + self.#fname.map_or(0, |v|
                    ::prost::encoding::bytes::encoded_len(#tag, &v.to_le_bytes().to_vec())
                )
            });

            clear_stmts.push(quote! { self.#fname = ::core::option::Option::None; });
            default_stmts.push(quote! { #fname: ::core::option::Option::None });
        },

        // --- Optional scalar (no widening) ---
        (LabelIr::Optional(_), FieldTypeIr::Scalar(s)) => {
            let enc_mod = prost_encoding_mod(s);

            encode_stmts.push(quote! {
                if let ::core::option::Option::Some(ref value) = self.#fname {
                    #enc_mod::encode(#tag, value, buf);
                }
            });

            merge_arms.push(quote! {
                #tag => {
                    #enc_mod::merge(
                        wire_type,
                        self.#fname.get_or_insert_with(::core::default::Default::default),
                        buf, ctx,
                    ).map_err(|mut error| { error.push(STRUCT_NAME, #field_name_str); error })
                }
            });

            encoded_len_stmts.push(quote! {
                + self.#fname.as_ref().map_or(0, |v|
                    #enc_mod::encoded_len(#tag, v)
                )
            });

            clear_stmts.push(quote! { self.#fname = ::core::option::Option::None; });
            default_stmts.push(quote! { #fname: ::core::option::Option::None });
        },

        // --- Repeated/FixedArray message ---
        (LabelIr::Repeated | LabelIr::FixedArray(_), _) if is_message => {
            encode_stmts.push(quote! {
                for msg in &self.#fname {
                    ::prost::encoding::message::encode(#tag, msg, buf);
                }
            });

            merge_arms.push(quote! {
                #tag => {
                    ::prost::encoding::message::merge_repeated(wire_type, &mut self.#fname, buf, ctx)
                        .map_err(|mut error| { error.push(STRUCT_NAME, #field_name_str); error })
                }
            });

            encoded_len_stmts.push(quote! {
                + ::prost::encoding::message::encoded_len_repeated(#tag, &self.#fname)
            });

            clear_stmts.push(quote! { self.#fname.clear(); });
            default_stmts.push(quote! { #fname: Vec::new() });
        },

        // --- Repeated/FixedArray pubkey (Pubkey ↔ PublicKeyProtoWrapper conversion) ---
        (LabelIr::Repeated | LabelIr::FixedArray(_), FieldTypeIr::Scalar(s))
            if is_pubkey_scalar(s) =>
        {
            let pubkey_ty = map_ir_type_to_native(&f.field_type, in_module);
            let wrapper_ty = quote!(shipstern_core::PublicKeyProtoWrapper);

            encode_stmts.push(quote! {
                for pk in &self.#fname {
                    let wrapper = #wrapper_ty::new(pk.0.to_vec());

                    ::prost::encoding::message::encode(#tag, &wrapper, buf);
                }
            });

            merge_arms.push(quote! {
                #tag => {
                    let mut wrapper = #wrapper_ty::default();

                    ::prost::encoding::message::merge(wire_type, &mut wrapper, buf, ctx)
                        .map_err(|mut error| { error.push(STRUCT_NAME, #field_name_str); error })?;

                    let arr: [u8; 32] = wrapper.value.try_into().map_err(|_|
                        ::prost::DecodeError::new("expected exactly 32 bytes for Pubkey")
                    )?;

                    self.#fname.push(#pubkey_ty::new(arr));

                    ::core::result::Result::Ok(())
                }
            });

            encoded_len_stmts.push(quote! {
                + self.#fname.iter().map(|pk| {
                    let wrapper = #wrapper_ty::new(pk.0.to_vec());

                    ::prost::encoding::message::encoded_len(#tag, &wrapper)
                }).sum::<usize>()
            });

            clear_stmts.push(quote! { self.#fname.clear(); });
            default_stmts.push(quote! { #fname: Vec::new() });
        },

        // --- Repeated/FixedArray scalar (widened) ---
        (LabelIr::Repeated | LabelIr::FixedArray(_), FieldTypeIr::Scalar(s))
            if needs_widening(s) =>
        {
            let enc_mod = prost_encoding_mod(s);
            let wide_ty = widened_type(s);
            let native_ty = map_ir_type_to_native(&f.field_type, in_module);

            encode_stmts.push(quote! {
                {
                    let tmp: Vec<#wide_ty> = self.#fname.iter().map(|&v| v as #wide_ty).collect();

                    #enc_mod::encode_packed(#tag, &tmp, buf);
                }
            });

            merge_arms.push(quote! {
                #tag => {
                    let mut tmp: Vec<#wide_ty> = self.#fname.iter().map(|&v| v as #wide_ty).collect();

                    #enc_mod::merge_repeated(wire_type, &mut tmp, buf, ctx)
                        .map_err(|mut error| { error.push(STRUCT_NAME, #field_name_str); error })?;

                    self.#fname = tmp.into_iter().map(|v| v as #native_ty).collect();

                    ::core::result::Result::Ok(())
                }
            });

            encoded_len_stmts.push(quote! {
                + {
                    let tmp: Vec<#wide_ty> = self.#fname.iter().map(|&v| v as #wide_ty).collect();

                    #enc_mod::encoded_len_packed(#tag, &tmp)
                }
            });

            clear_stmts.push(quote! { self.#fname.clear(); });
            default_stmts.push(quote! { #fname: Vec::new() });
        },

        // --- Repeated/FixedArray scalar (bytes conversion: u128/i128) ---
        (LabelIr::Repeated | LabelIr::FixedArray(_), FieldTypeIr::Scalar(s))
            if needs_bytes_conversion(s) =>
        {
            let native_ty = bytes_native_type(s);

            encode_stmts.push(quote! {
                for v in &self.#fname {
                    let tmp: Vec<u8> = v.to_le_bytes().to_vec();

                    ::prost::encoding::bytes::encode(#tag, &tmp, buf);
                }
            });

            merge_arms.push(quote! {
                #tag => {
                    let mut tmp: Vec<u8> = Vec::new();

                    ::prost::encoding::bytes::merge(wire_type, &mut tmp, buf, ctx)
                        .map_err(|mut error| { error.push(STRUCT_NAME, #field_name_str); error })?;

                    let arr: [u8; 16] = tmp.try_into().map_err(|_|
                        ::prost::DecodeError::new(
                            concat!("expected exactly 16 bytes for ", stringify!(#native_ty))
                        )
                    )?;

                    self.#fname.push(#native_ty::from_le_bytes(arr));

                    ::core::result::Result::Ok(())
                }
            });

            encoded_len_stmts.push(quote! {
                + self.#fname.iter().map(|v|
                    ::prost::encoding::bytes::encoded_len(#tag, &v.to_le_bytes().to_vec())
                ).sum::<usize>()
            });

            clear_stmts.push(quote! { self.#fname.clear(); });
            default_stmts.push(quote! { #fname: Vec::new() });
        },

        // --- Repeated/FixedArray scalar (no widening) ---
        (LabelIr::Repeated | LabelIr::FixedArray(_), FieldTypeIr::Scalar(s)) => {
            let enc_mod = prost_encoding_mod(s);

            // String and bytes use length-delimited, not packed varint.
            let is_length_delimited = matches!(
                s,
                ScalarIr::String
                    | ScalarIr::SizePrefixedString { .. }
                    | ScalarIr::Bytes
                    | ScalarIr::SizePrefixedBytes { .. }
                    | ScalarIr::FixedBytes(_)
            );

            if is_length_delimited {
                encode_stmts.push(quote! {
                    for v in &self.#fname {
                        #enc_mod::encode(#tag, v, buf);
                    }
                });

                encoded_len_stmts.push(quote! {
                    + #enc_mod::encoded_len_repeated(#tag, &self.#fname)
                });
            } else {
                encode_stmts.push(quote! {
                    #enc_mod::encode_packed(#tag, &self.#fname, buf);
                });

                encoded_len_stmts.push(quote! {
                    + #enc_mod::encoded_len_packed(#tag, &self.#fname)
                });
            }

            merge_arms.push(quote! {
                #tag => {
                    #enc_mod::merge_repeated(wire_type, &mut self.#fname, buf, ctx)
                        .map_err(|mut error| { error.push(STRUCT_NAME, #field_name_str); error })
                }
            });

            clear_stmts.push(quote! { self.#fname.clear(); });
            default_stmts.push(quote! { #fname: Vec::new() });
        },

        // All Message variants are handled by the `if is_message` guards above.
        _ => unreachable!("unhandled field combo: {:?} / {:?}", f.label, f.field_type),
    }
}
