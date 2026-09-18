use std::collections::HashSet;

use proc_macro2::{Span, TokenStream};
use quote::{format_ident, quote};
use syn::LitStr;

use crate::intermediate_representation::{
    FieldIr, FieldTypeIr, LabelIr, OneofIr, OneofKindIr, OptionEncodingIr, OptionPrefixIr,
    ScalarIr, TypeIr, TypeKindIr,
};

// Generated types derive `serde::Serialize`/`Deserialize` behind the `serde` feature flag.
// Enable it in consumer crates that need JSON (or other serde-format) conversion.
pub fn rust_types_from_ir(schema_ir: &crate::intermediate_representation::SchemaIr) -> TokenStream {
    let mut out = TokenStream::new();

    let oneof_parents: HashSet<&str> = schema_ir
        .oneofs
        .iter()
        .map(|oneof_ir| oneof_ir.parent_message.as_str())
        .collect();

    // Oneof parents from dispatch oneofs only (InstructionDispatch / EventDispatch).
    // Used to exclude dispatch wrapper types (e.g. "Instructions") from the payload
    // type lists. We must NOT use the full `oneof_parents` set here because defined
    // type enum names (e.g. "SetRealmConfigItemArgs") would accidentally exclude
    // instruction/event payload types that share the same name.
    let dispatch_oneof_parents: HashSet<&str> = schema_ir
        .oneofs
        .iter()
        .filter(|o| {
            matches!(
                o.kind,
                OneofKindIr::InstructionDispatch | OneofKindIr::EventDispatch
            )
        })
        .map(|o| o.parent_message.as_str())
        .collect();

    // Names that exist as both a top-level defined type and an instruction/event type.
    // Inside submodules, a struct with a colliding name can't reference itself (no boxing),
    // so any `Message("SwapArgs")` field inside `SwapArgs` must mean the top-level type.
    let collisions = schema_ir.colliding_names();

    // Collect instruction-kind type names (these go inside the instruction module)
    let instruction_type_names: HashSet<&str> = schema_ir
        .types
        .iter()
        .filter(|t| t.kind == TypeKindIr::Instruction)
        .map(|t| t.name.as_str())
        .collect();

    // Collect event-kind type names (these go inside the event module)
    let event_type_names: HashSet<&str> = schema_ir
        .types
        .iter()
        .filter(|t| t.kind == TypeKindIr::Event)
        .map(|t| t.name.as_str())
        .collect();

    // Collect account-kind names so we can skip DefinedTypes that share the same name.
    // This happens when an IDL declares both an account and a type with the same name
    // (e.g. `MarginAccount`). The Account version is authoritative; emitting both would
    // produce duplicate struct definitions.
    let account_type_names: HashSet<&str> = schema_ir
        .types
        .iter()
        .filter(|t| matches!(t.kind, TypeKindIr::Account { .. }))
        .map(|t| t.name.as_str())
        .collect();

    // Render non-instruction, non-event types at top level (exclude oneof parents, rendered separately).
    // Use kind-based filtering (not name-based) so that defined types whose names
    // collide with instruction/event wrapper types are still rendered at the top level.
    for t in &schema_ir.types {
        if oneof_parents.contains(t.name.as_str()) {
            continue;
        }

        if t.kind == TypeKindIr::Instruction || t.kind == TypeKindIr::Event {
            continue;
        }

        // Skip DefinedTypes shadowed by an Account of the same name.
        if t.kind == TypeKindIr::DefinedType && account_type_names.contains(t.name.as_str()) {
            continue;
        }

        out.extend(render_struct_type(t, None));
    }

    // Render oneof parent types + their modules/enums
    for oneof in &schema_ir.oneofs {
        match oneof.kind {
            OneofKindIr::InstructionDispatch => {
                let ix_types: Vec<&TypeIr> = schema_ir
                    .types
                    .iter()
                    .filter(|t| t.kind == TypeKindIr::Instruction)
                    .filter(|t| !dispatch_oneof_parents.contains(t.name.as_str()))
                    .collect();

                out.extend(render_dispatch(
                    oneof,
                    &ix_types,
                    &instruction_type_names,
                    &collisions,
                    &oneof.parent_message,
                    "instruction",
                    "Instruction",
                ));
            },
            OneofKindIr::EventDispatch => {
                let ev_types: Vec<&TypeIr> = schema_ir
                    .types
                    .iter()
                    .filter(|t| t.kind == TypeKindIr::Event)
                    .filter(|t| !dispatch_oneof_parents.contains(t.name.as_str()))
                    .collect();

                // Use "Events" as the Rust wrapper name (not "ProgramEvents" which is the proto name)
                out.extend(render_dispatch(
                    oneof,
                    &ev_types,
                    &event_type_names,
                    &collisions,
                    "Events",
                    "event",
                    "Event",
                ));
            },
            OneofKindIr::Enum => {
                out.extend(render_enum_oneof(oneof));
            },
        }
    }

    out
}

