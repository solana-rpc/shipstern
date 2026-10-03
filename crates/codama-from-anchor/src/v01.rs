//! Port of `nodes-from-anchor/src/v01/*` (the mapping, before any pass runs).

use std::collections::{HashMap, HashSet};

use codama_nodes::{
    AccountNode, AccountValueNode, ArgumentValueNode, BooleanValueNode, BytesEncoding,
    BytesTypeNode, BytesValueNode, ConstantDiscriminatorNode, ConstantNode, ConstantPdaSeedNode,
    ConstantPdaSeedValue, ConstantValueNode, DefaultValueStrategy, DefinedTypeNode,
    DiscriminatorNode, Docs, ErrorNode, EventNode, FieldDiscriminatorNode, FixedSizeTypeNode,
    HiddenPrefixTypeNode, InstructionAccountNode, InstructionArgumentNode,
    InstructionInputValueNode, InstructionNode, IsSigner, NestedTypeNode, Number, NumberFormat,
    NumberValueNode, OptionalAccountStrategy, PdaNode, PdaSeedNode, PdaSeedValueNode,
    PdaSeedValueValue, PdaValueNode, PdaValuePda, PdaValueProgramId, ProgramNode, ProgramOrigin,
    PublicKeyTypeNode, PublicKeyValueNode, RootNode, StringTypeNode, StringValueNode,
    StructFieldTypeNode, StructTypeNode, TypeNode, ValueNode, VariablePdaSeedNode,
};
use serde_json::Value;

use crate::{
    case::{camel, camel_case, ident},
    idl::{self, Idl, InstructionAccountItem, Seed},
    types::{empty_struct, type_node, Generics},
    Error, ItemKind,
};

/// `CODAMA_VERSION` from `@codama/nodes@1.11.0`, stamped on every root node.
const CODAMA_STANDARD_VERSION: &str = "1.9.2";

pub(crate) fn root_node(idl: &Idl) -> Result<RootNode, Error> {
    Ok(RootNode {
        standard: "codama".to_owned(),
        version: CODAMA_STANDARD_VERSION.to_owned(),
        program: program_node(idl)?,
        additional_programs: Vec::new(),
    })
}

fn program_node(idl: &Idl) -> Result<ProgramNode, Error> {
    let (types, generics) = Generics::extract(&idl.types);

    // Account and event payloads are declared as types; they are not also
    // emitted as defined types.
    let claimed: HashSet<&str> = idl
        .accounts
        .iter()
        .map(|a| a.name.as_str())
        .chain(idl.events.iter().map(|e| e.name.as_str()))
        .collect();

    let unclaimed: Vec<&idl::TypeDef> = types
        .iter()
        .copied()
        .filter(|t| !claimed.contains(t.name.as_str()))
        .collect();

    let defined_types: Vec<DefinedTypeNode> = unclaimed
        .iter()
        .map(|t| defined_type_node(t, &generics).map_err(|e| e.at(ItemKind::Type, &t.name)))
        .collect::<Result<_, _>>()?;

    let accounts = idl
        .accounts
        .iter()
        .map(|a| account_node(a, &types, &generics).map_err(|e| e.at(ItemKind::Account, &a.name)))
        .collect::<Result<_, _>>()?;

    let constants = idl
        .constants
        .iter()
        .map(|c| constant_node(c, &generics).map_err(|e| e.at(ItemKind::Constant, &c.name)))
        .collect::<Result<_, _>>()?;

    let errors = idl
        .errors
        .iter()
        .map(|e| error_node(e).map_err(|err| err.at(ItemKind::Error, &e.name)))
        .collect::<Result<_, _>>()?;

    let events = idl
        .events
        .iter()
        .map(|e| event_node(e, &types, &generics).map_err(|err| err.at(ItemKind::Event, &e.name)))
        .collect::<Result<_, _>>()?;

    let instructions = idl
        .instructions
        .iter()
        .map(|ix| {
            instruction_node(ix, &generics).map_err(|e| e.at(ItemKind::Instruction, &ix.name))
        })
        .collect::<Result<_, _>>()?;

    // After the conversions, so an IDL JS also fails reports what JS does.
    check_names(ItemKind::Type, unclaimed.iter().map(|t| t.name.as_str()))?;
    check_items(
        ItemKind::Account,
        idl.accounts
            .iter()
            .map(|a| (a.name.as_str(), a.discriminator.as_slice())),
    )?;
    check_items(
        ItemKind::Event,
        idl.events
            .iter()
            .map(|e| (e.name.as_str(), e.discriminator.as_slice())),
    )?;
    check_items(
        ItemKind::Instruction,
        idl.instructions
            .iter()
            .map(|ix| (ix.name.as_str(), ix.discriminator.as_slice())),
    )?;

    Ok(ProgramNode {
        name: ident(&idl.metadata.name).map_err(|e| e.at(ItemKind::Program, &idl.metadata.name))?,
        public_key: idl.address.clone(),
        version: idl.metadata.version.clone(),
        origin: Some(ProgramOrigin::Anchor),
        docs: Docs::default(),
        accounts,
        instructions,
        defined_types,
        pdas: Vec::new(),
        events,
        errors,
        constants,
    })
}

