//! Implicit `repr(C)` padding in `zero_copy(unsafe)` types. Borsh reads fields
//! back to back, so every field after a padding gap would be misread.

use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
};

use serde_json::{json, Value};

use crate::{
    case,
    idl::{Idl, Repr, TypeDef},
    Error, ItemKind,
};

/// Insert a `[u8; N]` field wherever `repr(C)` pads a struct in a `zero_copy(unsafe)`
/// account. Fails on a layout it cannot compute or one Borsh also reads.
pub(crate) fn pad_zero_copy(idl: &mut Idl) -> Result<(), Error> {
    let padded: Vec<(usize, Vec<Value>)> = {
        let layouts = Layouts {
            defs: idl.types.iter().map(|t| (t.name.as_str(), t)).collect(),
            known: RefCell::default(),
        };

        // Only accounts are read in place. A `bytemuck` derive checks only its own
        // fields, so an `unsafe` struct inside a `bytemuck` account is a root too.
        let accounts: Vec<&TypeDef> = idl
            .accounts
            .iter()
            .filter_map(|a| layouts.defs.get(a.name.as_str()).copied())
            .filter(|t| !t.is_generic())
            .collect();

        let in_bytemuck = reachable(
            &layouts.defs,
            accounts
                .iter()
                .filter(|t| is_bytemuck(t))
                .map(|t| t.name.as_str())
                .collect(),
        );

        // The IDL drops an enum's `repr`, so steward's `repr(u64)` tag looks like one byte.
        if let Some(ty) = idl
            .types
            .iter()
            .find(|t| in_bytemuck.contains(t.name.as_str()) && t.is_enum())
        {
            return Err(Error::UnknownLayout.at(ItemKind::Type, &ty.name));
        }

        let roots: Vec<&TypeDef> = accounts
            .into_iter()
            .chain(
                idl.types
                    .iter()
                    .filter(|t| in_bytemuck.contains(t.name.as_str())),
            )
            .filter(|t| is_bytemuck_unsafe(t) && !t.is_generic())
            .collect();

        if let Some(ty) = roots.iter().find(|t| layouts.struct_layout(t, 0).is_none()) {
            return Err(Error::UnknownLayout.at(ItemKind::Type, &ty.name));
        }

        // A nested struct keeps its padding in zero-copy memory.
        // Its `serialization` value does not change that layout.
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

fn is_bytemuck(ty: &TypeDef) -> bool {
    ty.serialization.as_ref().and_then(Value::as_str) == Some("bytemuck")
}

fn is_bytemuck_unsafe(ty: &TypeDef) -> bool {
    ty.serialization.as_ref().and_then(Value::as_str) == Some("bytemuckunsafe")
}

/// A generic alias has no fixed layout.
fn alias_target(ty: &TypeDef) -> Option<&Value> {
    if ty.is_generic() {
        return None;
    }

    crate::types::alias_target(ty.ty.as_ref()?)
}

fn padding(name: &str, len: u64) -> Value {
    json!({ "name": name, "type": { "array": ["u8", len] } })
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Shape {
    size: u64,
    align: u64,
}

struct Layout {
    shape: Shape,
    /// Padding before each field, in field order.
    pads: Vec<u64>,
    tail: u64,
}

/// The IDL's types, each nested type's shape computed once: recomputing it per
/// use is exponential in the nesting.
/// Anchor omits `fields` on an empty struct, and the type mapping reads that as
/// empty; the layout walker must agree, or a marker struct hides the whole account.
fn struct_fields(ty: &TypeDef) -> Option<&[Value]> {
    match ty.ty.as_ref()?.as_object()?.get("fields") {
        None | Some(Value::Null) => Some(&[]),
        Some(fields) => fields.as_array().map(Vec::as_slice),
    }
}

struct Layouts<'a> {
    defs: HashMap<&'a str, &'a TypeDef>,
    known: RefCell<HashMap<&'a str, Option<Shape>>>,
}

impl<'a> Layouts<'a> {
    /// `None` when there is nothing to insert or the layout is not fully known.
    fn padded_fields(&self, ty: &'a TypeDef) -> Option<Vec<Value>> {
        let fields = struct_fields(ty)?;
        let layout = self.struct_layout(ty, 0)?;

        if layout.tail == 0 && layout.pads.iter().all(|&pad| pad == 0) {
            return None;
        }

        let mut taken: HashSet<String> = fields
            .iter()
            .filter_map(|field| field.get("name")?.as_str())
            .map(case::camel_case)
            .collect();

        // The renderer renames the later of two equal names, which could be a real field.
        let mut free_name = |base: String| {
            let mut name = base.clone();

            for n in 2.. {
                if taken.insert(case::camel_case(&name)) {
                    break;
                }

                name = format!("{base}_{n}");
            }

            name
        };

        let mut out = Vec::with_capacity(fields.len() + 1);

        for (field, &pad) in fields.iter().zip(&layout.pads) {
            if pad > 0 {
                let name = field.get("name")?.as_str()?;

                out.push(padding(&free_name(format!("padding_before_{name}")), pad));
            }

            out.push(field.clone());
        }

        if layout.tail > 0 {
            out.push(padding(&free_name("padding_end".to_owned()), layout.tail));
        }

        Some(out)
    }

    fn struct_layout(&self, ty: &'a TypeDef, depth: usize) -> Option<Layout> {
        if depth > crate::NESTING_LIMIT || ty.is_generic() {
            return None;
        }

        let fields = struct_fields(ty)?;

        // `repr(Rust)` has no field-order contract, even with `packed`.
        let repr = match ty.repr.as_ref()? {
            Repr::Rust(_) => return None,
            repr => repr,
        };

        let shapes = fields
            .iter()
            .map(|field| {
                // A tuple field is a bare type. Only `repr(transparent)` gives it a
                // layout the parser can use, as a newtype.
                let ty = match field.get("type") {
                    Some(ty) => ty,
                    None if matches!(repr, Repr::Transparent) => field,
                    None => return None,
                };

                self.field_layout(ty, depth)
            })
            .collect::<Option<Vec<_>>>()?;

        if matches!(repr, Repr::Transparent) {
            if shapes
                .iter()
                .any(|shape| shape.size == 0 && shape.align != 1)
            {
                return None;
            }

            let mut nonzero = shapes.iter().copied().filter(|shape| shape.size != 0);
            let shape = nonzero.next()?;

            if nonzero.next().is_some() {
                return None;
            }

            return Some(Layout {
                shape,
                pads: vec![0; shapes.len()],
                tail: 0,
            });
        }

        let Repr::C(modifier) = repr else {
            return None;
        };

        let (mut offset, mut widest) = (0_u64, 1_u64);
        let mut pads = Vec::with_capacity(shapes.len());

        for shape in &shapes {
            // `repr(C, packed)` is `packed(1)`. Anchor writes `packed(N)` the same way.
            let align = if modifier.packed { 1 } else { shape.align };
            let start = offset.checked_next_multiple_of(align)?;

            pads.push(start - offset);
            offset = start.checked_add(shape.size)?;
            widest = widest.max(align);
        }

        let explicit = match modifier.align {
            None => 1,
            // rustc accepts a power of two up to 2^29.
            Some(align) if align.is_power_of_two() && align <= 1 << 29 => {
                u64::try_from(align).ok()?
            },
            Some(_) => return None,
        };
        let align = widest.max(explicit);
        let size = offset.checked_next_multiple_of(align)?;

        Some(Layout {
            shape: Shape { size, align },
            pads,
            tail: size - offset,
        })
    }

    /// Size and alignment on SBF, where 128-bit integers align to 8 (checked
    /// against live `voltr_vault` accounts) and a pubkey is a `[u8; 32]`.
    fn field_layout(&self, ty: &'a Value, depth: usize) -> Option<Shape> {
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

            return Some(Shape { size, align });
        }

        if let Some([item, len]) = ty.get("array").and_then(Value::as_array).map(Vec::as_slice) {
            let item = self.field_layout(item, depth)?;
            let len = len.as_u64()?;

            return Some(Shape {
                size: item.size.checked_mul(len)?,
                align: item.align,
            });
        }

        let name = ty.get("defined")?.get("name")?.as_str()?;

        if let Some(&known) = self.known.borrow().get(name) {
            return known;
        }

        // A type that contains itself reads this `None` instead of recursing.
        self.known.borrow_mut().insert(name, None);

        let def = self.defs.get(name)?;

        let layout = if let Some(target) = alias_target(def) {
            self.field_layout(target, depth + 1)
        } else {
            self.struct_layout(def, depth + 1).map(|l| l.shape)
        };

        self.known.borrow_mut().insert(name, layout);

        layout
    }
}