fn render_struct_type(t: &TypeIr, local_names: Option<&HashSet<&str>>) -> TokenStream {
    let ident = format_ident!("{}", t.name);

    let fields: Vec<_> = t
        .fields
        .iter()
        .map(|f| render_field(f, local_names))
        .collect();

    if cfg!(feature = "proto") {
        let prost_impl = super::manual_prost::manual_prost_struct_impl(t, local_names);

        quote! {
            #[derive(Clone, PartialEq, ::borsh::BorshDeserialize, ::borsh::BorshSerialize)]
            #[cfg_attr(feature = "serde", derive(::serde::Serialize, ::serde::Deserialize))]
            pub struct #ident {
                #(#fields),*
            }

            #prost_impl
        }
    } else {
        quote! {
            #[derive(Clone, Debug, PartialEq, ::borsh::BorshDeserialize, ::borsh::BorshSerialize)]
            #[cfg_attr(feature = "serde", derive(::serde::Serialize, ::serde::Deserialize))]
            pub struct #ident {
                #(#fields),*
            }
        }
    }
}

///
/// Render a dispatch module: wrapper struct + inner module with enum + payload types.
///
/// Generates:
/// - `pub mod <mod_name> { pub enum <enum_name> { ... } /* + payload types */ }`
/// - Wrapper struct with a single field of the enum type (non-Option)
/// - Custom Borsh impls for the wrapper
/// - When proto: manual `prost::Message` impl (no prost derive on the wrapper)
///
fn render_dispatch(
    oneof_ir: &OneofIr,
    payload_types: &[&TypeIr],
    local_names: &HashSet<&str>,
    collisions: &std::collections::HashSet<String>,
    rust_wrapper_name: &str,
    mod_name: &str,
    enum_name: &str,
) -> TokenStream {
    let wrapper_ident = format_ident!("{}", rust_wrapper_name);
    let mod_ident = format_ident!("{}", mod_name);
    let oneof_ident = format_ident!("{}", enum_name);
    let field_ident = format_ident!("{}", oneof_ir.field_name);

    // Wrapper types (e.g. `Swap`) have fields that always reference local instruction
    // types (`SwapAccounts`, `SwapArgs`). Non-wrapper types (like `SwapArgs` itself)
    // may reference top-level defined types. When a name collides (instruction type
    // and defined type share the same name), non-wrapper types need collision-adjusted
    // `local_names` so their fields resolve to `super::`.
    let wrapper_names: HashSet<&str> = oneof_ir
        .variants
        .iter()
        .map(|v| v.message_type.as_str())
        .collect();

    let collision_adjusted_locals: HashSet<&str> = local_names
        .iter()
        .copied()
        .filter(|n| !collisions.contains(*n))
        .collect();

    let module_types: Vec<TokenStream> = payload_types
        .iter()
        .map(|t| {
            if wrapper_names.contains(t.name.as_str()) {
                render_struct_type(t, Some(local_names))
            } else {
                render_struct_type(t, Some(&collision_adjusted_locals))
            }
        })
        .collect();

    // Re-export top-level types for variants whose `<IxName>Args` (or
    // `<EvName>Args`) wrapper was suppressed because it would collide with a
    // top-level DefinedType. The variant's `args:` field then resolves to the
    // re-exported (FLAT) type, matching what the proto schema declares — so
    // encode/decode through the schema agree byte-for-byte. See
    // `build_instructions_schema::build_instruction_messages` for the
    // suppression rule.
    let module_reexports: Vec<TokenStream> = oneof_ir
        .variants
        .iter()
        .filter_map(|v| {
            let args_name = format!("{}Args", v.message_type);

            if local_names.contains(args_name.as_str()) {
                None
            } else {
                let args_ident = format_ident!("{}", args_name);

                Some(quote! { pub use super::#args_ident; })
            }
        })
        .collect();

    // Struct variants: `Swap { accounts: SwapAccounts, args: SwapArgs }`
    let variants = oneof_ir.variants.iter().map(|v| {
        let v_ident = format_ident!("{}", v.variant_name);
        let accounts_ident = format_ident!("{}Accounts", v.message_type);
        let args_ident = format_ident!("{}Args", v.message_type);

        quote! {
            #v_ident { accounts: #accounts_ident, args: #args_ident }
        }
    });

    let borsh_serialize_arms = oneof_ir.variants.iter().enumerate().map(|(i, v)| {
        let disc = i as u8;
        let v_ident = format_ident!("{}", v.variant_name);
        let msg_ident = format_ident!("{}", v.message_type);

        quote! {
            #mod_ident::#oneof_ident::#v_ident { accounts, args } => {
                ::borsh::BorshSerialize::serialize(&#disc, writer)?;
                ::borsh::BorshSerialize::serialize(&(#mod_ident::#msg_ident { accounts: accounts.clone(), args: args.clone() }), writer)
            }
        }
    });

    let borsh_deserialize_arms = oneof_ir.variants.iter().enumerate().map(|(i, v)| {
        let disc = i as u8;
        let v_ident = format_ident!("{}", v.variant_name);
        let msg_ident = format_ident!("{}", v.message_type);

        quote! {
            #disc => {
                let v: #mod_ident::#msg_ident = ::borsh::BorshDeserialize::deserialize_reader(reader)?;

                #mod_ident::#oneof_ident::#v_ident { accounts: v.accounts, args: v.args }
            }
        }
    });

    let proto_impls = if cfg!(feature = "proto") {
        let oneof_impl =
            super::manual_prost::manual_prost_oneof_impl(oneof_ir, &mod_ident, &oneof_ident);

        let message_impl = super::manual_prost::manual_prost_message_impl(
            &wrapper_ident,
            &field_ident,
            &mod_ident,
            &oneof_ident,
        );

        quote! { #oneof_impl #message_impl }
    } else {
        quote! {}
    };

    // Debug is manual in proto mode (provided by manual_prost_message_impl).
    let parent_debug_derive = if cfg!(feature = "proto") {
        quote! {}
    } else {
        quote! { Debug, }
    };

    quote! {
        #[derive(Clone, #parent_debug_derive PartialEq)]
        #[cfg_attr(feature = "serde", derive(::serde::Serialize, ::serde::Deserialize))]
        pub struct #wrapper_ident {
            pub #field_ident: #mod_ident::#oneof_ident,
        }

        pub mod #mod_ident {
            #(#module_reexports)*

            #(#module_types)*

            #[derive(Clone, Debug, PartialEq)]
            #[cfg_attr(feature = "serde", derive(::serde::Serialize, ::serde::Deserialize))]
            pub enum #oneof_ident {
                #(#variants),*
            }
        }

        #proto_impls

        impl ::borsh::BorshSerialize for #wrapper_ident {
            fn serialize<W: ::borsh::io::Write>(
                &self,
                writer: &mut W
            ) -> ::core::result::Result<(), ::borsh::io::Error> {
                match &self.#field_ident {
                    #(#borsh_serialize_arms,)*
                }
            }
        }

        impl ::borsh::BorshDeserialize for #wrapper_ident {
            fn deserialize_reader<R: ::borsh::io::Read>(
                reader: &mut R
            ) -> ::core::result::Result<Self, ::borsh::io::Error> {
                let disc: u8 = ::borsh::BorshDeserialize::deserialize_reader(reader)?;

                let #field_ident = match disc {
                    #(#borsh_deserialize_arms,)*

                    _ => {
                        return ::core::result::Result::Err(::borsh::io::Error::new(
                            ::borsh::io::ErrorKind::InvalidData,
                            format!("invalid discriminant {disc} (type {})", stringify!(#wrapper_ident))
                        ));
                    }
                };

                ::core::result::Result::Ok(Self { #field_ident })
            }
        }
    }
}

/// Render user-defined enum oneofs: keeps the module wrapper pattern.
fn render_enum_oneof(oneof_ir: &OneofIr) -> TokenStream {
    let parent_ident = format_ident!("{}", oneof_ir.parent_message);
    let mod_ident = format_ident!("{}", crate::utils::to_snake_case(&oneof_ir.parent_message));
    let oneof_ident = format_ident!("Kind");
    let field_ident = format_ident!("{}", oneof_ir.field_name);

    let variants = oneof_ir.variants.iter().map(|v| {
        let v_ident = format_ident!("{}", v.variant_name);
        let msg_ident = format_ident!("{}", v.message_type);

        let tag = v.tag;

        if cfg!(feature = "proto") {
            quote! {
                #[prost(message, tag = #tag)]
                #v_ident(super::#msg_ident)
            }
        } else {
            quote! {
                #v_ident(super::#msg_ident)
            }
        }
    });

    let borsh_serialize_arms = oneof_ir.variants.iter().enumerate().map(|(i, v)| {
        let disc = i as u8;
        let v_ident = format_ident!("{}", v.variant_name);

        quote! {
            #mod_ident::#oneof_ident::#v_ident(v) => {
                ::borsh::BorshSerialize::serialize(&#disc, writer)?;
                ::borsh::BorshSerialize::serialize(v, writer)
            }
        }
    });

    let borsh_deserialize_arms = oneof_ir.variants.iter().enumerate().map(|(i, v)| {
        let disc = i as u8;
        let v_ident = format_ident!("{}", v.variant_name);

        quote! {
            #disc => {
                let v = ::borsh::BorshDeserialize::deserialize_reader(reader)?;

                #mod_ident::#oneof_ident::#v_ident(v)
            }
        }
    });

    let enum_derive = if cfg!(feature = "proto") {
        quote! {
            #[derive(Clone, PartialEq, ::prost::Oneof)]
            #[cfg_attr(feature = "serde", derive(::serde::Serialize, ::serde::Deserialize))]
        }
    } else {
        quote! {
            #[derive(Clone, Debug, PartialEq)]
            #[cfg_attr(feature = "serde", derive(::serde::Serialize, ::serde::Deserialize))]
        }
    };

    let proto_impls = if cfg!(feature = "proto") {
        let first_variant = &oneof_ir.variants[0];

        let first_variant_ident = format_ident!("{}", first_variant.variant_name);
        let first_variant_msg = format_ident!("{}", first_variant.message_type);

        let message_impl = super::manual_prost::manual_prost_message_impl(
            &parent_ident,
            &field_ident,
            &mod_ident,
            &oneof_ident,
        );

        quote! {
            #message_impl

            impl ::core::default::Default for #parent_ident {
                fn default() -> Self {
                    Self {
                        #field_ident: #mod_ident::#oneof_ident::#first_variant_ident(
                            <#first_variant_msg as ::core::default::Default>::default(),
                        ),
                    }
                }
            }
        }
    } else {
        quote! {}
    };

    let debug_derive = if cfg!(feature = "proto") {
        quote! {}
    } else {
        quote! { Debug, }
    };

    quote! {
        #[derive(Clone, #debug_derive PartialEq)]
        #[cfg_attr(feature = "serde", derive(::serde::Serialize, ::serde::Deserialize))]
        pub struct #parent_ident {
            pub #field_ident: #mod_ident::#oneof_ident,
        }

        pub mod #mod_ident {
            #enum_derive
            pub enum #oneof_ident {
                #(#variants),*
            }
        }

        #proto_impls

        impl ::borsh::BorshSerialize for #parent_ident {
            fn serialize<W: ::borsh::io::Write>(
                &self,
                writer: &mut W
            ) -> ::core::result::Result<(), ::borsh::io::Error> {
                match &self.#field_ident {
                    #(#borsh_serialize_arms,)*
                }
            }
        }

        impl ::borsh::BorshDeserialize for #parent_ident {
            fn deserialize_reader<R: ::borsh::io::Read>(
                reader: &mut R
            ) -> ::core::result::Result<Self, ::borsh::io::Error> {
                let disc: u8 = ::borsh::BorshDeserialize::deserialize_reader(reader)?;

                let #field_ident = match disc {
                    #(#borsh_deserialize_arms,)*

                    _ => {
                        return ::core::result::Result::Err(::borsh::io::Error::new(
                            ::borsh::io::ErrorKind::InvalidData,
                            format!("invalid enum discriminant {disc} (type {})", stringify!(#parent_ident))
                        ));
                    }
                };

                ::core::result::Result::Ok(Self { #field_ident })
            }
        }
    }
}

