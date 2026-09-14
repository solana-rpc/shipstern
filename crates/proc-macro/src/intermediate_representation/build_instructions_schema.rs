use codama_nodes::InstructionNode;

use crate::intermediate_representation::{
    helpers::build_fields_ir, FieldIr, FieldTypeIr, LabelIr, OneofIr, OneofVariantIr,
    OptionEncodingIr, ScalarIr, SchemaIr, TypeIr, TypeKindIr,
};

///
/// Converts Codama `instructions` into IR messages.
///
/// For each instruction in the IDL, we generate three messages:
///
///   1) <IxName>Accounts  → holds all account pubkeys
///   2) <IxName>Args      → holds instruction arguments
///   3) <IxName>          → wrapper combining accounts + args
///
/// At the end, we also create a global:
///
///   <ProgramName> { oneof instruction { ... } }
///
/// This acts as the root enum for all instructions.
///
/// ─────────────────────────────────────────────────────────────
/// Example IDL instruction:
///
/// ```text
/// instruction open_position {
///   accounts {
///     owner: pubkey,
///     position: pubkey,
///     pool: pubkey,
///   }
///
///   args {
///     collateral: u64,
///     leverage: u32,
///   }
/// }
/// ```
///
/// ─────────────────────────────────────────────────────────────
/// IR result:
///
/// 1) Accounts message
///
/// ```rust, ignore
/// TypeIr {
///   name: "OpenPositionAccounts",
///   fields: [
///     { name: "owner",    tag: 1, field_type: PublicKey },
///     { name: "position", tag: 2, field_type: PublicKey },
///     { name: "pool",     tag: 3, field_type: PublicKey },
///   ],
///   kind: Instruction
/// }
/// ```
///
/// 2) Args message
///
/// ```rust, ignore
/// TypeIr {
///   name: "OpenPositionArgs",
///   fields: [
///     { name: "collateral", tag: 1, field_type: Uint64 },
///     { name: "leverage",   tag: 2, field_type: Uint32 },
///   ],
///   kind: Instruction
/// }
/// ```
///
/// 3) Wrapper payload message
///
/// ```rust, ignore
/// TypeIr {
///   name: "OpenPosition",
///   fields: [
///     { name: "accounts", tag: 1, field_type: Message(OpenPositionAccounts), optional },
///     { name: "args",     tag: 2, field_type: Message(OpenPositionArgs),     optional },
///   ],
///   kind: Instruction
/// }
/// ```
///
/// ─────────────────────────────────────────────────────────────
/// Final global instruction enum (uses the program name, e.g. "Perpetuals"):
///
/// ```rust, ignore
/// OneofIr {
///   parent_message: "Perpetuals",
///   field_name: "instruction",
///   variants: [
///     { tag: 1, variant_name: "OpenPosition", message_type: "OpenPosition" },
///     { tag: 2, variant_name: "SetLimits",    message_type: "SetLimits" },
///     ...
///   ]
/// }
/// ```
///
/// This allows a single protobuf message to represent any instruction.
///
pub fn build_instructions_schema(instructions: &[InstructionNode], ir: &mut SchemaIr) {
    for ix in instructions {
        build_instruction_messages(ix, ir);
    }

    build_instruction_dispatch_oneof(instructions, ir);
}

