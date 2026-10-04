use base64::{engine::general_purpose::STANDARD, Engine};
use codama_nodes::{
    CamelCaseString, DiscriminatorNode, Endianness, Number, NumberFormat, TypeNode, ValueNode,
};
use proc_macro2::TokenStream;
use quote::{format_ident, quote};

/// A key that identifies a discriminator for collision detection.
/// Instructions with the same key will match the same discriminator check.
#[derive(PartialEq, Eq, Clone)]
pub(crate) enum DiscriminatorKey {
    Field { offset: usize, bytes: Vec<u8> },
    Size { size: usize },
}

impl DiscriminatorKey {
    /// On-wire discriminator bytes and the offset they appear at; `None` for
    /// size-only discriminators, which have no byte prefix.
    pub(crate) fn to_bytes_offset(&self) -> Option<(Vec<u8>, usize)> {
        match self {
            DiscriminatorKey::Field { offset, bytes } => Some((bytes.clone(), *offset)),
            DiscriminatorKey::Size { .. } => None,
        }
    }
}

/// The bytes a numeric discriminator occupies on the wire: a `u32` index is four
/// bytes, not one. Panics on a value it cannot encode, which surfaces as a compile
/// error rather than a missing dispatch arm.
pub(crate) fn number_discriminator_bytes(ty: &TypeNode, value: u64) -> Vec<u8> {
    let TypeNode::Number(number) = ty else {
        panic!("number discriminator {value} has a non-number type");
    };

    let width = match number.format {
        NumberFormat::U8 | NumberFormat::I8 => 1,
        NumberFormat::U16 | NumberFormat::I16 => 2,
        NumberFormat::U32 | NumberFormat::I32 => 4,
        NumberFormat::U64 | NumberFormat::I64 => 8,
        NumberFormat::U128 | NumberFormat::I128 => 16,
        // A compact-u16 below 0x80 is one byte.
        NumberFormat::ShortU16 if value < 0x80 => 1,
        format => panic!("number discriminator {value} as {format:?} is not supported"),
    };

    let signed = matches!(
        number.format,
        NumberFormat::I8
            | NumberFormat::I16
            | NumberFormat::I32
            | NumberFormat::I64
            | NumberFormat::I128
    );
    let bits = width * 8 - usize::from(signed);
    let value = u128::from(value);

    if bits < 128 && value >> bits != 0 {
        panic!(
            "number discriminator {value} does not fit {:?}",
            number.format
        );
    }

    let mut bytes = value.to_le_bytes()[..width].to_vec();

    if matches!(number.endian, Endianness::Be) {
        bytes.reverse();
    }

    bytes
}