/// Names that camel-case alike would generate the same Rust item twice.
fn check_names<'a>(kind: ItemKind, names: impl Iterator<Item = &'a str>) -> Result<(), Error> {
    let mut seen = HashMap::new();

    for name in names {
        if let Some(first) = seen.insert(camel_case(name), name) {
            return Err(Error::NameCollision(first.to_owned()).at(kind, name));
        }
    }

    Ok(())
}

/// The parser tries discriminators in IDL order, so a proper prefix of another can
/// match its data. Equal instructions are told apart by account count, or fail at
/// parse time as ambiguous when the counts match.
fn check_items<'a>(
    kind: ItemKind,
    items: impl Iterator<Item = (&'a str, &'a [u8])>,
) -> Result<(), Error> {
    let items: Vec<_> = items.collect();

    check_names(kind, items.iter().map(|(name, _)| *name))?;

    // The account parser has no collision group, so the first of two equal ones always wins.
    let rejects_equal = kind == ItemKind::Account;

    for (i, (name, discriminator)) in items.iter().enumerate() {
        for (j, (other, other_discriminator)) in items.iter().enumerate() {
            let err = if other_discriminator.len() > discriminator.len()
                && other_discriminator.starts_with(discriminator)
            {
                Error::AmbiguousDiscriminator {
                    discriminator: discriminator.to_vec(),
                    other: (*other).to_owned(),
                }
            } else if rejects_equal && j < i && other_discriminator == discriminator {
                Error::DuplicateDiscriminator((*other).to_owned())
            } else {
                continue;
            };

            return Err(err.at(kind, name));
        }
    }

    Ok(())
}

fn defined_type_node(ty: &idl::TypeDef, generics: &Generics<'_>) -> Result<DefinedTypeNode, Error> {
    let body = ty.ty.clone().unwrap_or_else(empty_struct);

    Ok(DefinedTypeNode {
        name: ident(&ty.name)?,
        docs: Docs::from(ty.docs.clone()),
        r#type: Box::new(type_node(&body, generics)?),
    })
}

pub(crate) fn fixed_bytes(len: usize) -> TypeNode {
    TypeNode::FixedSize(FixedSizeTypeNode {
        size: len,
        r#type: Box::new(TypeNode::Bytes(BytesTypeNode {})),
    })
}

fn base16_bytes(bytes: &[u8]) -> BytesValueNode {
    BytesValueNode {
        data: hex::encode(bytes),
        encoding: BytesEncoding::Base16,
    }
}

