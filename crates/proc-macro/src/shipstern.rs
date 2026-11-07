use proc_macro2::{Span, TokenStream};
use quote::quote;
use syn::{
    spanned::Spanned, Attribute, Fields, GenericArgument, ItemEnum, ItemStruct, Meta,
    PathArguments, PathSegment, Type,
};

enum Mode {
    Message,
    Oneof,
    Enumeration,
}

///
/// Main entry point for the `#[shipstern]` attribute macro.
/// Determines the mode (message, oneof, enumeration) and dispatches to the appropriate expansion function.
///
/// # Usage Examples
///
/// ```rust, ignore
/// // Struct with prost::Message (default mode):
/// #[shipstern]
/// struct Transfer {
///     pub source: Option<String>,
///     pub destination: Option<String>,
///     pub amount: u64,
/// }
///
/// // Enum with prost::Oneof:
/// #[shipstern(oneof)]
/// enum Instruction {
///     Transfer(Transfer),
///     Approve(Approve),
/// }
///
/// // Enum with prost::Enumeration:
/// #[shipstern(enumeration)]
/// #[repr(i32)]
/// enum AccountType {
///     Uninitialized = 0,
///     Mint = 1,
///     Account = 2,
/// }
/// ```
///
pub fn expand(attr: TokenStream, item: TokenStream) -> syn::Result<TokenStream> {
    let mode = parse_mode(attr)?;

    // Try struct first, then enum
    if let Ok(mut item_struct) = syn::parse2::<ItemStruct>(item.clone()) {
        match mode {
            Mode::Message => expand_message(&mut item_struct),

            Mode::Oneof => Err(syn::Error::new_spanned(
                &item_struct.ident,
                "shipstern(oneof) requires an enum, not a struct",
            )),

            Mode::Enumeration => Err(syn::Error::new_spanned(
                &item_struct.ident,
                "shipstern(enumeration) requires an enum, not a struct",
            )),
        }
    } else if let Ok(mut item_enum) = syn::parse2::<ItemEnum>(item.clone()) {
        match mode {
            Mode::Message => Err(syn::Error::new_spanned(
                &item_enum.ident,
                "shipstern on an enum requires a mode: #[shipstern(oneof)] or \
                 #[shipstern(enumeration)]",
            )),

            Mode::Oneof => expand_oneof(&mut item_enum),

            Mode::Enumeration => expand_enumeration(&mut item_enum),
        }
    } else {
        Err(syn::Error::new(
            Span::call_site(),
            "shipstern can only be applied to structs or enums",
        ))
    }
}

fn parse_mode(attr: TokenStream) -> syn::Result<Mode> {
    if attr.is_empty() {
        return Ok(Mode::Message);
    }

    let ident: syn::Ident = syn::parse2(attr)?;
    match ident.to_string().as_str() {
        "oneof" => Ok(Mode::Oneof),
        "enumeration" => Ok(Mode::Enumeration),
        other => Err(syn::Error::new(
            ident.span(),
            format!("unknown shipstern mode `{other}`, expected `oneof` or `enumeration`"),
        )),
    }
}

