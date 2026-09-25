//! Port of `v01/typeNodes/*` and `v01/unwrapGenerics.ts`.

use std::collections::HashMap;

use codama_nodes::{
    ArrayTypeNode, BooleanTypeNode, BytesEncoding, BytesTypeNode, CountNode, DefinedTypeLinkNode,
    Docs, EnumEmptyVariantTypeNode, EnumStructVariantTypeNode, EnumTupleVariantTypeNode,
    EnumTypeNode, EnumVariantTypeNode, FixedCountNode, NestedTypeNode, NumberFormat,
    NumberTypeNode, OptionTypeNode, PrefixedCountNode, PublicKeyTypeNode, SizePrefixTypeNode,
    StringTypeNode, StructFieldTypeNode, StructTypeNode, TupleTypeNode, TypeNode,
};
use serde_json::{Map, Value};

use crate::{case::camel, idl::TypeDef, Error};

/// Arguments resolve in the scope of the type being expanded, not the one that
/// wrote them, as in JS.
#[derive(Clone, Default)]
pub(crate) struct Generics<'a> {
    pub types: HashMap<&'a str, &'a TypeDef>,
    const_args: HashMap<String, Value>,
    type_args: HashMap<String, Value>,
}

impl<'a> Generics<'a> {
    /// Generic types never become defined types; each use is expanded inline.
    pub(crate) fn extract(types: &'a [TypeDef]) -> (Vec<&'a TypeDef>, Self) {
        let (generic, plain): (Vec<&TypeDef>, Vec<&TypeDef>) =
            types.iter().partition(|t| t.generics.is_some());

        let generics = Self {
            types: generic.into_iter().map(|t| (t.name.as_str(), t)).collect(),
            ..Self::default()
        };

        (plain, generics)
    }
}

pub(crate) fn type_node(ty: &Value, generics: &Generics<'_>) -> Result<TypeNode, Error> {
    type_node_at(ty, generics, 0)
}

fn type_node_at(ty: &Value, generics: &Generics<'_>, depth: usize) -> Result<TypeNode, Error> {
    if depth > crate::NESTING_LIMIT {
        return Err(Error::RecursionLimit);
    }

    let depth = depth + 1;

    if let Value::String(leaf) = ty
        && let Some(node) = leaf_type_node(leaf)
    {
        return Ok(node);
    }

    let Value::Object(obj) = ty else {
        return Err(unrecognized(ty));
    };

    if let Some(Value::Array(pair)) = obj.get("array")
        && let [item, len] = pair.as_slice()
    {
        let item = type_node_at(item, generics, depth)?;
        let count = array_len(len, generics)?;

        return Ok(array(
            item,
            CountNode::Fixed(FixedCountNode { value: count }),
        ));
    }

    if let Some(item) = obj.get("vec") {
        let item = type_node_at(item, generics, depth)?;

        return Ok(array(
            item,
            CountNode::Prefixed(PrefixedCountNode {
                prefix: u32_prefix(),
            }),
        ));
    }

    if let Some(Value::Object(defined)) = obj.get("defined") {
        return defined_type(defined, generics, depth);
    }

    if let Some(name) = obj.get("generic") {
        let arg = name
            .as_str()
            .and_then(|name| generics.type_args.get(name))
            .and_then(|arg| arg.get("type"))
            .ok_or_else(|| Error::GenericArgMissing(name.to_string()))?;

        return type_node_at(arg, generics, depth);
    }

    let kind = obj.get("kind").and_then(Value::as_str);

    if kind == Some("enum")
        && let Some(variants) = obj.get("variants")
    {
        return enum_type(variants, generics, depth);
    }

    if kind == Some("alias")
        && let Some(value) = obj.get("value")
    {
        return type_node_at(value, generics, depth);
    }

    if let Some(item) = obj.get("option") {
        return option(item, false, generics, depth);
    }

    if let Some(item) = obj.get("coption") {
        return option(item, true, generics, depth);
    }

    if kind == Some("struct") {
        let empty = Vec::new();

        let fields = match obj.get("fields") {
            Some(Value::Array(fields)) => fields,
            None | Some(Value::Null) => &empty,
            Some(_) => return Err(unrecognized(ty)),
        };

        if fields.iter().all(is_named_field) {
            return Ok(TypeNode::Struct(struct_type(fields, generics, depth)?));
        }

        if !fields.iter().any(is_named_field) {
            return Ok(TypeNode::Tuple(tuple_type(fields, generics, depth)?));
        }
    }

    Err(unrecognized(ty))
}