/// An account's or event's type. Looked up before the discriminator check, so an
/// IDL wrong in both ways reports what JS does.
fn payload(
    name: &str,
    discriminator: &[u8],
    types: &[&idl::TypeDef],
    generics: &Generics<'_>,
) -> Result<TypeNode, Error> {
    let ty = types
        .iter()
        .find(|t| t.name == name)
        .ok_or(Error::TypeMissing)?;

    if discriminator.is_empty() {
        return Err(Error::EmptyDiscriminator);
    }

    type_node(ty.ty.as_ref().unwrap_or(&Value::Null), generics)
}

fn account_node(
    account: &idl::Account,
    types: &[&idl::TypeDef],
    generics: &Generics<'_>,
) -> Result<AccountNode, Error> {
    let TypeNode::Struct(data) = payload(&account.name, &account.discriminator, types, generics)?
    else {
        return Err(Error::TypeNotStruct);
    };

    let discriminator = StructFieldTypeNode {
        name: camel("discriminator")?,
        default_value_strategy: Some(DefaultValueStrategy::Omitted),
        docs: Docs::default(),
        r#type: Box::new(fixed_bytes(account.discriminator.len())),
        default_value: Box::new(Some(ValueNode::Bytes(base16_bytes(&account.discriminator)))),
        display: None,
    };

    let fields = std::iter::once(discriminator).chain(data.fields).collect();

    Ok(AccountNode {
        name: ident(&account.name)?,
        size: None,
        docs: Docs::default(),
        data: NestedTypeNode::Value(StructTypeNode { fields }),
        pda: None,
        discriminators: vec![field_discriminator()?],
    })
}

fn field_discriminator() -> Result<DiscriminatorNode, Error> {
    Ok(DiscriminatorNode::Field(FieldDiscriminatorNode {
        name: camel("discriminator")?,
        offset: 0,
    }))
}

fn event_node(
    event: &idl::Event,
    types: &[&idl::TypeDef],
    generics: &Generics<'_>,
) -> Result<EventNode, Error> {
    // JS writes any payload, and the `program-events` parser panics on anything but a struct.
    let data @ TypeNode::Struct(_) = payload(&event.name, &event.discriminator, types, generics)?
    else {
        return Err(Error::TypeNotStruct);
    };

    let constant = ConstantValueNode {
        r#type: Box::new(fixed_bytes(event.discriminator.len())),
        value: Box::new(ValueNode::Bytes(base16_bytes(&event.discriminator))),
    };

    Ok(EventNode {
        name: ident(&event.name)?,
        docs: Docs::default(),
        data: Box::new(TypeNode::HiddenPrefix(HiddenPrefixTypeNode {
            r#type: Box::new(data),
            prefix: vec![constant.clone()],
        })),
        discriminators: vec![DiscriminatorNode::Constant(ConstantDiscriminatorNode {
            offset: 0,
            constant,
        })],
    })
}

fn error_node(error: &idl::ErrorCode) -> Result<ErrorNode, Error> {
    let msg = error.msg.clone().unwrap_or_default();

    Ok(ErrorNode {
        name: camel(&error.name)?,
        code: error.code,
        docs: Docs::from(vec![format!("{}: {msg}", error.name)]),
        message: msg,
    })
}

fn constant_node(constant: &idl::Const, generics: &Generics<'_>) -> Result<ConstantNode, Error> {
    let declared = match &constant.ty {
        Some(Value::String(s)) if s == "bytes" => TypeNode::Bytes(BytesTypeNode {}),
        None | Some(Value::Null) => TypeNode::String(StringTypeNode::new(BytesEncoding::Utf8)),
        Some(Value::String(s)) if s.is_empty() => {
            TypeNode::String(StringTypeNode::new(BytesEncoding::Utf8))
        },
        Some(ty) => type_node(ty, generics)?,
    };

    let (ty, value) = constant_value(&constant.value, declared);

    Ok(ConstantNode {
        name: camel(&constant.name)?,
        docs: Docs::default(),
        r#type: Box::new(ty),
        value: Box::new(value),
    })
}

