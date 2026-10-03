//! Implicit `repr(C)` padding in `zero_copy(unsafe)` types. Borsh reads fields
//! back to back, so every field after a padding gap would be misread.

use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
};

use serde_json::{json, Value};

use crate::{
    idl::{Idl, TypeDef},
    Error, ItemKind,
};

/// Insert a `[u8; N]` field wherever `repr(C)` pads a `bytemuckunsafe` struct or a
/// struct nested in one. Fails on a layout it cannot compute or one Borsh also reads.
pub(crate) fn pad_zero_copy(idl: &mut Idl) -> Result<(), Error> {
    let padded: Vec<(usize, Vec<Value>)> = {
        let layouts = Layouts {
            defs: idl.types.iter().map(|t| (t.name.as_str(), t)).collect(),
            known: RefCell::default(),
        };

        let roots: Vec<&TypeDef> = idl.types.iter().filter(|t| is_unsafe_c(t)).collect();

        if let Some(ty) = roots.iter().find(|t| layouts.struct_layout(t, 0).is_none()) {
            return Err(Error::UnknownLayout.at(ItemKind::Type, &ty.name));
        }

        // A nested struct sits in zero-copy memory with its own padding,
        // whatever its `serialization` says.
        let in_zero_copy = reachable(
            &layouts.defs,
            roots.iter().map(|t| t.name.as_str()).collect(),
        );

        idl.types
            .iter()
            .enumerate()
            .filter(|(_, t)| in_zero_copy.contains(t.name.as_str()))
            .filter_map(|(i, t)| Some((i, layouts.padded_fields(t)?)))
            .collect()
    };

    // The one definition serves both reads, and Borsh writes no padding.
    {
        let borsh = borsh_types(idl);

        let shared = padded
            .iter()
            .filter_map(|(i, _)| idl.types.get(*i))
            .find(|t| borsh.contains(t.name.as_str()));

        if let Some(ty) = shared {
            return Err(Error::PaddedBorshType.at(ItemKind::Type, &ty.name));
        }
    }

    for (i, fields) in padded {
        let Some(Value::Object(body)) = idl.types.get_mut(i).and_then(|t| t.ty.as_mut()) else {
            continue;
        };

        body.insert("fields".to_owned(), Value::Array(fields));
    }

    Ok(())
}

/// Instruction arguments, events, Borsh accounts, and every type they reach.
fn borsh_types(idl: &Idl) -> HashSet<&str> {
    let defs: HashMap<&str, &TypeDef> = idl.types.iter().map(|t| (t.name.as_str(), t)).collect();

    let mut stack = Vec::new();

    for arg in idl.instructions.iter().flat_map(|ix| &ix.args) {
        push_defined(&arg.ty, &mut stack);
    }

    stack.extend(idl.events.iter().map(|e| e.name.as_str()));
    stack.extend(
        idl.accounts
            .iter()
            .map(|a| a.name.as_str())
            .filter(|name| defs.get(name).is_some_and(|t| is_borsh(t))),
    );

    reachable(&defs, stack)
}

/// The types in `stack` and every type they link to.
fn reachable<'a>(
    defs: &HashMap<&'a str, &'a TypeDef>,
    mut stack: Vec<&'a str>,
) -> HashSet<&'a str> {
    let mut seen = HashSet::new();

    while let Some(name) = stack.pop() {
        if !seen.insert(name) {
            continue;
        }

        if let Some(ty) = defs.get(name).and_then(|t| t.ty.as_ref()) {
            push_defined(ty, &mut stack);
        }
    }

    seen
}

fn push_defined<'a>(ty: &'a Value, out: &mut Vec<&'a str>) {
    match ty {
        Value::Object(obj) => {
            out.extend(
                obj.get("defined")
                    .and_then(|d| d.get("name"))
                    .and_then(Value::as_str),
            );
            obj.values().for_each(|v| push_defined(v, out));
        },
        Value::Array(items) => items.iter().for_each(|v| push_defined(v, out)),
        _ => {},
    }
}

fn is_borsh(ty: &TypeDef) -> bool {
    ty.serialization
        .as_ref()
        .is_none_or(|s| s.as_str() == Some("borsh"))
}

fn is_unsafe_c(ty: &TypeDef) -> bool {
    let repr = ty.repr.as_ref();

    ty.serialization.as_ref().and_then(Value::as_str) == Some("bytemuckunsafe")
        && repr.and_then(|r| r.get("kind")).and_then(Value::as_str) == Some("c")
        && !is_packed(ty)
}

fn is_packed(ty: &TypeDef) -> bool {
    ty.repr
        .as_ref()
        .and_then(|r| r.get("packed"))
        .and_then(Value::as_bool)
        == Some(true)
}

/// A generic alias has no fixed layout.
fn alias_target(ty: &TypeDef) -> Option<&Value> {
    if ty.generics.is_some() {
        return None;
    }

    crate::types::alias_target(ty.ty.as_ref()?)
}