/// Offset and bytes of a discriminator that is a byte prefix; `None` for a size
/// discriminator or one the parser cannot match.
fn prefix_discriminator(
    discriminator: &DiscriminatorNode,
    resolve_field: impl Fn(&CamelCaseString) -> Option<ResolvedFieldDiscriminator>,
) -> Option<(usize, Vec<u8>)> {
    match discriminator {
        DiscriminatorNode::Constant(cn) => {
            let bytes = match cn.constant.value.as_ref() {
                ValueNode::Number(nn) => {
                    let Number::UnsignedInteger(value) = nn.number else {
                        return None;
                    };

                    number_discriminator_bytes(&cn.constant.r#type, value)
                },
                ValueNode::Bytes(bv) => decode_discriminator_field_bytes(bv),
                _ => return None,
            };

            Some((crate::utils::as_index(cn.offset), bytes))
        },

        // Anchor's 8-byte sighash or a Shank number, both a byte prefix.
        DiscriminatorNode::Field(node) => {
            let resolved = resolve_field(&node.name)?;

            if !matches!(
                resolved.r#type,
                TypeNode::FixedSize(_) | TypeNode::Number(_)
            ) {
                return None;
            }

            Some((crate::utils::as_index(node.offset), resolved.bytes?))
        },

        DiscriminatorNode::Size(_) => None,
    }
}

/// Decode discriminator bytes from a codama [`BytesValueNode`](codama_nodes::BytesValueNode).
pub(crate) fn decode_discriminator_field_bytes(bytes: &codama_nodes::BytesValueNode) -> Vec<u8> {
    match bytes.encoding {
        codama_nodes::BytesEncoding::Base16 => {
            let padded = crate::utils::pad_hex(&bytes.data);
            hex::decode(&padded).expect("decode base16")
        },
        codama_nodes::BytesEncoding::Base58 => {
            bs58::decode(&bytes.data).into_vec().expect("decode base58")
        },
        codama_nodes::BytesEncoding::Base64 => STANDARD.decode(&bytes.data).expect("decode base64"),
        codama_nodes::BytesEncoding::Utf8 => bytes.data.as_bytes().to_vec(),
    }
}

struct ResolvedFieldDiscriminator {
    r#type: TypeNode,
    bytes: Option<Vec<u8>>,
}

fn resolve_ix_field(
    ix: &codama_nodes::InstructionNode,
    name: &CamelCaseString,
) -> Option<ResolvedFieldDiscriminator> {
    let field = ix.arguments.iter().find(|f| &f.name == name)?;

    let bytes = match (*field.default_value).as_ref()? {
        codama_nodes::InstructionInputValueNode::BytesValue(b) => {
            Some(decode_discriminator_field_bytes(b))
        },
        codama_nodes::InstructionInputValueNode::NumberValue(nn) => {
            let Number::UnsignedInteger(value) = nn.number else {
                return None;
            };

            Some(number_discriminator_bytes(&field.r#type, value))
        },
        _ => None,
    };

    Some(ResolvedFieldDiscriminator {
        r#type: (*field.r#type).clone(),
        bytes,
    })
}

fn resolve_event_field(
    ev: &codama_nodes::EventNode,
    name: &CamelCaseString,
) -> Option<ResolvedFieldDiscriminator> {
    let struct_node = crate::intermediate_representation::helpers::unwrap_event_struct(&ev.data);

    let field = struct_node.fields.iter().find(|f| &f.name == name)?;

    let bytes = match (*field.default_value).as_ref()? {
        ValueNode::Bytes(b) => Some(decode_discriminator_field_bytes(b)),
        _ => None,
    };

    Some(ResolvedFieldDiscriminator {
        r#type: (*field.r#type).clone(),
        bytes,
    })
}

/// Extract a discriminator key for an instruction (collision detection).
pub(crate) fn extract_ix_discriminator_key(
    ix: &codama_nodes::InstructionNode,
) -> Option<DiscriminatorKey> {
    extract_discriminator_key(&ix.discriminators, |name| resolve_ix_field(ix, name))
}

///
/// Width of the window the match arm compares for a fixed-size discriminator
/// field; a mismatch with the decoded bytes makes the arm unmatchable. `None`
/// for constant-bytes and numeric discriminators, which size it themselves.
///
fn ix_discriminator_slice_width(ix: &codama_nodes::InstructionNode) -> Option<usize> {
    let DiscriminatorNode::Field(node) = ix.discriminators.first()? else {
        return None;
    };

    match resolve_ix_field(ix, &node.name)?.r#type {
        TypeNode::FixedSize(fixed) => Some(fixed.size),
        _ => None,
    }
}

///
/// An event's own discriminators, rebased onto the envelope-stripped layout.
///
/// Both the CPI and log paths pass data without the envelope, so subtracting
/// `payload_offset` from the IDL offsets serves both.
///
/// Example output:
///
/// ```rust, ignore
/// // IDL:      [const e445..@0, const 40c6..@8]
/// // rebased:  [const 40c6..@0]
/// ```
///
/// `None` when the event declares no envelope; its offsets are already stripped.
///
fn rebased_event_discriminators(
    ev: &codama_nodes::EventNode,
    envelope: Option<&crate::parse::CpiEventEnvelope>,
) -> Option<Vec<DiscriminatorNode>> {
    let envelope = envelope?;

    let mut rebased: Vec<_> = ev
        .discriminators
        .iter()
        .filter_map(|discriminator| {
            let DiscriminatorNode::Constant(node) = discriminator else {
                return None;
            };

            // The envelope itself is the constant at offset 0.
            if node.offset == 0 {
                return None;
            }

            let mut node = node.clone();
            node.offset -= envelope.payload_offset as u64;

            Some(DiscriminatorNode::Constant(node))
        })
        .collect();

    rebased.sort_by_key(|discriminator| match discriminator {
        DiscriminatorNode::Constant(node) => crate::utils::as_index(node.offset),
        _ => 0,
    });

    Some(rebased)
}

/// Extract a discriminator key for an event (collision detection).
pub(crate) fn extract_event_discriminator_key(
    ev: &codama_nodes::EventNode,
    envelope: Option<&crate::parse::CpiEventEnvelope>,
) -> Option<DiscriminatorKey> {
    let rebased = rebased_event_discriminators(ev, envelope);

    let discriminators = rebased.as_deref().unwrap_or(&ev.discriminators);

    extract_discriminator_key(discriminators, |name| resolve_event_field(ev, name))
}

/// Extract discriminator info for an instruction (match arm + args deserialization).
pub(crate) fn extract_ix_discriminator_info(
    ix: &codama_nodes::InstructionNode,
    args_ident: &syn::Ident,
    has_args: bool,
    mod_ident: &syn::Ident,
) -> Option<DiscriminatorInfo> {
    extract_discriminator_info(
        &ix.discriminators,
        args_ident,
        has_args,
        mod_ident,
        |name| resolve_ix_field(ix, name),
    )
}

///
/// Extract discriminator info for an event (match arm + args deserialization).
///
/// A rebased chain of two or more discriminators is combined with `&&` and the
/// payload read after the last one. A single discriminator delegates to the
/// same code the pre-chain path ran, so IDLs declaring no envelope are
/// behaviourally unchanged; generated tokens are not diffed.
///
pub(crate) fn extract_event_discriminator_info(
    ev: &codama_nodes::EventNode,
    args_ident: &syn::Ident,
    has_args: bool,
    mod_ident: &syn::Ident,
    envelope: Option<&crate::parse::CpiEventEnvelope>,
) -> Option<DiscriminatorInfo> {
    let Some(rebased) = rebased_event_discriminators(ev, envelope) else {
        return extract_discriminator_info(
            &ev.discriminators,
            args_ident,
            has_args,
            mod_ident,
            |name| resolve_event_field(ev, name),
        );
    };

    let (last, leading) = rebased.split_last()?;

    // Args start after the last discriminator, so only that one contributes the
    // payload expression; the rest contribute checks alone.
    let tail = extract_discriminator_info(
        std::slice::from_ref(last),
        args_ident,
        has_args,
        mod_ident,
        |name| resolve_event_field(ev, name),
    )?;

    let mut checks = Vec::with_capacity(rebased.len());

    for discriminator in leading {
        let info = extract_discriminator_info(
            std::slice::from_ref(discriminator),
            args_ident,
            false,
            mod_ident,
            |name| resolve_event_field(ev, name),
        )?;

        checks.push(info.check);
    }

    checks.push(tail.check);

    Some(DiscriminatorInfo {
        args_expr: tail.args_expr,
        check: quote! { #((#checks))&&* },
    })
}

fn extract_discriminator_key(
    discriminators: &[DiscriminatorNode],
    resolve_field: impl Fn(&CamelCaseString) -> Option<ResolvedFieldDiscriminator>,
) -> Option<DiscriminatorKey> {
    let discriminator = discriminators.first()?;

    if let DiscriminatorNode::Size(sn) = discriminator {
        return Some(DiscriminatorKey::Size {
            size: crate::utils::as_index(sn.size),
        });
    }

    let (offset, bytes) = prefix_discriminator(discriminator, resolve_field)?;

    Some(DiscriminatorKey::Field { offset, bytes })
}

/// Information extracted from a discriminator that's needed by both the match arm and helper fn.
pub(crate) struct DiscriminatorInfo {
    /// TokenStream for the args expression inside the helper fn body.
    /// `None` when the instruction has no arguments.
    pub(crate) args_expr: Option<TokenStream>,
    /// TokenStream for the discriminator check in the match arm.
    pub(crate) check: TokenStream,
}

fn extract_discriminator_info(
    discriminators: &[DiscriminatorNode],
    args_ident: &syn::Ident,
    has_args: bool,
    mod_ident: &syn::Ident,
    resolve_field: impl Fn(&CamelCaseString) -> Option<ResolvedFieldDiscriminator>,
) -> Option<DiscriminatorInfo> {
    let discriminator = discriminators.first()?;

    // Discriminator by total size only
    if let DiscriminatorNode::Size(sn) = discriminator {
        let size = crate::utils::unsuffixed(sn.size);

        let args_expr = if has_args {
            Some(quote! {
                {
                    let mut slice: &[u8] = data;

                    <#mod_ident::#args_ident as ::borsh::BorshDeserialize>::deserialize_reader(&mut slice)
                        .map_err(|e| ParseError::Other(e.into()))?
                }
            })
        } else {
            None
        };

        let check = quote! {
            data.len() == #size
        };

        return Some(DiscriminatorInfo { args_expr, check });
    }

    let (offset_at, discriminator_bytes) = prefix_discriminator(discriminator, resolve_field)?;

    let offset = crate::utils::unsuffixed(offset_at as u64);
    let end = crate::utils::unsuffixed((offset_at + discriminator_bytes.len()) as u64);

    let args_expr = has_args.then(|| {
        quote! {
            {
                let mut slice: &[u8] = data.get(#end..).ok_or(ParseError::from("Missing args bytes"))?;

                <#mod_ident::#args_ident as ::borsh::BorshDeserialize>::deserialize_reader(&mut slice)
                    .map_err(|e| ParseError::Other(e.into()))?
            }
        }
    });

    let check = quote! {
        if let Some(slice) = data.get(#offset..#end) {
            slice == &[#(#discriminator_bytes),*]
        } else {
            false
        }
    };

    Some(DiscriminatorInfo { args_expr, check })
}

///
/// Generate a public parse helper function for a single instruction.
///
/// Example output:
/// ```rust, ignore
/// pub fn parse_swap_base_in(
///     accounts: &[::shipstern_core::Pubkey],
///     data: &[u8],
/// ) -> ParseResult<Instructions> {
///     Ok(Instructions {
///         instruction: instruction::Instruction::SwapBaseIn {
///             accounts: instruction::SwapBaseInAccounts { ... },
///             args: <instruction::SwapBaseInArgs as BorshDeserialize>::try_from_slice(...)?,
///         },
///     })
/// }
/// ```
///
fn single_instruction_helper_fn(
    instruction: &codama_nodes::InstructionNode,
    wrapper_ident: &syn::Ident,
) -> Option<TokenStream> {
    let ix_name_pascal = crate::utils::to_pascal_case(&instruction.name);
    let ix_name_snake = crate::utils::to_snake_case(&instruction.name);

    let variant_ident = format_ident!("{}", ix_name_pascal);
    let accounts_ident = format_ident!("{}Accounts", ix_name_pascal);
    let args_ident = format_ident!("{}Args", ix_name_pascal);
    let fn_ident = format_ident!("parse_{}", ix_name_snake);
    let ix_mod = format_ident!("instruction");

    let has_args = !instruction.arguments.is_empty();

    let info = extract_ix_discriminator_info(instruction, &args_ident, has_args, &ix_mod)?;

    let accounts_fields = instruction
        .accounts
        .iter()
        .enumerate()
        .map(|(idx, account)| {
            let field_name = format_ident!("{}", crate::utils::to_snake_case(&account.name));
            let at = crate::utils::unsuffixed(idx as u64);

            if account.is_optional.unwrap_or(false) {
                quote! {
                    #field_name: accounts.get(#at).and_then(|a| {
                        if a == &::shipstern_core::Pubkey::new(PROGRAM_ID) {
                            None
                        } else {
                            Some(*a)
                        }
                    })
                }
            } else {
                let error_msg = format!("Account does not exist at index {idx}");

                quote! { #field_name: *accounts.get(#at).ok_or(ParseError::from(#error_msg))? }
            }
        });

    let num_defined_accounts = crate::utils::unsuffixed(instruction.accounts.len() as u64);

    let has_explicit_remaining = instruction
        .accounts
        .iter()
        .any(|a| crate::utils::to_snake_case(&a.name) == "remaining_accounts");

    let remaining_accounts_field = if has_explicit_remaining {
        quote! {}
    } else {
        quote! {
            remaining_accounts: accounts
                .get(#num_defined_accounts..)
                .unwrap_or_default()
                .to_vec(),
        }
    };

    let accounts_value = quote! {
        instruction::#accounts_ident {
            #(#accounts_fields,)*
            #remaining_accounts_field
        }
    };
    let args_field = info.args_expr.map(|expr| {
        quote! { args: #expr, }
    });

    Some(quote! {
        pub fn #fn_ident(
            accounts: &[::shipstern_core::Pubkey],
            data: &[u8],
        ) -> ParseResult<#wrapper_ident> {
            Ok(#wrapper_ident {
                instruction: instruction::Instruction::#variant_ident {
                    accounts: #accounts_value,
                    #args_field
                },
            })
        }
    })
}

///
/// Generate a discriminator match arm that delegates to the helper function.
///
/// Example output:
/// ```rust, ignore
/// if {
///     if let Some(slice) = data.get(0..1) {
///         slice == &[9]
///     } else {
///         false
///     }
/// } {
///     return parse_swap_base_in(accounts, data);
/// }
/// ```
///
fn single_instruction_match_arm(
    instruction: &codama_nodes::InstructionNode,
) -> Option<TokenStream> {
    let ix_name_snake = crate::utils::to_snake_case(&instruction.name);
    let fn_ident = format_ident!("parse_{}", ix_name_snake);
    let args_ident = format_ident!("{}Args", crate::utils::to_pascal_case(&instruction.name));
    let ix_mod = format_ident!("instruction");

    let has_args = !instruction.arguments.is_empty();

    let info = extract_ix_discriminator_info(instruction, &args_ident, has_args, &ix_mod)?;

    let check = info.check;

    Some(quote! {
        if {
            #check
        } {
            return #fn_ident(accounts, data);
        }
    })
}

///
/// Generate a match arm for a group of instructions sharing the same discriminator.
///
/// Disambiguates by account count: instructions with unique account counts are resolved
/// automatically (highest count first). Instructions sharing both discriminator and account
/// count produce a runtime error directing the user to [`CustomInstructionParser`].
///
pub(crate) fn collision_group_match_arm(
    instructions: &[&codama_nodes::InstructionNode],
) -> TokenStream {
    // Use the first instruction to get the shared discriminator check.
    let first = instructions[0];

    let args_ident = format_ident!("{}Args", crate::utils::to_pascal_case(&first.name));
    let ix_mod = format_ident!("instruction");

    let has_args = !first.arguments.is_empty();

    let info = extract_ix_discriminator_info(first, &args_ident, has_args, &ix_mod)
        .expect("collision group should have valid discriminator");

    let check = info.check;

    // Group by account count (BTreeMap gives us sorted keys).
    let mut by_count: std::collections::BTreeMap<usize, Vec<&codama_nodes::InstructionNode>> =
        std::collections::BTreeMap::new();

    for ix in instructions {
        by_count.entry(ix.accounts.len()).or_default().push(ix);
    }

    let ambiguous_error = |names: &[String]| {
        let msg = format!(
            "Ambiguous instruction: variants [{}] share the same discriminator and account count. \
             Use CustomInstructionParser to disambiguate.",
            names.join(", ")
        );

        quote! {
            return Err(ParseError::from(#msg));
        }
    };

    let mut inner_arms = Vec::new();
    let mut ambiguous: Vec<String> = Vec::new();

    // Iterate from highest to lowest account count.
    for (&count, ixs) in by_count.iter().rev() {
        let count = crate::utils::unsuffixed(count as u64);

        if ixs.len() == 1 {
            let ix_name_snake = crate::utils::to_snake_case(&ixs[0].name);

            let fn_ident = format_ident!("parse_{}", ix_name_snake);

            inner_arms.push(quote! {
                if accounts.len() >= #count {
                    return #fn_ident(accounts, data);
                }
            });
        } else {
            let names: Vec<String> = ixs.iter().map(|ix| ix.name.to_string()).collect();
            let error = ambiguous_error(&names);

            // Stop here: a lower-count member would otherwise parse these accounts as itself.
            inner_arms.push(quote! {
                if accounts.len() >= #count {
                    #error
                }
            });

            ambiguous.extend(names);
        }
    }

    // Only reached below the group's lowest account count.
    let fallback = if !ambiguous.is_empty() {
        ambiguous_error(&ambiguous)
    } else {
        quote! {}
    };

    quote! {
        if {
            #check
        } {
            #(#inner_arms)*
            #fallback
        }
    }
}

pub fn instruction_parser(
    program_name_camel: &CamelCaseString,
    instructions: &[codama_nodes::InstructionNode],
    has_events: bool,
    cpi_event_config: &super::shipstern_parser::CpiEventConfig,
) -> TokenStream {
    let program_name = crate::utils::to_pascal_case(program_name_camel);

    let instruction_parser_id = format!("{}::InstructionParser", program_name);

    let wrapper_ident = format_ident!("Instructions");

    // 1. Per-instruction parse helper functions
    let helper_fns: Vec<TokenStream> = instructions
        .iter()
        .filter_map(|ix| single_instruction_helper_fn(ix, &wrapper_ident))
        .collect();

    // 1b. Per-instruction discriminator constants, exposed on the wrapper type.
    //
    // `to_snake_case().to_uppercase()` folds `ä` and `Ä` together, so a colliding
    // group gets no constants at all, like the width and empty guards. Such an IDL
    // compiles as before, minus the constants.
    let const_name_counts = instructions.iter().fold(
        std::collections::HashMap::<String, usize>::new(),
        |mut counts, ix| {
            *counts
                .entry(crate::utils::to_snake_case(&ix.name).to_uppercase())
                .or_default() += 1;

            counts
        },
    );

    let disc_consts: Vec<TokenStream> = instructions
        .iter()
        .filter_map(|ix| {
            let (bytes, offset) = extract_ix_discriminator_key(ix)?.to_bytes_offset()?;

            if bytes.is_empty() {
                return None;
            }

            // Mirror the account-side guard: when the declared field width and the
            // decoded default bytes disagree the generated arm can never match, so
            // expose no constant for a discriminator the parser cannot honor.
            if ix_discriminator_slice_width(ix).is_some_and(|width| width != bytes.len()) {
                return None;
            }

            let base = crate::utils::to_snake_case(&ix.name).to_uppercase();

            if const_name_counts.get(&base) != Some(&1) {
                return None;
            }

            let disc_ident = format_ident!("{}_DISCRIMINATOR", base);
            let offset_ident = format_ident!("{}_DISCRIMINATOR_OFFSET", base);

            let disc_doc = format!(
                "Discriminator bytes that identify the `{}` instruction on the wire.\n\nPairs \
                 with [`Self::{}`] as a memcmp predicate: these bytes appear at that offset in \
                 the raw instruction data. It is not a payload boundary.",
                *ix.name, offset_ident,
            );

            let offset_doc = format!(
                "Byte offset at which [`Self::{}`] begins in the instruction data.",
                disc_ident,
            );

            let offset = crate::utils::unsuffixed(offset as u64);

            Some(quote! {
                #[doc = #disc_doc]
                pub const #disc_ident: &'static [u8] = &[#(#bytes),*];

                #[doc = #offset_doc]
                pub const #offset_ident: usize = #offset;
            })
        })
        .collect();

    // Skip the impl block entirely when no instruction has a byte discriminator
    // (e.g. every instruction is size-discriminated).
    let disc_consts_impl = if disc_consts.is_empty() {
        quote! {}
    } else {
        quote! {
            impl #wrapper_ident {
                #(#disc_consts)*
            }
        }
    };

    // 2. Group instructions by discriminator for collision detection,
    //    then generate match arms per group.
    let mut groups: Vec<(DiscriminatorKey, Vec<&codama_nodes::InstructionNode>)> = Vec::new();

    for ix in instructions {
        if let Some(key) = extract_ix_discriminator_key(ix) {
            if let Some(group) = groups.iter_mut().find(|(k, _)| k == &key) {
                group.1.push(ix);
            } else {
                groups.push((key, vec![ix]));
            }
        }
    }

    let match_arms: Vec<TokenStream> = groups
        .iter()
        .filter_map(|(_, ixs)| {
            if ixs.len() == 1 {
                single_instruction_match_arm(ixs[0])
            } else {
                Some(collision_group_match_arm(ixs))
            }
        })
        .collect();

    // When program-events feature is active and the IDL has events,
    // InstructionParser outputs ProgramEventOutput instead of Instructions.
    let event_ix_tag = cpi_event_config.discriminator.iter().copied();
    let event_payload_offset = crate::utils::unsuffixed(cpi_event_config.payload_offset as u64);

    let instruction_parser_impl = if has_events {
        let output_ident = format_ident!("ProgramEventOutput");

        quote! {
            #[derive(Debug, Copy, Clone)]
            pub struct InstructionParser;

            impl Parser for InstructionParser {
                type Input = ::shipstern_core::instruction::InstructionUpdate;
                type Output = #output_ident;

                fn id(&self) -> std::borrow::Cow<'static, str> {
                    #instruction_parser_id.into()
                }

                fn prefilter(&self) -> Prefilter {
                    Prefilter::builder()
                        .transaction_accounts([PROGRAM_ID])
                        .build()
                        .unwrap()
                }

                async fn parse(
                    &self,
                    ix_update: &::shipstern_core::instruction::InstructionUpdate,
                ) -> ParseResult<Self::Output> {
                    if *ix_update.program != PROGRAM_ID {
                        return Err(ParseError::Filtered);
                    }

                    // Skip standalone CPI events — they are collected by the
                    // parent instruction's parse call below.
                    const EVENT_IX_TAG: &[u8] = &[#(#event_ix_tag),*];

                    if ix_update.data.starts_with(EVENT_IX_TAG) {
                        return Err(ParseError::Filtered);
                    }

                    // 1. Parse the regular instruction. Do not hide errors here: Kafka
                    // fallback routing relies on parser errors to emit raw failed records.
                    let instruction = Some(resolve_instruction_default(
                        &ix_update.accounts,
                        &ix_update.data,
                        &ix_update.path,
                    )?);

                    let mut program_events = Vec::new();

                    // 2. Scan inner instructions for CPI self-invocation events.
                    for inner in &ix_update.inner {
                        const EVENT_PAYLOAD_OFFSET: usize = #event_payload_offset;

                        if inner.data.starts_with(EVENT_IX_TAG)
                            && inner.data.len() >= EVENT_PAYLOAD_OFFSET
                            && *inner.program == PROGRAM_ID
                        {
                            // Strip the CPI event wrapper — the event discriminator
                            // follows at EVENT_PAYLOAD_OFFSET.
                            if let Ok(ev) = resolve_event_default(&inner.accounts, &inner.data[EVENT_PAYLOAD_OFFSET..]) {
                                program_events.push(ev);
                            }
                        }
                    }

                    // 3. Scan logs for "Program data:" events.
                    program_events.extend(resolve_events_from_logs(ix_update.log_messages()));

                    if instruction.is_none() && program_events.is_empty() {
                        return Err(ParseError::Filtered);
                    }

                    Ok(#output_ident { instruction, program_events })
                }
            }

            impl ::shipstern_core::ProgramParser for InstructionParser {
                #[inline]
                fn program_id(&self) -> shipstern_core::Pubkey {
                    shipstern_core::Pubkey::new(PROGRAM_ID)
                }
            }
        }
    } else {
        quote! {
            #[derive(Debug, Copy, Clone)]
            pub struct InstructionParser;

            impl Parser for InstructionParser {
                type Input = ::shipstern_core::instruction::InstructionUpdate;
                type Output = #wrapper_ident;

                fn id(&self) -> std::borrow::Cow<'static, str> {
                    #instruction_parser_id.into()
                }

                fn prefilter(&self) -> Prefilter {
                    Prefilter::builder()
                        .transaction_accounts([PROGRAM_ID])
                        .build()
                        .unwrap()
                }

                async fn parse(
                    &self,
                    ix_update: &::shipstern_core::instruction::InstructionUpdate,
                ) -> ParseResult<Self::Output> {
                    if *ix_update.program != PROGRAM_ID {
                        return Err(ParseError::Filtered);
                    }

                    // Anchor programs emit events as self-CPI instructions whose data
                    // starts with the event tag. These are not real instructions and
                    // would fail discriminator matching, so filter them out.
                    {
                        const EVENT_IX_TAG: &[u8] = &[#(#event_ix_tag),*];

                        if ix_update.data.starts_with(EVENT_IX_TAG) {
                            return Err(ParseError::Filtered);
                        }
                    }

                    resolve_instruction_default(
                        &ix_update.accounts,
                        &ix_update.data,
                        &ix_update.path,
                    )
                }
            }

            impl ::shipstern_core::ProgramParser for InstructionParser {
                #[inline]
                fn program_id(&self) -> shipstern_core::Pubkey {
                    shipstern_core::Pubkey::new(PROGRAM_ID)
                }
            }
        }
    };

    quote! {
        //
        // Per-instruction parse helper functions.
        // Each parses a specific instruction variant from raw accounts and data,
        // without checking the discriminator.
        //

        #(#helper_fns)*

        #disc_consts_impl

        ///
        /// Default instruction resolution using discriminator matching.
        ///
        /// Tries each instruction's discriminator in order and delegates to
        /// the corresponding `parse_*()` helper function.
        ///
        /// Call this from a custom [`InstructionResolver`] to handle
        /// non-ambiguous instructions while overriding specific ones.
        ///
        pub fn resolve_instruction_default(
            accounts: &[::shipstern_core::Pubkey],
            data: &[u8],
            path: &::shipstern_core::instruction::Path,
        ) -> ParseResult<#wrapper_ident> {
            #(#match_arms)*

            Err(ParseError::DiscriminatorNotFound(format!(
                "instruction discriminator not found at path {path:?}"
            )))
        }

        ///
        ///  Trait for customizing instruction resolution logic.
        ///
        /// For programs where several instructions share a discriminator and need
        /// runtime disambiguation. Plug it in with [`CustomInstructionParser`].
        ///
        pub trait InstructionResolver: Send + Sync + std::fmt::Debug + Copy + 'static {
            fn resolve(
                &self,
                accounts: &[::shipstern_core::Pubkey],
                data: &[u8],
                path: &::shipstern_core::instruction::Path,
            ) -> ParseResult<#wrapper_ident>;
        }

        ///
        /// Instruction parser with a custom resolver for ambiguous discriminators.
        ///
        /// Use this instead of [`InstructionParser`] when you need to override
        /// how instructions with shared discriminators are resolved.
        ///
        /// # Example
        ///
        /// ```rust,ignore
        /// #[derive(Debug, Copy, Clone)]
        /// struct MyResolver;
        ///
        /// impl program::InstructionResolver for MyResolver {
        ///     fn resolve(
        ///         &self,
        ///         accounts: &[shipstern_core::Pubkey],
        ///         data: &[u8],
        ///         path: &shipstern_core::instruction::Path,
        ///     ) -> ParseResult<program::Instructions> {
        ///         // Custom disambiguation logic here
        ///         program::resolve_instruction_default(accounts, data, path)
        ///     }
        /// }
        ///
        /// let parser = program::CustomInstructionParser(MyResolver);
        /// ```
        ///
        #[derive(Debug, Copy, Clone)]
        pub struct CustomInstructionParser<R: InstructionResolver>(pub R);

        impl<R: InstructionResolver> Parser for CustomInstructionParser<R> {
            type Input = ::shipstern_core::instruction::InstructionUpdate;
            type Output = #wrapper_ident;

            fn id(&self) -> std::borrow::Cow<'static, str> {
                #instruction_parser_id.into()
            }

            fn prefilter(&self) -> Prefilter {
                Prefilter::builder()
                    .transaction_accounts([PROGRAM_ID])
                    .build()
                    .unwrap()
            }

            async fn parse(
                &self,
                ix_update: &::shipstern_core::instruction::InstructionUpdate,
            ) -> ParseResult<Self::Output> {
                if *ix_update.program != PROGRAM_ID {
                    return Err(ParseError::Filtered);
                }

                self.0.resolve(&ix_update.accounts, &ix_update.data, &ix_update.path)
            }
        }

        impl<R: InstructionResolver> ::shipstern_core::ProgramParser for CustomInstructionParser<R> {
            #[inline]
            fn program_id(&self) -> shipstern_core::Pubkey {
                shipstern_core::Pubkey::new(PROGRAM_ID)
            }
        }

        #instruction_parser_impl
    }
}