/// Port of `utils.ts` `parseConstantValue`: text that does not parse as the
/// declared type becomes a UTF-8 string constant.
fn constant_value(text: &str, declared: TypeNode) -> (TypeNode, ValueNode) {
    let as_string = || {
        (
            TypeNode::String(StringTypeNode::new(BytesEncoding::Utf8)),
            ValueNode::String(StringValueNode {
                string: text.to_owned(),
            }),
        )
    };

    match &declared {
        TypeNode::Bytes(_) => match serde_json::from_str::<Vec<u8>>(text) {
            Ok(bytes) => (declared, ValueNode::Bytes(base16_bytes(&bytes))),
            Err(_) => as_string(),
        },

        TypeNode::Number(number) => {
            let is_float = matches!(number.format, NumberFormat::F32 | NumberFormat::F64);

            match js_number(text, is_float) {
                Some(n) => (declared, ValueNode::Number(NumberValueNode { number: n })),
                None => as_string(),
            }
        },

        TypeNode::Boolean(_) => match text {
            "true" | "false" => (
                declared,
                ValueNode::Boolean(BooleanValueNode {
                    boolean: text == "true",
                }),
            ),
            _ => as_string(),
        },

        TypeNode::PublicKey(_) => (
            declared,
            ValueNode::PublicKey(PublicKeyValueNode {
                public_key: text.to_owned(),
                identifier: None,
            }),
        ),

        _ => (
            declared,
            ValueNode::String(StringValueNode {
                string: text.to_owned(),
            }),
        ),
    }
}

/// JS `Number()` over `^-?\d+$` (floats also `\.\d+`). Integers stay exact
/// here, where JS rounds past 2^53.
fn js_number(text: &str, is_float: bool) -> Option<Number> {
    let unsigned = text.strip_prefix('-').unwrap_or(text);

    let (int_part, frac_part) = match unsigned.split_once('.') {
        Some((int, frac)) if is_float => (int, Some(frac)),
        Some(_) => return None,
        None => (unsigned, None),
    };

    let all_digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());

    if !all_digits(int_part) || frac_part.is_some_and(|f| !all_digits(f)) {
        return None;
    }

    if frac_part.is_none() {
        if let Ok(n) = text.parse::<u64>() {
            return Some(Number::UnsignedInteger(n));
        }

        if let Ok(n) = text.parse::<i64>() {
            return Some(if n == 0 {
                Number::UnsignedInteger(0)
            } else {
                Number::SignedInteger(n)
            });
        }
    }

    let value: f64 = text.parse().ok()?;

    // A double with no fractional part serializes as an integer in JS.
    if value.fract() == 0.0 && value.abs() < 9_007_199_254_740_992.0 {
        return Some(if value < 0.0 {
            Number::SignedInteger(value as i64)
        } else {
            Number::UnsignedInteger(value as u64)
        });
    }

    Some(Number::Float(value))
}

fn instruction_node(
    ix: &idl::Instruction,
    generics: &Generics<'_>,
) -> Result<InstructionNode, Error> {
    if ix.discriminator.is_empty() {
        return Err(Error::EmptyDiscriminator);
    }

    let discriminator = InstructionArgumentNode {
        name: camel("discriminator")?,
        default_value_strategy: Some(DefaultValueStrategy::Omitted),
        docs: Docs::default(),
        r#type: Box::new(fixed_bytes(ix.discriminator.len())),
        default_value: Box::new(Some(InstructionInputValueNode::BytesValue(base16_bytes(
            &ix.discriminator,
        )))),
        display: None,
    };

    let mut arguments = vec![discriminator];

    for arg in &ix.args {
        let name = ident(&arg.name)?;

        if name.as_str() == "discriminator" {
            return Err(Error::DiscriminatorArgument);
        }

        arguments.push(InstructionArgumentNode {
            name,
            default_value_strategy: None,
            docs: Docs::from(arg.docs.clone()),
            r#type: Box::new(type_node(&arg.ty, generics)?),
            default_value: Box::new(None),
            display: None,
        });
    }

    Ok(InstructionNode {
        name: ident(&ix.name)?,
        docs: Docs::from(ix.docs.clone()),
        optional_account_strategy: Some(OptionalAccountStrategy::ProgramId),
        accounts: instruction_accounts(&ix.accounts, &arguments, None)?,
        arguments,
        discriminators: vec![field_discriminator()?],
        ..InstructionNode::default()
    })
}

