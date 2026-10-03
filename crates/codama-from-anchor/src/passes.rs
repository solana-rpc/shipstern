//! The `defaultVisitor.ts` passes that reach a generated parser, in JS order;
//! the README has the evidence for the four left out. Traversal covers only
//! the node kinds the v01 mapping emits.

use std::collections::{HashMap, HashSet};

use codama_nodes::{
    CountNode, EnumVariantTypeNode, InstructionArgumentNode, InstructionInputValueNode,
    InstructionNode, NestedTypeNode, NumberFormat, PdaSeedNode, PdaValuePda, ProgramNode,
    StructTypeNode, TypeNode,
};

use crate::{case::ident, v01::fixed_bytes, Error, ItemKind};

pub(crate) fn run(program: &mut ProgramNode) -> Result<(), Error> {
    unwrap_instruction_args_defined_types(program);
    flatten_instruction_data_arguments(program)?;
    transform_u8_arrays_to_bytes(program);

    Ok(())
}

/// One walker, expanded as a read and a write version, so the passes that count
/// links and the ones that rewrite types visit the same node kinds.
macro_rules! walker {
    ($children:ident, $fields:ident $(, $mut:tt)?) => {
        fn $children(ty: &$($mut)? TypeNode, f: &mut dyn FnMut(&$($mut)? TypeNode)) {
            match ty {
                TypeNode::Array(n) => f(&$($mut)? n.item),
                TypeNode::FixedSize(n) => f(&$($mut)? n.r#type),
                TypeNode::SizePrefix(n) => f(&$($mut)? n.r#type),
                TypeNode::Option(n) => f(&$($mut)? n.item),

                TypeNode::HiddenPrefix(n) => {
                    f(&$($mut)? n.r#type);

                    for constant in &$($mut)? n.prefix {
                        f(&$($mut)? constant.r#type);
                    }
                },

                TypeNode::Struct(s) => $fields(s, f),
                TypeNode::Tuple(t) => {
                    for item in &$($mut)? t.items {
                        f(item);
                    }
                },

                TypeNode::Enum(e) => {
                    for variant in &$($mut)? e.variants {
                        match variant {
                            EnumVariantTypeNode::Struct(v) => {
                                if let NestedTypeNode::Value(s) = &$($mut)? v.r#struct {
                                    $fields(s, f);
                                }
                            },
                            EnumVariantTypeNode::Tuple(v) => {
                                if let NestedTypeNode::Value(t) = &$($mut)? v.tuple {
                                    for item in &$($mut)? t.items {
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

        fn $fields(s: &$($mut)? StructTypeNode, f: &mut dyn FnMut(&$($mut)? TypeNode)) {
            for field in &$($mut)? s.fields {
                f(&$($mut)? field.r#type);
            }
        }
    };
}

walker!(for_each_child, struct_children);
walker!(for_each_child_mut, struct_children_mut, mut);

/// Every root type inside an instruction, including PDA seed types held in
/// account default values.
fn instruction_types_mut(ix: &mut InstructionNode, f: &mut dyn FnMut(&mut TypeNode)) {
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

#[derive(Default)]
struct Uses {
    total: usize,
    direct_arg: usize,
}

/// Port of `unwrapInstructionArgsDefinedTypesVisitor`.
fn unwrap_instruction_args_defined_types(program: &mut ProgramNode) {
    let mut uses: HashMap<String, Uses> = HashMap::new();

    let mut count = |ty: &TypeNode| count_links(ty, &mut uses);

    for account in &program.accounts {
        if let NestedTypeNode::Value(s) = &account.data {
            struct_children(s, &mut count);
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

    // JS also counts seed types in `program.pdas`, where extractPdas hoists the
    // inline PDAs owned by this program. PDAs stay inline here.
    for account in program.instructions.iter().flat_map(|ix| &ix.accounts) {
        let Some(InstructionInputValueNode::PdaValue(value)) = &*account.default_value else {
            continue;
        };

        let PdaValuePda::Pda(pda) = value.pda.as_ref() else {
            continue;
        };

        // An empty id is falsy in JS, so it counts as this program.
        let owned = pda
            .program_id
            .as_ref()
            .is_none_or(|id| id.is_empty() || *id == program.public_key);

        if !owned {
            continue;
        }

        for seed in &pda.seeds {
            if let PdaSeedNode::Variable(seed) = seed {
                count(&seed.r#type);
            }
        }
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

    // Links in accounts and defined types are all counted, so an inlined type is
    // linked only from an instruction.

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
                    name: ident(&field.name)?,
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

            // Raised after conversion, so `name` is the camel-cased one.
            return Err(Error::ConflictingFlattenedArguments(duplicates)
                .at(ItemKind::Instruction, ix.name.as_str()));
        }

        ix.arguments = flattened;
    }

    Ok(())
}

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

    *ty = fixed_bytes(size);
}
