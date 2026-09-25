//! Port of `nodes-from-anchor/src/v01/*` (the mapping, before any pass runs).

use std::collections::HashSet;

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
    case::{camel, camel_case},
    idl::{self, Idl, InstructionAccountItem, Seed},
    types::{empty_struct, type_node, Generics},
    Error,
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

    let defined_types = types
        .iter()
        .filter(|t| !claimed.contains(t.name.as_str()))
        .map(|t| defined_type_node(t, &generics))
        .collect::<Result<_, _>>()?;

    let accounts = idl
        .accounts
        .iter()
        .map(|a| account_node(a, &types, &generics))
        .collect::<Result<_, _>>()?;

    let constants = idl
        .constants
        .iter()
        .map(|c| constant_node(c, &generics))
        .collect::<Result<_, _>>()?;

    let errors = idl
        .errors
        .iter()
        .map(error_node)
        .collect::<Result<_, _>>()?;

    let events = idl
        .events
        .iter()
        .map(|e| event_node(e, &types, &generics))
        .collect::<Result<_, _>>()?;

    let instructions = idl
        .instructions
        .iter()
        .map(|ix| instruction_node(ix, &generics))
        .collect::<Result<_, _>>()?;

    Ok(ProgramNode {
        name: camel(&idl.metadata.name)?,
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

fn defined_type_node(ty: &idl::TypeDef, generics: &Generics<'_>) -> Result<DefinedTypeNode, Error> {
    let body = ty.ty.clone().unwrap_or_else(empty_struct);

    Ok(DefinedTypeNode {
        name: camel(&ty.name)?,
        docs: Docs::from(ty.docs.clone()),
        r#type: Box::new(type_node(&body, generics)?),
    })
}

fn hex(bytes: &[u8]) -> String { bytes.iter().map(|b| format!("{b:02x}")).collect() }

fn fixed_bytes(len: usize) -> TypeNode {
    TypeNode::FixedSize(FixedSizeTypeNode {
        size: len,
        r#type: Box::new(TypeNode::Bytes(BytesTypeNode {})),
    })
}

fn base16_bytes(bytes: &[u8]) -> BytesValueNode {
    BytesValueNode {
        data: hex(bytes),
        encoding: BytesEncoding::Base16,
    }
}

fn find_type<'a>(types: &[&'a idl::TypeDef], name: &str) -> Option<&'a idl::TypeDef> {
    types.iter().copied().find(|t| t.name == name)
}

fn account_node(
    account: &idl::Account,
    types: &[&idl::TypeDef],
    generics: &Generics<'_>,
) -> Result<AccountNode, Error> {
    let ty = find_type(types, &account.name)
        .ok_or_else(|| Error::AccountTypeMissing(account.name.clone()))?;

    let body = ty.ty.clone().unwrap_or(Value::Null);

    let TypeNode::Struct(data) = type_node(&body, generics)? else {
        return Err(Error::AccountTypeNotStruct(account.name.clone()));
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
        name: camel(&account.name)?,
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
    let ty =
        find_type(types, &event.name).ok_or_else(|| Error::EventTypeMissing(event.name.clone()))?;

    let body = ty.ty.clone().unwrap_or(Value::Null);

    let constant = ConstantValueNode {
        r#type: Box::new(fixed_bytes(event.discriminator.len())),
        value: Box::new(ValueNode::Bytes(base16_bytes(&event.discriminator))),
    };

    Ok(EventNode {
        name: camel(&event.name)?,
        docs: Docs::default(),
        data: Box::new(TypeNode::HiddenPrefix(HiddenPrefixTypeNode {
            r#type: Box::new(type_node(&body, generics)?),
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
        arguments.push(InstructionArgumentNode {
            name: camel(&arg.name)?,
            default_value_strategy: None,
            docs: Docs::from(arg.docs.clone()),
            r#type: Box::new(type_node(&arg.ty, generics)?),
            default_value: Box::new(None),
            display: None,
        });
    }

    Ok(InstructionNode {
        name: camel(&ix.name)?,
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
                let group_prefix = should_prefix.then(|| match prefix {
                    Some(p) => format!("{p}_{}", group.name),
                    None => group.name.clone(),
                });

                out.extend(instruction_accounts(
                    &group.accounts,
                    arguments,
                    group_prefix.as_deref(),
                )?);
            },

            InstructionAccountItem::Single(account) => {
                out.push(instruction_account_node(
                    account,
                    arguments,
                    prefix.filter(|_| should_prefix),
                )?);
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
        (Some(address), _) => Some(InstructionInputValueNode::PublicKeyValue(
            PublicKeyValueNode {
                public_key: address.clone(),
                identifier: Some(camel(&name)?),
            },
        )),

        (None, Some(pda)) => pda_default(pda, &name, arguments, prefix)?,

        (None, None) => None,
    };

    Ok(InstructionAccountNode {
        name: camel(&name)?,
        is_writable: account.writable,
        is_signer: if account.signer {
            IsSigner::True
        } else {
            IsSigner::False
        },
        is_optional: Some(account.optional),
        docs: Docs::from(account.docs.clone()),
        default_value: Box::new(default_value),
        account_link: None,
        display: None,
    })
}

/// `None` when any seed uses a nested path such as `config.authority`: the JS
/// converter skips the whole default then.
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
        let (definition, value) = pda_seed(seed, arguments, prefix)?;

        definitions.push(definition);
        values.extend(value);
    }

    let mut program_id = None;
    let mut program_id_value = None;

    if let Some(program) = &pda.program {
        let (definition, value) = pda_seed(program, arguments, prefix)?;

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

fn pda_seed(
    seed: &Seed,
    arguments: &[InstructionArgumentNode],
    prefix: Option<&str>,
) -> Result<(PdaSeedNode, Option<PdaSeedValueNode>), Error> {
    match seed {
        Seed::Const { value } => Ok((
            PdaSeedNode::Constant(ConstantPdaSeedNode {
                r#type: Box::new(TypeNode::Bytes(BytesTypeNode {})),
                value: Box::new(ConstantPdaSeedValue::Bytes(BytesValueNode {
                    data: bs58::encode(value).into_string(),
                    encoding: BytesEncoding::Base58,
                })),
            }),
            None,
        )),

        Seed::Account { path } => {
            let account = path.split('.').next().unwrap_or_default();
            let name = camel(&prefixed(prefix, account))?;

            Ok((
                PdaSeedNode::Variable(VariablePdaSeedNode {
                    name: name.clone(),
                    docs: Docs::default(),
                    r#type: Box::new(TypeNode::PublicKey(PublicKeyTypeNode {})),
                }),
                Some(PdaSeedValueNode {
                    name: name.clone(),
                    value: Box::new(PdaSeedValueValue::Account(AccountValueNode { name })),
                }),
            ))
        },

        Seed::Arg { path } => {
            let original = path.split('.').next().unwrap_or_default();
            let wanted = camel_case(original);

            let argument = arguments
                .iter()
                .find(|a| a.name.as_str() == wanted)
                .ok_or_else(|| Error::ArgumentTypeMissing(original.to_owned()))?;

            Ok((
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
            ))
        },

        Seed::Unsupported => Err(Error::SeedKindUnimplemented),
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