fn leaf_type_node(leaf: &str) -> Option<TypeNode> {
    let number = |format| Some(TypeNode::Number(NumberTypeNode::le(format)));

    match leaf {
        "bool" => Some(TypeNode::Boolean(BooleanTypeNode::default())),
        "pubkey" => Some(TypeNode::PublicKey(PublicKeyTypeNode {})),
        "string" => Some(size_prefixed(TypeNode::String(StringTypeNode::new(
            BytesEncoding::Utf8,
        )))),
        "bytes" => Some(size_prefixed(TypeNode::Bytes(BytesTypeNode {}))),
        "u8" => number(NumberFormat::U8),
        "u16" => number(NumberFormat::U16),
        "u32" => number(NumberFormat::U32),
        "u64" => number(NumberFormat::U64),
        "u128" => number(NumberFormat::U128),
        "i8" => number(NumberFormat::I8),
        "i16" => number(NumberFormat::I16),
        "i32" => number(NumberFormat::I32),
        "i64" => number(NumberFormat::I64),
        "i128" => number(NumberFormat::I128),
        "f32" => number(NumberFormat::F32),
        "f64" => number(NumberFormat::F64),
        "shortU16" => number(NumberFormat::ShortU16),
        _ => None,
    }
}

/// Borsh strings and byte vectors carry a `u32` length prefix.
fn size_prefixed(inner: TypeNode) -> TypeNode {
    TypeNode::SizePrefix(SizePrefixTypeNode {
        r#type: Box::new(inner),
        prefix: Box::new(u32_prefix()),
    })
}

fn u32_prefix() -> NestedTypeNode<NumberTypeNode> {
    NestedTypeNode::Value(NumberTypeNode::le(NumberFormat::U32))
}

fn array(item: TypeNode, count: CountNode) -> TypeNode {
    TypeNode::Array(ArrayTypeNode {
        item: Box::new(item),
        count: Box::new(count),
    })
}

/// A const-generic length goes through JS `parseInt`, which reads a leading run
/// of digits and ignores the rest.
fn array_len(len: &Value, generics: &Generics<'_>) -> Result<u64, Error> {
    if let Some(n) = len.as_u64() {
        return Ok(n);
    }

    let raw = len
        .get("generic")
        .and_then(Value::as_str)
        .and_then(|name| generics.const_args.get(name))
        .and_then(|arg| arg.get("value"))
        .and_then(Value::as_str)
        .ok_or_else(|| Error::InvalidArrayLength(len.to_string()))?;

    let digits: String = raw
        .trim_start()
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();

    digits
        .parse()
        .map_err(|_| Error::InvalidArrayLength(raw.to_string()))
}

/// `coption` is a fixed-size option with a `u32` tag; `option` is a `u8` tag.
fn option(
    item: &Value,
    is_coption: bool,
    generics: &Generics<'_>,
    depth: usize,
) -> Result<TypeNode, Error> {
    let prefix = if is_coption {
        NumberFormat::U32
    } else {
        NumberFormat::U8
    };

    Ok(TypeNode::Option(OptionTypeNode {
        fixed: Some(is_coption),
        item: Box::new(type_node_at(item, generics, depth)?),
        prefix: NestedTypeNode::Value(NumberTypeNode::le(prefix)),
    }))
}