///
/// Render a single struct field.
///
/// `local_names`: when `Some`, we are inside a submodule. Message types not in the set
/// and borsh helper paths get `super::` prefixed.
///
pub fn render_field(f: &FieldIr, local_names: Option<&HashSet<&str>>) -> TokenStream {
    let name = format_ident!("{}", f.name);
    let in_module = local_names.is_some();
    let path_prefix = if in_module { "super::" } else { "" };

    // Custom borsh attrs for fields whose on-chain encoding differs from the Rust type.
    // Public Rust types and protobuf fields stay independent of these wire codecs.
    let borsh_attr = if matches!(
        f.field_type,
        FieldTypeIr::Scalar(
            ScalarIr::SizePrefixedBytes { .. } | ScalarIr::SizePrefixedString { .. }
        )
    ) {
        super::size_prefixed_bytes::attrs(f, path_prefix)
    } else {
        let option = option_borsh_attrs(&f.label, &f.field_type, path_prefix);

        if !option.is_empty() {
            option
        } else {
            let fixed = fixed_bytes_borsh_attrs(&f.label, &f.field_type, path_prefix);

            if !fixed.is_empty() {
                fixed
            } else {
                let float = float_borsh_attrs(&f.label, &f.field_type, path_prefix);

                if !float.is_empty() {
                    float
                } else {
                    // For FixedArray, we always need a custom borsh attr (no length prefix).
                    fixed_array_default_borsh_attrs(&f.label, path_prefix)
                }
            }
        }
    };

    // Resolve a Message type ident, adding `super::` when in a submodule and the type is external
    let resolve_msg = |msg: &str| -> TokenStream {
        let ident = format_ident!("{}", msg);

        if let Some(locals) = local_names {
            if locals.contains(msg) {
                quote!(#ident)
            } else {
                quote!(super::#ident)
            }
        } else {
            quote!(#ident)
        }
    };

    // Both proto and non-proto paths now use native types and no prost attributes.
    // The manual prost::Message impl (generated by manual_prost_struct_impl) handles encoding.
    match (&f.label, &f.field_type) {
        (LabelIr::Singular, FieldTypeIr::Message(msg)) => {
            let ty = resolve_msg(msg);

            quote! { #borsh_attr pub #name: #ty }
        },
        (LabelIr::Singular, field_type) => {
            let rust_type = map_ir_type_to_native(field_type, in_module);

            quote! { #borsh_attr pub #name: #rust_type }
        },
        (LabelIr::Optional(_), FieldTypeIr::Message(msg)) => {
            let ty = resolve_msg(msg);

            quote! { #borsh_attr pub #name: ::core::option::Option<#ty> }
        },
        (LabelIr::Optional(_), field_type) => {
            let rust_type = map_ir_type_to_native(field_type, in_module);

            quote! { #borsh_attr pub #name: ::core::option::Option<#rust_type> }
        },
        (LabelIr::Repeated | LabelIr::FixedArray(_), FieldTypeIr::Message(msg)) => {
            let ty = resolve_msg(msg);

            quote! { #borsh_attr pub #name: Vec<#ty> }
        },
        (LabelIr::Repeated | LabelIr::FixedArray(_), field_type) => {
            let rust_type = map_ir_type_to_native(field_type, in_module);

            quote! { #borsh_attr pub #name: Vec<#rust_type> }
        },
    }
}

/// Returns custom Borsh hooks for Codama options that do not use Rust Borsh's
/// native u8-tag encoding. This includes non-u8 prefixes and fixed options,
/// whose `None` representation includes zero padding for the item size.
fn option_borsh_attrs(label: &LabelIr, field_type: &FieldTypeIr, path_prefix: &str) -> TokenStream {
    let LabelIr::Optional(encoding) = label else {
        return quote! {};
    };

    if encoding.uses_native_borsh() {
        return quote! {};
    }

    let padding = encoding.none_padding.unwrap_or(0);
    let (deserialize, serialize) = option_borsh_paths(encoding, field_type, path_prefix, padding);
    let deserialize = LitStr::new(&deserialize, Span::call_site());
    let serialize = LitStr::new(&serialize, Span::call_site());

    quote! {
        #[borsh(
            deserialize_with = #deserialize,
            serialize_with = #serialize
        )]
    }
}

fn option_borsh_paths(
    encoding: &OptionEncodingIr,
    field_type: &FieldTypeIr,
    path_prefix: &str,
    padding: usize,
) -> (String, String) {
    let names = match field_type {
        FieldTypeIr::Scalar(ScalarIr::FixedBytes(size)) => {
            return option_fixed_bytes_borsh_paths(encoding, *size, path_prefix, padding);
        },
        FieldTypeIr::Scalar(ScalarIr::Float) => {
            return option_float_borsh_paths(encoding, "f32", path_prefix, padding);
        },
        FieldTypeIr::Scalar(ScalarIr::Double) => {
            return option_float_borsh_paths(encoding, "f64", path_prefix, padding);
        },
        _ => ("borsh_deserialize_option", "borsh_serialize_option"),
    };

    match &encoding.prefix {
        OptionPrefixIr::FixedWidth {
            byte_len,
            one_value,
            big_endian,
        } => (
            format!(
                "{path_prefix}{}::<_, {byte_len}, {one_value}, {big_endian}, {padding}, _>",
                names.0
            ),
            format!(
                "{path_prefix}{}::<_, {byte_len}, {one_value}, {big_endian}, {padding}, _>",
                names.1
            ),
        ),
        OptionPrefixIr::ShortU16 => (
            format!("{path_prefix}borsh_deserialize_short_u16_option::<_, {padding}, _>"),
            format!("{path_prefix}borsh_serialize_short_u16_option::<_, {padding}, _>"),
        ),
    }
}

fn option_fixed_bytes_borsh_paths(
    encoding: &OptionEncodingIr,
    size: usize,
    path_prefix: &str,
    padding: usize,
) -> (String, String) {
    match &encoding.prefix {
        OptionPrefixIr::FixedWidth {
            byte_len,
            one_value,
            big_endian,
        } => (
            format!(
                "{path_prefix}borsh_deserialize_option_fixed_bytes::<{size}, {byte_len}, \
                 {one_value}, {big_endian}, {padding}, _>"
            ),
            format!(
                "{path_prefix}borsh_serialize_option_fixed_bytes::<{size}, {byte_len}, \
                 {one_value}, {big_endian}, {padding}, _>"
            ),
        ),
        OptionPrefixIr::ShortU16 => (
            format!(
                "{path_prefix}borsh_deserialize_short_u16_option_fixed_bytes::<{size}, {padding}, \
                 _>"
            ),
            format!(
                "{path_prefix}borsh_serialize_short_u16_option_fixed_bytes::<{size}, {padding}, _>"
            ),
        ),
    }
}

fn option_float_borsh_paths(
    encoding: &OptionEncodingIr,
    suffix: &str,
    path_prefix: &str,
    padding: usize,
) -> (String, String) {
    match &encoding.prefix {
        OptionPrefixIr::FixedWidth {
            byte_len,
            one_value,
            big_endian,
        } => (
            format!(
                "{path_prefix}borsh_deserialize_option_{suffix}_permissive::<{byte_len}, \
                 {one_value}, {big_endian}, {padding}, _>"
            ),
            format!(
                "{path_prefix}borsh_serialize_option_{suffix}_permissive::<{byte_len}, \
                 {one_value}, {big_endian}, {padding}, _>"
            ),
        ),
        OptionPrefixIr::ShortU16 => (
            format!(
                "{path_prefix}borsh_deserialize_short_u16_option_{suffix}_permissive::<{padding}, \
                 _>"
            ),
            format!(
                "{path_prefix}borsh_serialize_short_u16_option_{suffix}_permissive::<{padding}, _>"
            ),
        ),
    }
}

/// Returns `#[borsh(deserialize_with = "...", serialize_with = "...")]` for fixed-size byte fields
/// (FixedBytes), or an empty TokenStream for all other field types.
fn fixed_bytes_borsh_attrs(
    label: &LabelIr,
    field_type: &FieldTypeIr,
    path_prefix: &str,
) -> TokenStream {
    match field_type {
        FieldTypeIr::Scalar(ScalarIr::FixedBytes(_)) => {},
        _ => return quote! {},
    }

    let size = match field_type {
        FieldTypeIr::Scalar(ScalarIr::FixedBytes(n)) => *n,
        _ => unreachable!(),
    };

    let (deserialize_path, serialize_path) = (
        LitStr::new(
            &format!("{path_prefix}borsh_deserialize_fixed_bytes::<{size}, _>"),
            Span::call_site(),
        ),
        LitStr::new(
            &format!("{path_prefix}borsh_serialize_fixed_bytes::<{size}, _>"),
            Span::call_site(),
        ),
    );

    let (deserialize_opt_path, serialize_opt_path) = (
        LitStr::new(
            &format!("{path_prefix}borsh_deserialize_opt_fixed_bytes::<{size}, _>"),
            Span::call_site(),
        ),
        LitStr::new(
            &format!("{path_prefix}borsh_serialize_opt_fixed_bytes::<{size}, _>"),
            Span::call_site(),
        ),
    );

    let (deserialize_vec_path, serialize_vec_path) = (
        LitStr::new(
            &format!("{path_prefix}borsh_deserialize_vec_fixed_bytes::<{size}, _>"),
            Span::call_site(),
        ),
        LitStr::new(
            &format!("{path_prefix}borsh_serialize_vec_fixed_bytes::<{size}, _>"),
            Span::call_site(),
        ),
    );

    match label {
        LabelIr::Singular => quote! {
            #[borsh(
                deserialize_with = #deserialize_path,
                serialize_with = #serialize_path
            )]
        },
        LabelIr::Optional(_) => quote! {
            #[borsh(
                deserialize_with = #deserialize_opt_path,
                serialize_with = #serialize_opt_path
            )]
        },
        LabelIr::Repeated => quote! {
            #[borsh(
                deserialize_with = #deserialize_vec_path,
                serialize_with = #serialize_vec_path
            )]
        },
        LabelIr::FixedArray(n) => {
            let d = LitStr::new(
                &format!(
                    "{path_prefix}borsh_deserialize_fixed_array_fixed_bytes::<{size}, {n}, _>"
                ),
                Span::call_site(),
            );

            let s = LitStr::new(
                &format!("{path_prefix}borsh_serialize_fixed_array_fixed_bytes::<{size}, {n}, _>"),
                Span::call_site(),
            );

            quote! { #[borsh(deserialize_with = #d, serialize_with = #s)] }
        },
    }
}

///
/// Returns `#[borsh(deserialize_with = "...", serialize_with = "...")]` for float fields
/// (f32, f64) that need permissive deserialization allowing NaN/Infinity values.
/// Standard borsh rejects NaN for portability, but on-chain data may contain them.
///
fn float_borsh_attrs(label: &LabelIr, field_type: &FieldTypeIr, path_prefix: &str) -> TokenStream {
    let suffix = match field_type {
        FieldTypeIr::Scalar(ScalarIr::Float) => "f32",
        FieldTypeIr::Scalar(ScalarIr::Double) => "f64",
        _ => return quote! {},
    };

    let (deserialize_fn_name, serialize_fn_name) = match label {
        LabelIr::Singular => (
            format!("{path_prefix}borsh_deserialize_{suffix}_permissive"),
            format!("{path_prefix}borsh_serialize_{suffix}_permissive"),
        ),
        LabelIr::Optional(_) => (
            format!("{path_prefix}borsh_deserialize_opt_{suffix}_permissive"),
            format!("{path_prefix}borsh_serialize_opt_{suffix}_permissive"),
        ),
        LabelIr::Repeated => (
            format!("{path_prefix}borsh_deserialize_vec_{suffix}_permissive"),
            format!("{path_prefix}borsh_serialize_vec_{suffix}_permissive"),
        ),
        LabelIr::FixedArray(n) => (
            format!("{path_prefix}borsh_deserialize_fixed_array_{suffix}_permissive::<{n}, _>"),
            format!("{path_prefix}borsh_serialize_fixed_array_{suffix}_permissive::<{n}, _>"),
        ),
    };

    let deserialize_lit = LitStr::new(&deserialize_fn_name, Span::call_site());
    let serialize_lit = LitStr::new(&serialize_fn_name, Span::call_site());

    quote! {
        #[borsh(
            deserialize_with = #deserialize_lit,
            serialize_with = #serialize_lit
        )]
    }
}