///
/// Build the three messages for a single instruction:
///   - `<IxName>Accounts`  — one PublicKey field per account
///   - `<IxName>Args`      — instruction arguments (delegates to `build_fields_ir`)
///   - `<IxName>`          — wrapper with optional accounts + args
///
fn build_instruction_messages(ix: &InstructionNode, ir: &mut SchemaIr) {
    let ix_name = crate::utils::to_pascal_case(&ix.name);

    let accounts_name = format!("{ix_name}Accounts");
    let args_name = format!("{ix_name}Args");
    let payload_name = ix_name.clone();

    let mut account_fields: Vec<FieldIr> = ix
        .accounts
        .iter()
        .enumerate()
        .map(|(i, acct)| FieldIr {
            name: crate::utils::to_snake_case(&acct.name),
            tag: (i + 1) as u32,
            label: if acct.is_optional.unwrap_or(false) {
                LabelIr::Optional(OptionEncodingIr::default())
            } else {
                LabelIr::Singular
            },
            field_type: FieldTypeIr::Scalar(ScalarIr::PublicKey),
        })
        .collect();

    // Only add `remaining_accounts` if the IDL doesn't already define one.
    if !account_fields
        .iter()
        .any(|f| f.name == "remaining_accounts")
    {
        account_fields.push(FieldIr {
            name: "remaining_accounts".to_string(),
            tag: (account_fields.len() + 1) as u32,
            label: LabelIr::Repeated,
            field_type: FieldTypeIr::Scalar(ScalarIr::PublicKey),
        });
    }

    // Use `types.push()` instead of `push_unique_type()` because instruction
    // wrapper names (after dropping the `Instruction` suffix) can collide with
    // defined types or accounts that were pushed earlier. We always want the
    // Instruction-kind entry in the schema; the renderer routes types to the
    // correct scope based on `kind`, so duplicates with different kinds are fine.
    ir.types.push(TypeIr {
        name: accounts_name.clone(),
        fields: account_fields,
        kind: TypeKindIr::Instruction,
    });

    let arg_fields = build_fields_ir(&args_name, &ix.arguments, ir, TypeKindIr::Helper);

    // Skip emitting the instruction-kind args wrapper when its name collides
    // with a top-level DefinedType. The proto schema renderer drops duplicate
    // names and keeps the DefinedType's flat shape, so emitting the Rust
    // wrapper here would encode one extra layer of nesting compared to what
    // the schema declares (a schema/encoder mismatch the consumer can't
    // decode). With the wrapper omitted, field resolution in the dispatch
    // wrapper struct (`instruction::<IxName>`) naturally falls back to
    // `super::<args_name>`, and the dispatch module emits a `pub use` to
    // keep `instruction::<args_name>` paths working in parser helper fns.
    let collides_with_defined_type = ir
        .types
        .iter()
        .any(|t| t.name == args_name && t.kind == TypeKindIr::DefinedType);

    if !collides_with_defined_type {
        ir.types.push(TypeIr {
            name: args_name.clone(),
            fields: arg_fields,
            kind: TypeKindIr::Instruction,
        });
    }

    ir.types.push(TypeIr {
        name: payload_name,
        fields: vec![
            FieldIr {
                name: "accounts".to_string(),
                tag: 1,
                label: LabelIr::Singular,
                field_type: FieldTypeIr::Message(accounts_name),
            },
            FieldIr {
                name: "args".to_string(),
                tag: 2,
                label: LabelIr::Singular,
                field_type: FieldTypeIr::Message(args_name),
            },
        ],
        kind: TypeKindIr::Instruction,
    });
}

///
/// Build the `Instructions` message with a `oneof instruction { ... }` that dispatches
/// to each individual `<InstructionName>` payload.
///
fn build_instruction_dispatch_oneof(instructions: &[InstructionNode], ir: &mut SchemaIr) {
    // Nothing to dispatch — skip the oneof entirely. An empty InstructionDispatch
    // would produce an uninhabited `Instruction` enum whose borsh match arms are
    // non-exhaustive (E0004) and whose proto schema is invalid.
    if instructions.is_empty() {
        return;
    }

    let parent_name = "Instructions".to_string();

    let variants: Vec<OneofVariantIr> = instructions
        .iter()
        .enumerate()
        .map(|(i, ix)| {
            let ix_name = crate::utils::to_pascal_case(&ix.name);

            OneofVariantIr {
                tag: (i + 1) as u32,
                variant_name: ix_name.clone(),
                message_type: ix_name.clone(),
            }
        })
        .collect();

    ir.oneofs.push(OneofIr {
        parent_message: parent_name,
        field_name: "instruction".to_string(),
        variants,
        kind: crate::intermediate_representation::OneofKindIr::InstructionDispatch,
    });
}