/// Group members get the group name as a prefix only when a prefix is already in
/// effect or some camel-cased name repeats anywhere in the list.
fn instruction_accounts(
    items: &[InstructionAccountItem],
    arguments: &[InstructionArgumentNode],
    prefix: Option<&str>,
) -> Result<Vec<InstructionAccountNode>, Error> {
    let should_prefix = prefix.is_some() || has_duplicate_names(items);

    let mut out = Vec::new();

    for item in items {
        match item {
            InstructionAccountItem::Group(group) => {
                let group_prefix = should_prefix.then(|| prefixed(prefix, &group.name));

                out.extend(instruction_accounts(
                    &group.accounts,
                    arguments,
                    group_prefix.as_deref(),
                )?);
            },

            InstructionAccountItem::Single(account) => {
                out.push(instruction_account_node(account, arguments, prefix)?);
            },
        }
    }

    Ok(out)
}

fn has_duplicate_names(items: &[InstructionAccountItem]) -> bool {
    fn walk(items: &[InstructionAccountItem], seen: &mut HashSet<String>) -> bool {
        items.iter().any(|item| match item {
            InstructionAccountItem::Group(group) => walk(&group.accounts, seen),
            InstructionAccountItem::Single(account) => !seen.insert(camel_case(&account.name)),
        })
    }

    walk(items, &mut HashSet::new())
}

fn prefixed(prefix: Option<&str>, name: &str) -> String {
    match prefix {
        Some(p) => format!("{p}_{name}"),
        None => name.to_owned(),
    }
}

fn instruction_account_node(
    account: &idl::InstructionAccount,
    arguments: &[InstructionArgumentNode],
    prefix: Option<&str>,
) -> Result<InstructionAccountNode, Error> {
    let name = prefixed(prefix, &account.name);

    let default_value = match (&account.address, &account.pda) {
        // JS treats an empty address as absent.
        (Some(address), _) if !address.is_empty() => Some(
            InstructionInputValueNode::PublicKeyValue(PublicKeyValueNode {
                public_key: address.clone(),
                identifier: Some(camel(&name)?),
            }),
        ),

        (_, Some(pda)) => pda_default(pda, &name, arguments, prefix)
            .map_err(|e| e.at(ItemKind::Account, &name))?,

        (_, None) => None,
    };

    Ok(InstructionAccountNode {
        name: ident(&name)?,
        is_writable: account.writable,
        is_signer: IsSigner::from(account.signer),
        is_optional: Some(account.optional),
        docs: Docs::from(account.docs.clone()),
        default_value: Box::new(default_value),
        account_link: None,
        display: None,
    })
}

