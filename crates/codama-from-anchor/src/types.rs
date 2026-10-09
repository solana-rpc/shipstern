//! Port of `v01/typeNodes/*` and `v01/unwrapGenerics.ts`.

use std::{
    cell::Cell,
    collections::{HashMap, HashSet},
    rc::Rc,
};

use codama_nodes::{
    ArrayTypeNode, BooleanTypeNode, BytesEncoding, BytesTypeNode, CountNode, DefinedTypeLinkNode,
    Docs, EnumEmptyVariantTypeNode, EnumStructVariantTypeNode, EnumTupleVariantTypeNode,
    EnumTypeNode, EnumVariantTypeNode, FixedCountNode, NestedTypeNode, NumberFormat,
    NumberTypeNode, OptionTypeNode, PrefixedCountNode, PublicKeyTypeNode, SizePrefixTypeNode,
    StringTypeNode, StructFieldTypeNode, StructTypeNode, TupleTypeNode, TypeNode,
};
use serde_json::{Map, Value};

use crate::{case::ident, idl::TypeDef, Error};

/// The IDL's type names, shared by every generic scope.
struct Defs<'a> {
    generic: HashMap<&'a str, &'a TypeDef>,
    plain: HashSet<&'a str>,
    budget: Cell<usize>,
}

/// Arguments bound to the parameters of the type being expanded, resolved in the
/// caller's scope so a parameter name the callee reuses cannot capture them.
pub(crate) struct Generics<'a> {
    defs: Rc<Defs<'a>>,
    const_args: HashMap<String, Value>,
    type_args: HashMap<String, Value>,
}

impl<'a> Generics<'a> {
    /// Generic types never become defined types; each use is expanded inline.
    pub(crate) fn extract(types: &'a [TypeDef]) -> (Vec<&'a TypeDef>, Self) {
        let (generic, plain): (Vec<&TypeDef>, Vec<&TypeDef>) =
            types.iter().partition(|t| t.generics.is_some());

        let defs = Defs {
            generic: generic.into_iter().map(|t| (t.name.as_str(), t)).collect(),
            plain: plain.iter().map(|t| t.name.as_str()).collect(),
            budget: Cell::new(crate::TYPE_NODE_LIMIT),
        };

        (plain, Self::scope(Rc::new(defs)))
    }

    fn scope(defs: Rc<Defs<'a>>) -> Self {
        Self {
            defs,
            const_args: HashMap::new(),
            type_args: HashMap::new(),
        }
    }

    /// Generic arguments are expanded at every use, so nested generics can
    /// grow exponentially within the nesting limit.
    fn spend(&self) -> Result<(), Error> {
        let left = self
            .defs
            .budget
            .get()
            .checked_sub(1)
            .ok_or(Error::TooLarge)?;

        self.defs.budget.set(left);

        Ok(())
    }

    /// A bound argument is copied whole, so the copy costs its size. Copies nest,
    /// so their JSON depth is held to what an IDL file itself can reach.
    fn spend_on(&self, value: &Value, depth: usize) -> Result<(), Error> {
        if depth > crate::NESTING_LIMIT {
            return Err(Error::RecursionLimit);
        }

        self.spend()?;

        match value {
            Value::Object(obj) => obj.values().try_for_each(|v| self.spend_on(v, depth + 1)),
            Value::Array(items) => items.iter().try_for_each(|v| self.spend_on(v, depth + 1)),
            _ => Ok(()),
        }
    }

    /// `arg` with this scope's generic references replaced by what they are
    /// bound to.
    fn resolve(&self, arg: &Value, depth: usize) -> Result<Value, Error> {
        if depth > crate::NESTING_LIMIT {
            return Err(Error::RecursionLimit);
        }

        self.spend()?;

        match arg {
            Value::Object(obj) => {
                if let Some(Value::String(name)) = obj.get("generic") {
                    if let Some(ty) = self.type_args.get(name).and_then(|a| a.get("type")) {
                        self.spend_on(ty, depth)?;

                        return Ok(ty.clone());
                    }

                    if let Some(len) = self.const_args.get(name) {
                        return Ok(Value::from(const_value(len)?));
                    }

                    // Passed through, the callee's own parameter of this name would capture it.
                    return Err(Error::GenericArgMissing(name.clone()));
                }

                // A const argument names an outer const parameter by value.
                if obj.get("kind").and_then(Value::as_str) == Some("const")
                    && let Some(Value::String(name)) = obj.get("value")
                    && let Some(bound) = self.const_args.get(name)
                {
                    self.spend_on(bound, depth)?;

                    return Ok(bound.clone());
                }

                obj.iter()
                    .map(|(key, value)| Ok((key.clone(), self.resolve(value, depth + 1)?)))
                    .collect::<Result<Map<_, _>, _>>()
                    .map(Value::Object)
            },

            Value::Array(items) => items
                .iter()
                .map(|item| self.resolve(item, depth + 1))
                .collect::<Result<Vec<_>, _>>()
                .map(Value::Array),

            other => Ok(other.clone()),
        }
    }
}

pub(crate) fn type_node(ty: &Value, generics: &Generics<'_>) -> Result<TypeNode, Error> {
    type_node_at(ty, generics, 0)
}