/// Returns borsh attrs for FixedArray fields whose element type has a standard BorshDeserialize
/// (i.e., not fixed-bytes, not widened, not floats). Returns empty for non-FixedArray labels.
fn fixed_array_default_borsh_attrs(label: &LabelIr, path_prefix: &str) -> TokenStream {
    let n = match label {
        LabelIr::FixedArray(n) => n,
        _ => return quote! {},
    };

    let deserialize_lit = LitStr::new(
        &format!("{path_prefix}borsh_deserialize_fixed_array::<_, {n}, _>"),
        Span::call_site(),
    );
    let serialize_lit = LitStr::new(
        &format!("{path_prefix}borsh_serialize_fixed_array::<_, {n}, _>"),
        Span::call_site(),
    );

    quote! {
        #[borsh(
            deserialize_with = #deserialize_lit,
            serialize_with = #serialize_lit
        )]
    }
}

/// Return the native Rust type for a field, preserving on-chain precision.
/// Does NOT widen u8→u32 etc. — manual prost impl handles the casting.
pub(super) fn map_ir_type_to_native(field_type: &FieldTypeIr, in_module: bool) -> TokenStream {
    match field_type {
        FieldTypeIr::Scalar(s) => match s {
            ScalarIr::Bool => quote!(bool),
            ScalarIr::U8 => quote!(u8),
            ScalarIr::U16 | ScalarIr::ShortU16 => quote!(u16),
            ScalarIr::Uint32 => quote!(u32),
            ScalarIr::Uint64 => quote!(u64),
            ScalarIr::I8 => quote!(i8),
            ScalarIr::I16 => quote!(i16),
            ScalarIr::Int32 => quote!(i32),
            ScalarIr::Int64 => quote!(i64),
            ScalarIr::Float => quote!(f32),
            ScalarIr::Double => quote!(f64),
            ScalarIr::U128 => quote!(u128),
            ScalarIr::I128 => quote!(i128),
            ScalarIr::String | ScalarIr::SizePrefixedString { .. } => quote!(String),
            ScalarIr::Bytes | ScalarIr::SizePrefixedBytes { .. } | ScalarIr::FixedBytes(_) => {
                quote!(Vec<u8>)
            },
            ScalarIr::PublicKey => {
                if in_module {
                    quote!(super::Pubkey)
                } else {
                    quote!(Pubkey)
                }
            },
        },
        FieldTypeIr::Message(name) => {
            let ident = format_ident!("{}", name);

            quote!(#ident)
        },
    }
}