/// `None` when a seed has a nested path like `config.authority` (as in JS) or does
/// not resolve (where JS fails the IDL). The parser never reads PDAs.
fn pda_default(
    pda: &idl::Pda,
    name: &str,
    arguments: &[InstructionArgumentNode],
    prefix: Option<&str>,
) -> Result<Option<InstructionInputValueNode>, Error> {
    if pda
        .seeds
        .iter()
        .any(|s| s.path().is_some_and(|p| p.contains('.')))
    {
        return Ok(None);
    }

    let mut definitions = Vec::with_capacity(pda.seeds.len());
    let mut values = Vec::new();

    for seed in &pda.seeds {
        let Some((definition, value)) = pda_seed(seed, arguments, prefix)? else {
            return Ok(None);
        };

        definitions.push(definition);
        values.extend(value);
    }

    let mut program_id = None;
    let mut program_id_value = None;

    if let Some(program) = &pda.program {
        let Some((definition, value)) = pda_seed(program, arguments, prefix)? else {
            return Ok(None);
        };

        if let PdaSeedNode::Constant(constant) = &definition
            && let ConstantPdaSeedValue::Bytes(bytes) = constant.value.as_ref()
            && bytes.encoding == BytesEncoding::Base58
        {
            program_id = Some(bytes.data.clone());
        } else if let Some(value) = value {
            program_id_value = match *value.value {
                PdaSeedValueValue::Account(account) => Some(PdaValueProgramId::Account(account)),
                PdaSeedValueValue::Argument(argument) => {
                    Some(PdaValueProgramId::Argument(argument))
                },
                _ => None,
            };
        }
    }

    Ok(Some(InstructionInputValueNode::PdaValue(PdaValueNode {
        pda: Box::new(PdaValuePda::Pda(PdaNode {
            name: camel(name)?,
            docs: Docs::default(),
            program_id,
            seeds: definitions,
        })),
        seeds: values,
        program_id: Box::new(program_id_value),
    })))
}

/// `None` for a seed that does not resolve.
fn pda_seed(
    seed: &Seed,
    arguments: &[InstructionArgumentNode],
    prefix: Option<&str>,
) -> Result<Option<(PdaSeedNode, Option<PdaSeedValueNode>)>, Error> {
    match seed {
        Seed::Const { value } => Ok(Some((
            PdaSeedNode::Constant(ConstantPdaSeedNode {
                r#type: Box::new(TypeNode::Bytes(BytesTypeNode {})),
                value: Box::new(ConstantPdaSeedValue::Bytes(BytesValueNode {
                    data: bs58::encode(value).into_string(),
                    encoding: BytesEncoding::Base58,
                })),
            }),
            None,
        ))),

        Seed::Account { path } => {
            let account = path.split('.').next().unwrap_or_default();
            let name = camel(&prefixed(prefix, account))?;

            Ok(Some((
                PdaSeedNode::Variable(VariablePdaSeedNode {
                    name: name.clone(),
                    docs: Docs::default(),
                    r#type: Box::new(TypeNode::PublicKey(PublicKeyTypeNode {})),
                }),
                Some(PdaSeedValueNode {
                    name: name.clone(),
                    value: Box::new(PdaSeedValueValue::Account(AccountValueNode { name })),
                }),
            )))
        },

        Seed::Arg { path } => {
            let wanted = camel_case(path.split('.').next().unwrap_or_default());

            let Some(argument) = arguments.iter().find(|a| a.name.as_str() == wanted) else {
                return Ok(None);
            };

            Ok(Some((
                PdaSeedNode::Variable(VariablePdaSeedNode {
                    name: argument.name.clone(),
                    docs: Docs::default(),
                    r#type: Box::new(seed_type(&argument.r#type)),
                }),
                Some(PdaSeedValueNode {
                    name: argument.name.clone(),
                    value: Box::new(PdaSeedValueValue::Argument(ArgumentValueNode {
                        name: argument.name.clone(),
                    })),
                }),
            )))
        },

        Seed::Unsupported => Ok(None),
    }
}

/// Anchor seeds use a string or byte argument's raw bytes, without the Borsh
/// `u32` length prefix the argument itself is encoded with.
fn seed_type(argument: &TypeNode) -> TypeNode {
    if let TypeNode::SizePrefix(sp) = argument
        && let NestedTypeNode::Value(prefix) = sp.prefix.as_ref()
        && prefix.format == NumberFormat::U32
    {
        match sp.r#type.as_ref() {
            TypeNode::String(s) if s.encoding == BytesEncoding::Utf8 => {
                return TypeNode::String(StringTypeNode::new(BytesEncoding::Utf8));
            },
            TypeNode::Bytes(_) => return TypeNode::Bytes(BytesTypeNode {}),
            _ => {},
        }
    }

    argument.clone()
}