fn type_node_at(ty: &Value, generics: &Generics<'_>, depth: usize) -> Result<TypeNode, Error> {
    if depth >= crate::NESTING_LIMIT {
        return Err(Error::RecursionLimit);
    }

    generics.spend()?;

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

    if let Some(target) = alias_target(ty) {
        return type_node_at(target, generics, depth);
    }

    if let Some(item) = obj.get("option") {
        return option(item, false, generics, depth);
    }

    if let Some(item) = obj.get("coption") {
        return option(item, true, generics, depth);
    }

    if kind == Some("struct") {
        let fields = match obj.get("fields") {
            Some(Value::Array(fields)) => fields.as_slice(),
            None | Some(Value::Null) => &[],
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

/// Anchor writes an alias as `{"kind": "type", "alias": T}`; JS 1.5.6 reads only
/// `{"kind": "alias", "value": T}`.
pub(crate) fn alias_target(ty: &Value) -> Option<&Value> {
    match ty.get("kind")?.as_str()? {
        "type" => ty.get("alias"),
        "alias" => ty.get("value"),
        _ => None,
    }
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

/// Solana's `MAX_PERMITTED_DATA_LENGTH`. No account holds a longer array, and the
/// generated parser allocates the whole length before it reads.
const MAX_ARRAY_LEN: u64 = 10 * 1024 * 1024;

fn array_len(len: &Value, generics: &Generics<'_>) -> Result<u64, Error> {
    let n = match len.as_u64() {
        Some(n) => n,
        None => {
            let arg = len
                .get("generic")
                .and_then(Value::as_str)
                .and_then(|name| generics.const_args.get(name))
                .ok_or_else(|| Error::InvalidArrayLength(len.to_string()))?;

            const_value(arg)?
        },
    };

    if n > MAX_ARRAY_LEN {
        return Err(Error::InvalidArrayLength(n.to_string()));
    }

    Ok(n)
}

/// A const argument's value goes through JS `parseInt`, which reads a leading
/// run of digits, in hex after a `0x` prefix, and ignores the rest.
fn const_value(arg: &Value) -> Result<u64, Error> {
    // Anchor passes a const parameter on to another generic as `{"kind": "type", "type": N}`.
    if let Some(len) = arg.get("type").and_then(Value::as_u64) {
        return Ok(len);
    }

    let raw = arg
        .get("value")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::InvalidArrayLength(arg.to_string()))?;

    let unsigned = raw.trim_start();
    let unsigned = unsigned.strip_prefix('+').unwrap_or(unsigned);

    let (digits, radix) = match unsigned
        .strip_prefix("0x")
        .or_else(|| unsigned.strip_prefix("0X"))
    {
        Some(hex) => (hex, 16),
        None => (unsigned, 10),
    };

    let end = digits
        .find(|c: char| !c.is_digit(radix))
        .unwrap_or(digits.len());

    u64::from_str_radix(&digits[..end], radix)
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
        .ok_or_else(|| unrecognized(&Value::Object(defined.clone())))?;

    // A dangling link would only fail later, as a missing type in rustc.
    if !defined.contains_key("generics") {
        if generics.defs.generic.contains_key(name) {
            return Err(Error::GenericArgsMissing(name.to_owned()));
        }

        if !generics.defs.plain.contains(name) {
            return Err(Error::UndefinedType(name.to_owned()));
        }

        return Ok(TypeNode::Link(DefinedTypeLinkNode {
            name: ident(name)?,
            program: None,
        }));
    }

    let generic_type = generics
        .defs
        .generic
        .get(name)
        .ok_or_else(|| Error::GenericTypeMissing(name.to_owned()))?;

    // Generics expand inline, and the renderer panics on an inline enum.
    if generic_type.is_enum() {
        return Err(Error::GenericEnum(name.to_owned()));
    }

    let args = defined
        .get("generics")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();

    let mut scope = Generics::scope(Rc::clone(&generics.defs));

    for (i, param) in generic_type.generics.iter().flatten().enumerate() {
        let arg = match args.get(i) {
            Some(arg) => generics.resolve(arg, 0)?,
            None => Value::Null,
        };

        if param.kind == "const" {
            scope.const_args.insert(param.name.clone(), arg);
        } else {
            scope.type_args.insert(param.name.clone(), arg);
        }
    }

    match &generic_type.ty {
        Some(body) => type_node_at(body, &scope, depth),
        None => type_node_at(&empty_struct(), &scope, depth),
    }
}

pub(crate) fn empty_struct() -> Value { serde_json::json!({ "kind": "struct", "fields": [] }) }

fn enum_type(variants: &Value, generics: &Generics<'_>, depth: usize) -> Result<TypeNode, Error> {
    let variants = variants.as_array().ok_or_else(|| unrecognized(variants))?;

    let mut out = Vec::with_capacity(variants.len());

    for variant in variants {
        // Same as struct fields: a missing name would generate a nameless variant.
        let Some(name) = variant.get("name").and_then(Value::as_str) else {
            return Err(unrecognized(variant));
        };

        let name = ident(name)?;

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
        // JS rejects a non-string name; an empty one fails later in `ident`.
        let (Some(name), Some(ty)) = (field.get("name").and_then(Value::as_str), field.get("type"))
        else {
            return Err(unrecognized(field));
        };

        let docs = match field.get("docs") {
            Some(docs) => crate::idl::docs(docs.clone())?,
            None => Vec::new(),
        };

        out.push(StructFieldTypeNode {
            name: ident(name)?,
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

/// Capped so a bad type deep in a large struct does not flood the `compile_error!`.
fn unrecognized(ty: &Value) -> Error {
    const MAX: usize = 200;

    let mut text = ty.to_string();

    if let Some((end, _)) = text.char_indices().nth(MAX) {
        text.truncate(end);
        text.push_str("...");
    }

    Error::UnrecognizedType(text)
}