/// A fieldless enum of 2 to 256 variants is one byte in Rust and in Borsh.
/// Anchor writes `repr(u8)` and `repr(u32)` both as `rust`, so a repr hides the size.
fn is_byte_enum(ty: &TypeDef) -> bool {
    let Some(body) = ty.ty.as_ref() else {
        return false;
    };

    let Some(variants) = body.get("variants").and_then(Value::as_array) else {
        return false;
    };

    ty.repr.is_none()
        && body.get("kind").and_then(Value::as_str) == Some("enum")
        && (2..=256).contains(&variants.len())
        && variants.iter().all(|v| v.get("fields").is_none())
}

fn padding(name: &str, len: u64) -> Value {
    json!({ "name": name, "type": { "array": ["u8", len] } })
}

struct Layout {
    size: u64,
    align: u64,
    /// Padding before each field, in field order.
    pads: Vec<u64>,
    tail: u64,
}

/// The IDL's types, each nested struct's size and alignment computed once:
/// recomputing them per use is exponential in the nesting.
struct Layouts<'a> {
    defs: HashMap<&'a str, &'a TypeDef>,
    known: RefCell<HashMap<&'a str, Option<(u64, u64)>>>,
}

impl<'a> Layouts<'a> {
    /// `None` when there is nothing to insert or the layout is not fully known.
    fn padded_fields(&self, ty: &'a TypeDef) -> Option<Vec<Value>> {
        let fields = ty.ty.as_ref()?.get("fields")?.as_array()?;
        let layout = self.struct_layout(ty, 0)?;

        if layout.tail == 0 && layout.pads.iter().all(|&pad| pad == 0) {
            return None;
        }

        let mut out = Vec::with_capacity(fields.len() + 1);

        for (field, &pad) in fields.iter().zip(&layout.pads) {
            if pad > 0 {
                let name = field.get("name")?.as_str()?;

                out.push(padding(&format!("padding_before_{name}"), pad));
            }

            out.push(field.clone());
        }

        if layout.tail > 0 {
            out.push(padding("padding_end", layout.tail));
        }

        Some(out)
    }

    fn struct_layout(&self, ty: &'a TypeDef, depth: usize) -> Option<Layout> {
        if depth > crate::NESTING_LIMIT || ty.generics.is_some() {
            return None;
        }

        let fields = ty.ty.as_ref()?.get("fields")?.as_array()?;
        let packed = is_packed(ty);

        let (mut offset, mut widest) = (0_u64, 1_u64);
        let mut pads = Vec::with_capacity(fields.len());

        for field in fields {
            let (size, align) = self.field_layout(field.get("type")?, depth)?;
            let align = if packed { 1 } else { align };
            let start = offset.checked_next_multiple_of(align)?;

            pads.push(start - offset);
            offset = start.checked_add(size)?;
            widest = widest.max(align);
        }

        let explicit = ty
            .repr
            .as_ref()
            .and_then(|r| r.get("align"))
            .and_then(Value::as_u64);
        let align = widest.max(explicit.unwrap_or(1));
        let size = offset.checked_next_multiple_of(align)?;

        Some(Layout {
            size,
            align,
            pads,
            tail: size - offset,
        })
    }

    /// Size and alignment on SBF, where 128-bit integers align to 8 (checked
    /// against live `voltr_vault` accounts) and a pubkey is a `[u8; 32]`.
    fn field_layout(&self, ty: &'a Value, depth: usize) -> Option<(u64, u64)> {
        // An alias chain recurses here without passing `struct_layout`'s bound.
        if depth > crate::NESTING_LIMIT {
            return None;
        }

        if let Some(leaf) = ty.as_str() {
            let size = match leaf {
                "bool" | "u8" | "i8" => 1,
                "u16" | "i16" => 2,
                "u32" | "i32" | "f32" => 4,
                "u64" | "i64" | "f64" => 8,
                "u128" | "i128" => 16,
                "pubkey" => 32,
                _ => return None,
            };

            let align = match leaf {
                "pubkey" => 1,
                "u128" | "i128" => 8,
                _ => size,
            };

            return Some((size, align));
        }

        if let Some([item, len]) = ty.get("array").and_then(Value::as_array).map(Vec::as_slice) {
            let (size, align) = self.field_layout(item, depth)?;

            return Some((size.checked_mul(len.as_u64()?)?, align));
        }

        let name = ty.get("defined")?.get("name")?.as_str()?;

        if let Some(&known) = self.known.borrow().get(name) {
            return known;
        }

        // A type that contains itself reads this `None` instead of recursing.
        self.known.borrow_mut().insert(name, None);

        let def = self.defs.get(name)?;

        let layout = if is_byte_enum(def) {
            Some((1, 1))
        } else if let Some(target) = alias_target(def) {
            self.field_layout(target, depth + 1)
        } else {
            self.struct_layout(def, depth + 1)
                .map(|l| (l.size, l.align))
        };

        self.known.borrow_mut().insert(name, layout);

        layout
    }
}