///
///  Expands a struct annotated with `#[shipstern]` into a `prost::Message`.
///
/// Auto-tags fields starting at 1 and infers prost annotations from Rust types.
/// Use `#[hint(...)]` on individual fields when the type can't be auto-inferred.
///
/// Input:
/// ```rust, ignore
/// #[shipstern]
/// struct Transfer {
///     pub source: Option<String>,
///     pub destination: Option<String>,
///     pub amount: u64,
///     pub data: Vec<u8>,
///     pub signers: Vec<String>,
///     #[hint(oneof = "Kind", tags = "6, 7")]
///     pub kind: Option<Kind>,
/// }
/// ```
///
/// Output (with `proto` feature):
/// ```rust, ignore
/// #[derive(::prost::Message)]
/// struct Transfer {
///     #[prost(string, optional, tag = 1)]
///     pub source: Option<String>,
///     #[prost(string, optional, tag = 2)]
///     pub destination: Option<String>,
///     #[prost(uint64, tag = 3)]
///     pub amount: u64,
///     #[prost(bytes = "vec", tag = 4)]
///     pub data: Vec<u8>,
///     #[prost(string, repeated, tag = 5)]
///     pub signers: Vec<String>,
///     #[prost(oneof = "Kind", tags = "6, 7")]
///     pub kind: Option<Kind>,
/// }
/// ```
///
/// Output (without `proto` feature):
/// ```rust, ignore
/// #[derive(Debug)]
/// struct Transfer {
///     pub source: Option<String>,
///     pub destination: Option<String>,
///     pub amount: u64,
///     pub data: Vec<u8>,
///     pub signers: Vec<String>,
///     pub kind: Option<Kind>,
/// }
/// ```
///
fn expand_message(item: &mut ItemStruct) -> syn::Result<TokenStream> {
    let Fields::Named(ref mut fields) = item.fields else {
        return Err(syn::Error::new_spanned(
            &item.ident,
            "shipstern requires named fields",
        ));
    };

    for (tag, field) in (1_u32..).zip(&mut fields.named) {
        // Always consume #[hint(...)] to prevent unknown-attribute errors
        let hint = take_proto_attr(&mut field.attrs)?;

        if cfg!(feature = "proto") {
            if let Some(override_tokens) = hint {
                let override_str = override_tokens.to_string();

                if override_str.contains("tags") {
                    field.attrs.push(parse_attr(quote! {
                        #[prost(#override_tokens)]
                    }));
                } else {
                    field.attrs.push(parse_attr(quote! {
                        #[prost(#override_tokens, tag = #tag)]
                    }));
                }
            } else {
                let annotation = classify_type(&field.ty);

                let prost_tokens = prost_annotation(&annotation, tag);

                field.attrs.push(parse_attr(quote! {
                    #[prost(#prost_tokens)]
                }));
            }
        }
    }

    if cfg!(feature = "proto") {
        item.attrs.push(parse_attr(quote! {
            #[derive(::prost::Message)]
        }));
    } else {
        item.attrs.push(parse_attr(quote! { #[derive(Debug)] }));
    }

    Ok(quote! { #item })
}

///
/// Expands an enum annotated with `#[shipstern(oneof)]` into a `prost::Oneof`.
///
/// Each variant must be a single-field tuple variant. Tags are auto-assigned starting at 1.
///
/// Input:
/// ```rust, ignore
/// #[shipstern(oneof)]
/// enum Instruction {
///     Transfer(Transfer),
///     Approve(Approve),
///     SetAmount(u64),
/// }
/// ```
///
/// Output (with `proto` feature):
/// ```rust, ignore
/// #[derive(::prost::Oneof)]
/// enum Instruction {
///     #[prost(message, tag = 1)]
///     Transfer(Transfer),
///     #[prost(message, tag = 2)]
///     Approve(Approve),
///     #[prost(uint64, tag = 3)]
///     SetAmount(u64),
/// }
/// ```
///
/// Output (without `proto` feature):
/// ```rust, ignore
/// #[derive(Debug)]
/// enum Instruction {
///     Transfer(Transfer),
///     Approve(Approve),
///     SetAmount(u64),
/// }
/// ```
///
fn expand_oneof(item: &mut ItemEnum) -> syn::Result<TokenStream> {
    for (tag, variant) in (1_u32..).zip(&mut item.variants) {
        let Fields::Unnamed(ref unnamed) = variant.fields else {
            return Err(syn::Error::new_spanned(
                variant,
                "oneof variants must be tuple variants with exactly one field",
            ));
        };

        if unnamed.unnamed.len() != 1 {
            return Err(syn::Error::new_spanned(
                variant,
                "oneof variants must have exactly one field",
            ));
        }

        if cfg!(feature = "proto") {
            let inner_ty = &unnamed.unnamed.first().unwrap().ty;
            let annotation = classify_type(inner_ty);
            let prost_tokens = prost_oneof_variant(&annotation, tag);

            variant.attrs.push(parse_attr(quote! {
                #[prost(#prost_tokens)]
            }));
        }
    }

    if cfg!(feature = "proto") {
        item.attrs.push(parse_attr(quote! {
            #[derive(::prost::Oneof)]
        }));
    } else {
        item.attrs.push(parse_attr(quote! { #[derive(Debug)] }));
    }

    Ok(quote! { #item })
}

///
/// Expands an enum annotated with `#[shipstern(enumeration)]` into a `prost::Enumeration`.
///
/// Requires `#[repr(i32)]` on the enum because protobuf enumerations are int32 on the wire.
///
/// Input:
/// ```rust, ignore
/// #[shipstern(enumeration)]
/// #[repr(i32)]
/// enum AccountType {
///     Uninitialized = 0,
///     Mint = 1,
///     Account = 2,
/// }
/// ```
///
/// Output (with `proto` feature):
/// ```rust, ignore
/// #[repr(i32)]
/// #[derive(::prost::Enumeration)]
/// #[derive(Debug)]
/// enum AccountType {
///     Uninitialized = 0,
///     Mint = 1,
///     Account = 2,
/// }
/// ```
///
/// Output (without `proto` feature):
/// ```rust, ignore
/// #[repr(i32)]
/// #[derive(Debug)]
/// enum AccountType {
///     Uninitialized = 0,
///     Mint = 1,
///     Account = 2,
/// }
/// ```
///
fn expand_enumeration(item: &mut ItemEnum) -> syn::Result<TokenStream> {
    // Verify #[repr(i32)] is present
    let has_repr_i32 = item.attrs.iter().any(|attr| {
        if !attr.path().is_ident("repr") {
            return false;
        }

        let Ok(inner) = attr.parse_args::<syn::Ident>() else {
            return false;
        };

        inner == "i32"
    });

    if !has_repr_i32 {
        return Err(syn::Error::new_spanned(
            &item.ident,
            "shipstern(enumeration) requires #[repr(i32)] on the enum",
        ));
    }

    if cfg!(feature = "proto") {
        item.attrs.push(parse_attr(quote! {
            #[derive(::prost::Enumeration)]
        }));
    }

    // prost::Enumeration does NOT auto-derive Debug, so always add it
    // Check if Debug is already in an existing derive to avoid duplicates
    if !has_derive(item, "Debug") {
        item.attrs.push(parse_attr(quote! { #[derive(Debug)] }));
    }

    Ok(quote! { #item })
}

enum FieldClassification {
    Scalar(ScalarKind),
    Bytes,
    OptionalScalar(ScalarKind),
    OptionalBytes,
    OptionalMessage,
    RepeatedScalar(ScalarKind),
    RepeatedBytes,
    RepeatedMessage,
    RequiredMessage,
}

#[derive(Clone, Copy)]
enum ScalarKind {
    Bool,
    Uint32,
    Uint64,
    Int32,
    Int64,
    Float,
    Double,
    StringType,
}

fn classify_type(ty: &Type) -> FieldClassification {
    let Some(seg) = last_segment(ty) else {
        return FieldClassification::RequiredMessage;
    };

    match seg.ident.to_string().as_str() {
        "u64" => FieldClassification::Scalar(ScalarKind::Uint64),
        "u32" => FieldClassification::Scalar(ScalarKind::Uint32),
        "i32" => FieldClassification::Scalar(ScalarKind::Int32),
        "i64" => FieldClassification::Scalar(ScalarKind::Int64),
        "f32" => FieldClassification::Scalar(ScalarKind::Float),
        "f64" => FieldClassification::Scalar(ScalarKind::Double),
        "bool" => FieldClassification::Scalar(ScalarKind::Bool),
        "String" => FieldClassification::Scalar(ScalarKind::StringType),
        "Option" => classify_option(seg),
        "Vec" => classify_vec(seg),
        "PublicKeyProtoWrapper" | "Pubkey" | "KeyBytes" => FieldClassification::RequiredMessage,
        _ => FieldClassification::RequiredMessage,
    }
}

fn classify_option(seg: &PathSegment) -> FieldClassification {
    let Some(inner) = unwrap_generic_arg(seg) else {
        return FieldClassification::OptionalMessage;
    };

    let Some(inner_seg) = last_segment(inner) else {
        return FieldClassification::OptionalMessage;
    };

    match inner_seg.ident.to_string().as_str() {
        "u64" => FieldClassification::OptionalScalar(ScalarKind::Uint64),
        "u32" => FieldClassification::OptionalScalar(ScalarKind::Uint32),
        "i32" => FieldClassification::OptionalScalar(ScalarKind::Int32),
        "i64" => FieldClassification::OptionalScalar(ScalarKind::Int64),
        "f32" => FieldClassification::OptionalScalar(ScalarKind::Float),
        "f64" => FieldClassification::OptionalScalar(ScalarKind::Double),
        "bool" => FieldClassification::OptionalScalar(ScalarKind::Bool),
        "String" => FieldClassification::OptionalScalar(ScalarKind::StringType),
        "Vec" => {
            // Option<Vec<u8>>
            if is_vec_u8(inner_seg) {
                FieldClassification::OptionalBytes
            } else {
                FieldClassification::OptionalMessage
            }
        },
        _ => FieldClassification::OptionalMessage,
    }
}

fn classify_vec(seg: &PathSegment) -> FieldClassification {
    let Some(inner) = unwrap_generic_arg(seg) else {
        return FieldClassification::RepeatedMessage;
    };

    let Some(inner_seg) = last_segment(inner) else {
        return FieldClassification::RepeatedMessage;
    };

    match inner_seg.ident.to_string().as_str() {
        "u8" => FieldClassification::Bytes, // Vec<u8> = bytes, not repeated u8
        "u64" => FieldClassification::RepeatedScalar(ScalarKind::Uint64),
        "u32" => FieldClassification::RepeatedScalar(ScalarKind::Uint32),
        "i32" => FieldClassification::RepeatedScalar(ScalarKind::Int32),
        "i64" => FieldClassification::RepeatedScalar(ScalarKind::Int64),
        "f32" => FieldClassification::RepeatedScalar(ScalarKind::Float),
        "f64" => FieldClassification::RepeatedScalar(ScalarKind::Double),
        "bool" => FieldClassification::RepeatedScalar(ScalarKind::Bool),
        "String" => FieldClassification::RepeatedScalar(ScalarKind::StringType),
        "Vec" => {
            // Vec<Vec<u8>> = repeated bytes
            if is_vec_u8(inner_seg) {
                FieldClassification::RepeatedBytes
            } else {
                FieldClassification::RepeatedMessage
            }
        },
        _ => FieldClassification::RepeatedMessage,
    }
}

fn prost_annotation(cls: &FieldClassification, tag: u32) -> TokenStream {
    match cls {
        FieldClassification::Scalar(kind) => {
            let ident = scalar_ident(*kind);

            quote!(#ident, tag = #tag)
        },
        FieldClassification::Bytes => {
            quote!(bytes = "vec", tag = #tag)
        },
        FieldClassification::OptionalScalar(kind) => {
            let ident = scalar_ident(*kind);

            quote!(#ident, optional, tag = #tag)
        },
        FieldClassification::OptionalBytes => {
            quote!(bytes = "vec", optional, tag = #tag)
        },
        FieldClassification::OptionalMessage => {
            quote!(message, optional, tag = #tag)
        },
        FieldClassification::RequiredMessage => {
            quote!(message, required, tag = #tag)
        },
        FieldClassification::RepeatedScalar(kind) => {
            let ident = scalar_ident(*kind);

            quote!(#ident, repeated, tag = #tag)
        },
        FieldClassification::RepeatedBytes => {
            quote!(bytes = "vec", repeated, tag = #tag)
        },
        FieldClassification::RepeatedMessage => {
            quote!(message, repeated, tag = #tag)
        },
    }
}

/// For oneof variant types: simpler annotation (no optional/repeated wrappers)
fn prost_oneof_variant(classification: &FieldClassification, tag: u32) -> TokenStream {
    match classification {
        FieldClassification::Scalar(kind) => {
            let ident = scalar_ident(*kind);

            quote!(#ident, tag = #tag)
        },
        FieldClassification::Bytes => {
            quote!(bytes, tag = #tag)
        },
        _ => {
            // message types, nested enums, etc.
            quote!(message, tag = #tag)
        },
    }
}

fn scalar_ident(kind: ScalarKind) -> syn::Ident {
    let s = match kind {
        ScalarKind::Bool => "bool",
        ScalarKind::Uint32 => "uint32",
        ScalarKind::Uint64 => "uint64",
        ScalarKind::Int32 => "int32",
        ScalarKind::Int64 => "int64",
        ScalarKind::Float => "float",
        ScalarKind::Double => "double",
        ScalarKind::StringType => "string",
    };

    syn::Ident::new(s, Span::call_site())
}

fn last_segment(ty: &Type) -> Option<&PathSegment> {
    if let Type::Path(type_path) = ty {
        type_path.path.segments.last()
    } else {
        None
    }
}

fn unwrap_generic_arg(seg: &PathSegment) -> Option<&Type> {
    if let PathArguments::AngleBracketed(ref args) = seg.arguments
        && let Some(GenericArgument::Type(ty)) = args.args.first()
    {
        return Some(ty);
    }

    None
}

fn is_vec_u8(vec_seg: &PathSegment) -> bool {
    let Some(inner) = unwrap_generic_arg(vec_seg) else {
        return false;
    };

    let Some(inner_seg) = last_segment(inner) else {
        return false;
    };

    matches!(inner_seg.ident.to_string().as_str(), "u8")
}

/// Extract and remove a `#[hint(...)]` attribute, returning its inner tokens.
fn take_proto_attr(attrs: &mut Vec<Attribute>) -> syn::Result<Option<TokenStream>> {
    let idx = attrs.iter().position(|a| a.path().is_ident("hint"));

    let Some(idx) = idx else {
        return Ok(None);
    };

    let attr = attrs.remove(idx);

    match &attr.meta {
        Meta::List(list) => Ok(Some(list.tokens.clone())),
        _ => Err(syn::Error::new(
            attr.span(),
            "expected #[hint(...)] with arguments",
        )),
    }
}

fn parse_attr(tokens: TokenStream) -> Attribute { syn::parse_quote!(#tokens) }

fn has_derive(item: &ItemEnum, name: &str) -> bool {
    item.attrs.iter().any(|attr| {
        if !attr.path().is_ident("derive") {
            return false;
        }

        let Ok(nested) = attr.parse_args_with(
            syn::punctuated::Punctuated::<syn::Path, syn::Token![,]>::parse_terminated,
        ) else {
            return false;
        };

        nested.iter().any(|p| p.is_ident(name))
    })
}
