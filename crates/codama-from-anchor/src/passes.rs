//! The `defaultVisitor.ts` passes that reach a generated parser, in JS order;
//! the README has the evidence for the three left out. Traversal covers only
//! the node kinds the v01 mapping emits.

use std::collections::{HashMap, HashSet};

use codama_nodes::{
    BytesTypeNode, CountNode, EnumVariantTypeNode, FixedSizeTypeNode, InstructionArgumentNode,
    InstructionInputValueNode, NestedTypeNode, NumberFormat, PdaSeedNode, PdaValuePda, ProgramNode,
    StructTypeNode, TypeNode,
};

use crate::{case::camel, Error};

pub(crate) fn run(program: &mut ProgramNode) -> Result<(), Error> {
    set_fixed_account_sizes(program);
    unwrap_instruction_args_defined_types(program);
    flatten_instruction_data_arguments(program)?;
    transform_u8_arrays_to_bytes(program);

    Ok(())
}

// --- traversal --------------------------------------------------------------

fn struct_of(nested: &NestedTypeNode<StructTypeNode>) -> Option<&StructTypeNode> {
    match nested {
        NestedTypeNode::Value(s) => Some(s),
        _ => None,
    }
}

fn for_each_child_mut(ty: &mut TypeNode, f: &mut dyn FnMut(&mut TypeNode)) {
    match ty {
        TypeNode::Array(n) => f(&mut n.item),
        TypeNode::FixedSize(n) => f(&mut n.r#type),
        TypeNode::SizePrefix(n) => f(&mut n.r#type),
        TypeNode::Option(n) => f(&mut n.item),

        TypeNode::HiddenPrefix(n) => {
            f(&mut n.r#type);

            for constant in &mut n.prefix {
                f(&mut constant.r#type);
            }
        },

        TypeNode::Struct(s) => struct_children_mut(s, f),
        TypeNode::Tuple(t) => {
            for item in &mut t.items {
                f(item);
            }
        },

        TypeNode::Enum(e) => {
            for variant in &mut e.variants {
                match variant {
                    EnumVariantTypeNode::Struct(v) => {
                        if let NestedTypeNode::Value(s) = &mut v.r#struct {
                            struct_children_mut(s, f);
                        }
                    },
                    EnumVariantTypeNode::Tuple(v) => {
                        if let NestedTypeNode::Value(t) = &mut v.tuple {
                            for item in &mut t.items {
                                f(item);
                            }
                        }
                    },
                    EnumVariantTypeNode::Empty(_) => {},
                }
            }
        },

        _ => {},
    }
}

fn struct_children_mut(s: &mut StructTypeNode, f: &mut dyn FnMut(&mut TypeNode)) {
    for field in &mut s.fields {
        f(&mut field.r#type);
    }
}

fn for_each_child(ty: &TypeNode, f: &mut dyn FnMut(&TypeNode)) {
    match ty {
        TypeNode::Array(n) => f(&n.item),
        TypeNode::FixedSize(n) => f(&n.r#type),
        TypeNode::SizePrefix(n) => f(&n.r#type),
        TypeNode::Option(n) => f(&n.item),

        TypeNode::HiddenPrefix(n) => {
            f(&n.r#type);

            for constant in &n.prefix {
                f(&constant.r#type);
            }
        },

        TypeNode::Struct(s) => s.fields.iter().for_each(|field| f(&field.r#type)),
        TypeNode::Tuple(t) => {
            for item in &t.items {
                f(item);
            }
        },

        TypeNode::Enum(e) => {
            for variant in &e.variants {
                match variant {
                    EnumVariantTypeNode::Struct(v) => {
                        if let NestedTypeNode::Value(s) = &v.r#struct {
                            s.fields.iter().for_each(|field| f(&field.r#type));
                        }
                    },
                    EnumVariantTypeNode::Tuple(v) => {
                        if let NestedTypeNode::Value(t) = &v.tuple {
                            for item in &t.items {
                                f(item);
                            }
                        }
                    },
                    EnumVariantTypeNode::Empty(_) => {},
                }
            }
        },

        _ => {},
    }
}

/// Every root type inside an instruction, including PDA seed types held in
/// account default values.
fn instruction_types_mut(ix: &mut codama_nodes::InstructionNode, f: &mut dyn FnMut(&mut TypeNode)) {
    for arg in ix.arguments.iter_mut().chain(ix.extra_arguments.iter_mut()) {
        f(&mut arg.r#type);
    }

    for account in &mut ix.accounts {
        let Some(InstructionInputValueNode::PdaValue(pda)) = account.default_value.as_mut() else {
            continue;
        };

        let PdaValuePda::Pda(pda) = pda.pda.as_mut() else {
            continue;
        };

        for seed in &mut pda.seeds {
            match seed {
                PdaSeedNode::Variable(v) => f(&mut v.r#type),
                PdaSeedNode::Constant(c) => f(&mut c.r#type),
            }
        }
    }
}

// --- setFixedAccountSizes ---------------------------------------------------

/// Port of `setFixedAccountSizesVisitor`.
fn set_fixed_account_sizes(program: &mut ProgramNode) {
    let defined: HashMap<&str, &TypeNode> = program
        .defined_types
        .iter()
        .map(|t| (t.name.as_str(), t.r#type.as_ref()))
        .collect();

    for account in &mut program.accounts {
        if account.size.is_some() {
            continue;
        }

        let Some(data) = struct_of(&account.data) else {
            continue;
        };

        // One cache per account, as JS builds a fresh visitor each time.
        let mut sizer = ByteSize {
            defined: &defined,
            stack: Vec::new(),
            memo: HashMap::new(),
        };

        account.size = sizer.of_struct(data).map(|size| size as u64);
    }
}

/// Port of `getByteSizeVisitor`: `None` means variable size.
struct ByteSize<'a> {
    defined: &'a HashMap<&'a str, &'a TypeNode>,
    stack: Vec<String>,
    memo: HashMap<String, Option<usize>>,
}

impl ByteSize<'_> {
    fn of_struct(&mut self, s: &StructTypeNode) -> Option<usize> {
        self.sum(s.fields.iter().map(|field| field.r#type.as_ref()))
    }

    fn sum<'t>(&mut self, types: impl IntoIterator<Item = &'t TypeNode>) -> Option<usize> {
        let mut total = 0usize;

        for ty in types {
            total = total.checked_add(self.of(ty)?)?;
        }

        Some(total)
    }

    fn of(&mut self, ty: &TypeNode) -> Option<usize> {
        match ty {
            TypeNode::Number(n) => number_size(n.format),
            TypeNode::Boolean(b) => match &b.size {
                NestedTypeNode::Value(n) => number_size(n.format),
                _ => None,
            },
            TypeNode::PublicKey(_) => Some(32),
            TypeNode::FixedSize(n) => Some(n.size),
            TypeNode::Bytes(_) | TypeNode::String(_) => None,

            TypeNode::SizePrefix(n) => {
                let inner = self.of(&n.r#type)?;

                match n.prefix.as_ref() {
                    NestedTypeNode::Value(p) => inner.checked_add(number_size(p.format)?),
                    _ => None,
                }
            },

            TypeNode::Option(o) => {
                if !o.fixed.unwrap_or(false) {
                    return None;
                }

                let NestedTypeNode::Value(prefix) = &o.prefix else {
                    return None;
                };

                number_size(prefix.format)?.checked_add(self.of(&o.item)?)
            },

            TypeNode::Array(a) => {
                let inner = self.of(&a.item);

                self.array_like(&a.count, inner)
            },

            TypeNode::Struct(s) => self.of_struct(s),
            TypeNode::Tuple(t) => self.sum(t.items.iter()),

            TypeNode::HiddenPrefix(n) => {
                let inner = self.of(&n.r#type)?;
                let prefix = self.sum(n.prefix.iter().map(|c| c.r#type.as_ref()))?;

                inner.checked_add(prefix)
            },

            TypeNode::Enum(e) => {
                let NestedTypeNode::Value(size) = &e.size else {
                    return None;
                };

                let prefix = number_size(size.format)?;

                if e.variants
                    .iter()
                    .all(|v| matches!(v, EnumVariantTypeNode::Empty(_)))
                {
                    return Some(prefix);
                }

                let sizes: Vec<Option<usize>> = e
                    .variants
                    .iter()
                    .map(|variant| match variant {
                        EnumVariantTypeNode::Empty(_) => Some(0),
                        EnumVariantTypeNode::Struct(v) => match &v.r#struct {
                            NestedTypeNode::Value(s) => self.of_struct(s),
                            _ => None,
                        },
                        EnumVariantTypeNode::Tuple(v) => match &v.tuple {
                            NestedTypeNode::Value(t) => self.sum(t.items.iter()),
                            _ => None,
                        },
                    })
                    .collect();

                match sizes.first() {
                    Some(Some(first)) if sizes.iter().all(|s| *s == Some(*first)) => {
                        first.checked_add(prefix)
                    },
                    _ => None,
                }
            },

            TypeNode::Link(link) => {
                let name = link.name.to_string();

                // Assume a cyclic type has no fixed size.
                if self.stack.contains(&name) {
                    return None;
                }

                if let Some(size) = self.memo.get(&name) {
                    return *size;
                }

                let target = *self.defined.get(name.as_str())?;

                self.stack.push(name.clone());
                let size = self.of(target);
                self.stack.pop();

                self.memo.insert(name, size);

                size
            },

            _ => None,
        }
    }

    fn array_like(&mut self, count: &CountNode, inner: Option<usize>) -> Option<usize> {
        match (count, inner) {
            (CountNode::Prefixed(p), Some(0)) => match &p.prefix {
                NestedTypeNode::Value(n) => number_size(n.format),
                _ => None,
            },
            (_, Some(0)) => Some(0),
            (CountNode::Fixed(c), _) if c.value == 0 => Some(0),
            (CountNode::Fixed(c), Some(inner)) => inner.checked_mul(usize::try_from(c.value).ok()?),
            _ => None,
        }
    }
}

fn number_size(format: NumberFormat) -> Option<usize> {
    match format {
        NumberFormat::U8 | NumberFormat::I8 => Some(1),
        NumberFormat::U16 | NumberFormat::I16 => Some(2),
        NumberFormat::U32 | NumberFormat::I32 | NumberFormat::F32 => Some(4),
        NumberFormat::U64 | NumberFormat::I64 | NumberFormat::F64 => Some(8),
        NumberFormat::U128 | NumberFormat::I128 => Some(16),
        NumberFormat::ShortU16 => None,
    }
}

// --- unwrapInstructionArgsDefinedTypes --------------------------------------

#[derive(Default)]
struct Uses {
    total: usize,
    direct_arg: usize,
}

/// Port of `unwrapInstructionArgsDefinedTypesVisitor`. Unlike JS, PDA seeds are
/// not counted as uses because extractPdas never hoists them; a whole-struct
/// seed would be needed to tell the difference.
fn unwrap_instruction_args_defined_types(program: &mut ProgramNode) {
    let mut uses: HashMap<String, Uses> = HashMap::new();

    let mut count = |ty: &TypeNode| count_links(ty, &mut uses);

    for account in &program.accounts {
        if let Some(s) = struct_of(&account.data) {
            s.fields.iter().for_each(|field| count(&field.r#type));
        }
    }

    for event in &program.events {
        count(&event.data);
    }

    for defined in &program.defined_types {
        count(&defined.r#type);
    }

    for constant in &program.constants {
        count(&constant.r#type);
    }

    for ix in &program.instructions {
        for arg in ix.arguments.iter().chain(&ix.extra_arguments) {
            if let TypeNode::Link(link) = arg.r#type.as_ref() {
                let entry = uses.entry(link.name.to_string()).or_default();

                entry.total += 1;
                entry.direct_arg += 1;
            } else {
                count_links(&arg.r#type, &mut uses);
            }
        }
    }

    let (inlined, kept): (Vec<_>, Vec<_>) = std::mem::take(&mut program.defined_types)
        .into_iter()
        .partition(|t| {
            uses.get(t.name.as_str())
                .is_some_and(|u| u.total == 1 && u.direct_arg == 1)
                && !matches!(t.r#type.as_ref(), TypeNode::Enum(_))
        });

    program.defined_types = kept;

    if inlined.is_empty() {
        return;
    }

    let inline: HashMap<String, TypeNode> = inlined
        .into_iter()
        .map(|t| (t.name.to_string(), *t.r#type))
        .collect();

    let mut replace = |ty: &mut TypeNode| inline_links(ty, &inline);

    for account in &mut program.accounts {
        if let NestedTypeNode::Value(s) = &mut account.data {
            struct_children_mut(s, &mut replace);
        }
    }

    for defined in &mut program.defined_types {
        replace(&mut defined.r#type);
    }

    for ix in &mut program.instructions {
        instruction_types_mut(ix, &mut replace);
    }
}

fn count_links(ty: &TypeNode, uses: &mut HashMap<String, Uses>) {
    if let TypeNode::Link(link) = ty {
        uses.entry(link.name.to_string()).or_default().total += 1;

        return;
    }

    for_each_child(ty, &mut |child| count_links(child, uses));
}

fn inline_links(ty: &mut TypeNode, inline: &HashMap<String, TypeNode>) {
    if let TypeNode::Link(link) = ty
        && let Some(body) = inline.get(link.name.as_str())
    {
        *ty = body.clone();
    }

    for_each_child_mut(ty, &mut |child| inline_links(child, inline));
}

// --- flattenInstructionDataArguments ----------------------------------------

/// Port of `flattenInstructionDataArgumentsVisitor`.
fn flatten_instruction_data_arguments(program: &mut ProgramNode) -> Result<(), Error> {
    for ix in &mut program.instructions {
        let mut flattened = Vec::with_capacity(ix.arguments.len());

        for arg in std::mem::take(&mut ix.arguments) {
            let TypeNode::Struct(s) = *arg.r#type else {
                flattened.push(arg);
                continue;
            };

            for field in s.fields {
                flattened.push(InstructionArgumentNode {
                    name: camel(&field.name)?,
                    default_value_strategy: field.default_value_strategy,
                    docs: field.docs,
                    r#type: field.r#type,
                    default_value: Box::new(
                        (*field.default_value).map(InstructionInputValueNode::from),
                    ),
                    display: field.display,
                });
            }
        }

        let mut seen = HashSet::new();

        let mut duplicates: Vec<String> = flattened
            .iter()
            .filter(|a| !seen.insert(a.name.to_string()))
            .map(|a| a.name.to_string())
            .collect();

        if !duplicates.is_empty() {
            duplicates.sort();
            duplicates.dedup();

            return Err(Error::ConflictingFlattenedArguments {
                instruction: ix.name.to_string(),
                names: duplicates,
            });
        }

        ix.arguments = flattened;
    }

    Ok(())
}

// --- transformU8ArraysToBytes -----------------------------------------------

/// Port of `transformU8ArraysToBytesVisitor`.
fn transform_u8_arrays_to_bytes(program: &mut ProgramNode) {
    for account in &mut program.accounts {
        if let NestedTypeNode::Value(s) = &mut account.data {
            struct_children_mut(s, &mut u8_arrays_to_bytes);
        }
    }

    for ix in &mut program.instructions {
        instruction_types_mut(ix, &mut u8_arrays_to_bytes);
    }

    for defined in &mut program.defined_types {
        u8_arrays_to_bytes(&mut defined.r#type);
    }

    for event in &mut program.events {
        u8_arrays_to_bytes(&mut event.data);
    }

    for constant in &mut program.constants {
        u8_arrays_to_bytes(&mut constant.r#type);
    }
}

fn u8_arrays_to_bytes(ty: &mut TypeNode) {
    for_each_child_mut(ty, &mut u8_arrays_to_bytes);

    let TypeNode::Array(array) = ty else {
        return;
    };

    let (TypeNode::Number(item), CountNode::Fixed(count)) =
        (array.item.as_ref(), array.count.as_ref())
    else {
        return;
    };

    if item.format != NumberFormat::U8 {
        return;
    }

    let Ok(size) = usize::try_from(count.value) else {
        return;
    };

    *ty = TypeNode::FixedSize(FixedSizeTypeNode {
        size,
        r#type: Box::new(TypeNode::Bytes(BytesTypeNode {})),
    });
}