fn defined_type(
    defined: &Map<String, Value>,
    generics: &Generics<'_>,
    depth: usize,
) -> Result<TypeNode, Error> {
    let name = defined
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::UnrecognizedType(Value::Object(defined.clone()).to_string()))?;

    if !defined.contains_key("generics") {
        return Ok(TypeNode::Link(DefinedTypeLinkNode {
            name: camel(name)?,
            program: None,
        }));
    }

    let generic_type = generics
        .types
        .get(name)
        .ok_or_else(|| Error::GenericTypeMissing(name.to_owned()))?;

    let args = defined
        .get("generics")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();

    let mut scope = generics.clone();

    for (i, param) in generic_type.generics.iter().flatten().enumerate() {
        let arg = args.get(i).cloned().unwrap_or(Value::Null);

        if param.kind == "const" {
            scope.const_args.insert(param.name.clone(), arg);
        } else {
            scope.type_args.insert(param.name.clone(), arg);
        }
    }

    let body = generic_type.ty.clone().unwrap_or_else(empty_struct);

    type_node_at(&body, &scope, depth)
}

pub(crate) fn empty_struct() -> Value { serde_json::json!({ "kind": "struct", "fields": [] }) }

fn enum_type(variants: &Value, generics: &Generics<'_>, depth: usize) -> Result<TypeNode, Error> {
    let variants = variants.as_array().ok_or_else(|| unrecognized(variants))?;

    let mut out = Vec::with_capacity(variants.len());

    for variant in variants {
        let name = camel(
            variant
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default(),
        )?;

        let fields = match variant.get("fields") {
            Some(Value::Array(fields)) => fields.as_slice(),
            None | Some(Value::Null) => &[],
            Some(_) => return Err(unrecognized(variant)),
        };

        let Some(first) = fields.first() else {
            out.push(EnumVariantTypeNode::Empty(EnumEmptyVariantTypeNode {
                name,
                discriminator: None,
                display: None,
            }));

            continue;
        };

        // Only the first field decides; a mixed list fails in the struct or
        // tuple conversion below, as it does in JS.
        if is_named_field(first) {
            out.push(EnumVariantTypeNode::Struct(EnumStructVariantTypeNode {
                name,
                discriminator: None,
                r#struct: NestedTypeNode::Value(struct_type(fields, generics, depth)?),
                display: None,
            }));
        } else {
            out.push(EnumVariantTypeNode::Tuple(EnumTupleVariantTypeNode {
                name,
                discriminator: None,
                tuple: NestedTypeNode::Value(tuple_type(fields, generics, depth)?),
                display: None,
            }));
        }
    }

    Ok(TypeNode::Enum(EnumTypeNode {
        variants: out,
        size: NestedTypeNode::Value(NumberTypeNode::le(NumberFormat::U8)),
    }))
}

fn is_named_field(field: &Value) -> bool {
    field.get("name").is_some() && field.get("type").is_some()
}

fn struct_type(
    fields: &[Value],
    generics: &Generics<'_>,
    depth: usize,
) -> Result<StructTypeNode, Error> {
    let mut out = Vec::with_capacity(fields.len());

    for field in fields {
        let (Some(name), Some(ty)) = (field.get("name"), field.get("type")) else {
            return Err(unrecognized(field));
        };

        let docs = match field.get("docs") {
            Some(docs) => crate::idl::docs(docs.clone())?,
            None => Vec::new(),
        };

        out.push(StructFieldTypeNode {
            name: camel(name.as_str().unwrap_or_default())?,
            default_value_strategy: None,
            docs: Docs::from(docs),
            r#type: Box::new(type_node_at(ty, generics, depth)?),
            default_value: Box::new(None),
            display: None,
        });
    }

    Ok(StructTypeNode { fields: out })
}

fn tuple_type(
    items: &[Value],
    generics: &Generics<'_>,
    depth: usize,
) -> Result<TupleTypeNode, Error> {
    let items = items
        .iter()
        .map(|item| type_node_at(item, generics, depth))
        .collect::<Result<_, _>>()?;

    Ok(TupleTypeNode { items })
}

fn unrecognized(ty: &Value) -> Error { Error::UnrecognizedType(ty.to_string()) }
