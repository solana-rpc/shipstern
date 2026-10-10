//! Run the `defaultVisitor.ts` passes that change a generated parser.
//! The README lists the omitted passes and their test evidence.

use std::collections::{HashMap, HashSet};

use codama_nodes::{
    CountNode, EnumVariantTypeNode, InstructionArgumentNode, InstructionInputValueNode,
    InstructionNode, NestedTypeNode, NumberFormat, PdaSeedNode, PdaValuePda, ProgramNode,
    PublicKeyValueNode, StructTypeNode, TypeNode,
};

use crate::{
    case::{camel, ident},
    v01::fixed_bytes,
    Error, ItemKind,
};

pub(crate) fn run(program: &mut ProgramNode) -> Result<(), Error> {
    set_instruction_account_default_values(program)?;
    unwrap_instruction_args_defined_types(program);
    flatten_instruction_data_arguments(program)?;
    transform_u8_arrays_to_bytes(program);

    Ok(())
}

/// Port of `setInstructionAccountDefaultValuesVisitor` with `@codama/visitors` 1.11.0
/// `getCommonInstructionAccountDefaultRules`, for the rules that give an address.
fn set_instruction_account_default_values(program: &mut ProgramNode) -> Result<(), Error> {
    for account in program
        .instructions
        .iter_mut()
        .flat_map(|ix| &mut ix.accounts)
    {
        // Every common rule sets `ignoreIfOptional`, which also skips an account
        // that already has a default.
        if account.is_optional.unwrap_or(false) || account.default_value.is_some() {
            continue;
        }

        let Some((public_key, identifier)) = common_account_address(account.name.as_str()) else {
            continue;
        };

        *account.default_value = Some(InstructionInputValueNode::PublicKeyValue(
            PublicKeyValueNode {
                public_key: public_key.to_owned(),
                identifier: identifier.map(camel).transpose()?,
            },
        ));
    }

    Ok(())
}

/// The address rules in JS order, each regex spelled out as the camel-cased names
/// it matches, so the first rule that matches wins as in JS.
fn common_account_address(name: &str) -> Option<(&'static str, Option<&'static str>)> {
    match name {
        "systemProgram" | "splSystemProgram" => {
            Some(("11111111111111111111111111111111", Some("splSystem")))
        },
        "tokenProgram" | "splTokenProgram" => Some((
            "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA",
            Some("splToken"),
        )),
        "ataProgram" | "splAtaProgram" => Some((
            "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL",
            Some("splAssociatedToken"),
        )),
        "tokenMetadataProgram" | "mplTokenMetadataProgram" => Some((
            "metaqbxxUerdq28cj1RbAWkYQm3ybzjb6a8bt518x1s",
            Some("mplTokenMetadata"),
        )),
        "tokenAuthRulesProgram"
        | "mplTokenAuthRulesProgram"
        | "authorizationRulesProgram"
        | "mplAuthorizationRulesProgram"
        | "authRulesProgram"
        | "mplAuthRulesProgram" => Some((
            "auth9SigNpDKz4sJJ1DfCTuZrZNSAgh9sFD3rboVmgg",
            Some("mplTokenAuthRules"),
        )),
        "candyMachineProgram" | "mplCandyMachineProgram" => Some((
            "CndyV3LdqHUfDLmE5naZjVN8rBZz4tqhdefbAnjHG3JR",
            Some("mplCandyMachine"),
        )),
        "candyGuardProgram" | "mplCandyGuardProgram" => Some((
            "Guard1JwRhJkVH6XZhzoYxeBVQe872VH6QggF4BWmS9g",
            Some("mplCandyGuard"),
        )),

        "clockSysvar" | "sysvarClock" => {
            Some(("SysvarC1ock11111111111111111111111111111111", None))
        },
        "epochScheduleSysvar" | "sysvarEpochSchedule" => {
            Some(("SysvarEpochSchedu1e111111111111111111111111", None))
        },
        "instructionSysvar"
        | "instructionsSysvar"
        | "sysvarInstruction"
        | "sysvarInstructions"
        | "instructionSysvarAccount"
        | "instructionsSysvarAccount"
        | "sysvarInstructionAccount"
        | "sysvarInstructionsAccount" => {
            Some(("Sysvar1nstructions1111111111111111111111111", None))
        },
        "recentBlockhashesSysvar" | "sysvarRecentBlockhashes" => {
            Some(("SysvarRecentB1ockHashes11111111111111111111", None))
        },
        "rent" | "rentSysvar" | "sysvarRent" => {
            Some(("SysvarRent111111111111111111111111111111111", None))
        },
        "rewardsSysvar" | "sysvarRewards" => {
            Some(("SysvarRewards111111111111111111111111111111", None))
        },
        "slotHashesSysvar" | "sysvarSlotHashes" => {
            Some(("SysvarS1otHashes111111111111111111111111111", None))
        },
        "slotHistorySysvar" | "sysvarSlotHistory" => {
            Some(("SysvarS1otHistory11111111111111111111111111", None))
        },
        "stakeHistorySysvar" | "sysvarStakeHistory" => {
            Some(("SysvarStakeHistory1111111111111111111111111", None))
        },

        "mplCoreProgram" => Some((
            "CoREENxT6tW1HoK8ypY1SxRMZTcVPm7R94rH4PZNhX7d",
            Some("mplCore"),
        )),

        _ => None,
    }
}

/// One walker, expanded as a read and a write version, so the passes that count
/// links and the ones that rewrite types visit the same node kinds.
macro_rules! walker {
    ($vis:vis $children:ident, $fields:ident $(, $mut:tt)?) => {
        $vis fn $children(ty: &$($mut)? TypeNode, f: &mut dyn FnMut(&$($mut)? TypeNode)) {
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

walker!(pub(crate) for_each_child, struct_children);
walker!(for_each_child_mut, struct_children_mut, mut);

/// Every root type inside an instruction, including PDA seed types held in
/// account default values.
fn instruction_types_mut(ix: &mut InstructionNode, f: &mut dyn FnMut(&mut TypeNode)) {
    for arg in &mut ix.arguments {
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
        for arg in &ix.arguments {
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
