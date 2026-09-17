use std::{
    collections::{HashMap, HashSet},
    fmt::Write,
};

use crate::intermediate_representation::{
    FieldTypeIr, LabelIr, OneofIr, OneofKindIr, ScalarIr, SchemaIr, TypeIr, TypeKindIr,
};

/// Output of proto schema rendering: the schema string plus dispatch message indices.
pub struct ProtoSchemaOutput {
    pub schema: String,
    /// 0-based index of the `{ProgramName}Account` message in the proto file descriptor.
    /// `None` if the program has no accounts.
    pub account_dispatch_index: Option<usize>,
    /// 0-based index of the `Instructions` message in the proto file descriptor.
    /// `None` if the program has no instructions.
    pub instruction_dispatch_index: Option<usize>,
    /// 0-based index of the `ProgramEventOutput` wrapper message in the proto file descriptor.
    /// `None` if the program has no events.
    pub program_event_output_index: Option<usize>,
}

///
/// Render the IR schema as a .proto string and compute dispatch message indices.
///
/// `program_name` is the PascalCase program name (e.g. "PumpFun").
///
pub fn proto_schema_string(
    schema: &SchemaIr,
    package: &str,
    program_name: &str,
) -> ProtoSchemaOutput {
    let mut out = String::new();
    let mut message_count: usize = 0;

    // Header
    {
        writeln!(&mut out, r#"syntax = "proto3";"#).unwrap();
        writeln!(&mut out).unwrap();

        if !package.is_empty() {
            writeln!(&mut out, "package {};", package).unwrap();
            writeln!(&mut out).unwrap();
        }
    }

    // PublicKey wrapper message
    {
        writeln!(&mut out, "message PublicKey {{").unwrap();
        writeln!(&mut out, "  bytes value = 1;").unwrap();
        writeln!(&mut out, "}}\n    ").unwrap();

        message_count += 1;
    }

    // Build rename map for event types whose names collide with non-event types.
    //
    // Instruction and event types live in separate Rust modules (`instruction::`
    // and `event::`), so there's no collision in Rust. But proto has a flat
    // namespace — without renaming, the second definition gets silently dropped,
    // causing the consumer to decode event payloads using the instruction schema
    // (wrong field structure → "unexpected EOF").
    let proto_rename = build_event_rename_map(schema);

    // Oneof parents are rendered separately below — skip them here to avoid duplicates.
    let oneof_parents: HashSet<&str> = schema
        .oneofs
        .iter()
        .map(|o| o.parent_message.as_str())
        .collect();

    // Render all non-oneof-parent messages
    {
        let mut seen_names: HashSet<String> = HashSet::new();

        for t in &schema.types {
            // Do not render oneof parent messages here, they are rendered separately below and we want to avoid duplicates.
            if oneof_parents.contains(t.name.as_str()) {
                continue;
            }

            // Only apply event renames to Event-kind types.
            let proto_name = if matches!(t.kind, TypeKindIr::Event) {
                resolve_proto_name(&t.name, &proto_rename)
            } else {
                t.name.clone()
            };

            // Skip if we've already rendered a message with the same proto name.
            if !seen_names.insert(proto_name.clone()) {
                continue;
            }

            render_type(&mut out, t, &proto_name, &proto_rename);

            message_count += 1;
        }
    }

    let account_types: Vec<&TypeIr> = schema
        .types
        .iter()
        .filter(|t| matches!(t.kind, TypeKindIr::Account { .. }))
        .collect();

    let account_dispatch_index = if account_types.is_empty() {
        None
    } else {
        let idx = message_count;

        // Use the same wrapper-name collision logic as the Rust code generator:
        // if `{program_name}Account` matches an account type name, fall back to
        // `{program_name}AccountOutput` to avoid a duplicate proto message name.
        let default_wrapper = format!("{program_name}Account");
        // TypeIr names are already in PascalCase, so compare directly.
        let wrapper_name = if account_types.iter().any(|t| t.name == default_wrapper) {
            format!("{program_name}AccountOutput")
        } else {
            default_wrapper
        };

        render_account_dispatch(&mut out, &wrapper_name, &account_types);

        message_count += 1;

        Some(idx)
    };

    let mut instruction_dispatch_index = None;
    let mut has_event_dispatch = false;

    for oneof in &schema.oneofs {
        if oneof.variants.is_empty() {
            continue;
        }

        if oneof.kind == OneofKindIr::InstructionDispatch {
            instruction_dispatch_index = Some(message_count);
        }

        if oneof.kind == OneofKindIr::EventDispatch {
            has_event_dispatch = true;
        }

        render_oneof_parent(&mut out, oneof, &proto_rename);

        message_count += 1;
    }

    // If we have both instructions and events, emit the ProgramEventOutput wrapper.
    let program_event_output_index = if has_event_dispatch && instruction_dispatch_index.is_some() {
        let idx = message_count;

        writeln!(&mut out, "message ProgramEventOutput {{").unwrap();
        writeln!(&mut out, "  optional Instructions instruction = 1;").unwrap();
        writeln!(&mut out, "  repeated ProgramEvents program_events = 2;").unwrap();
        writeln!(&mut out, "}}").unwrap();

        message_count += 1;
        let _ = message_count; // suppress unused warning

        Some(idx)
    } else {
        None
    };

    ProtoSchemaOutput {
        schema: out,
        account_dispatch_index,
        instruction_dispatch_index,
        program_event_output_index,
    }
}

/// Build a rename map for event types that collide with non-event types.
///
/// When an IDL has both an instruction and an event with the same name
/// (e.g. `withdraw`), the generated IR produces two sets of proto messages
/// (`WithdrawAccounts`, `WithdrawArgs`, `Withdraw`) — one for instructions
/// and one for events — with different field structures. In the flat proto
/// namespace, these would collide. We prefix the event versions with `Evt`
/// (e.g. `EvtWithdraw`, `EvtWithdrawAccounts`, `EvtWithdrawArgs`).
fn build_event_rename_map(schema: &SchemaIr) -> HashMap<&str, String> {
    let non_event_names: HashSet<&str> = schema
        .types
        .iter()
        .filter(|t| !matches!(t.kind, TypeKindIr::Event))
        .map(|t| t.name.as_str())
        .collect();

    let mut rename = HashMap::new();

    for t in &schema.types {
        if matches!(t.kind, TypeKindIr::Event) && non_event_names.contains(t.name.as_str()) {
            rename.insert(t.name.as_str(), format!("Evt{}", t.name));
        }
    }

    rename
}

/// Resolve a type name through the rename map.
fn resolve_proto_name(name: &str, rename: &HashMap<&str, String>) -> String {
    rename
        .get(name)
        .cloned()
        .unwrap_or_else(|| name.to_string())
}

fn render_type(out: &mut String, msg: &TypeIr, proto_name: &str, rename: &HashMap<&str, String>) {
    // Only resolve field type references through the rename map for Event types.
    // Instruction types reference instruction sub-messages (original names).
    let apply_field_rename = matches!(msg.kind, TypeKindIr::Event);

    writeln!(out, "message {} {{", proto_name).unwrap();

    for field in &msg.fields {
        let label = match &field.label {
            LabelIr::Singular => "",
            LabelIr::Optional(_) => "optional ",
            LabelIr::Repeated | LabelIr::FixedArray(_) => "repeated ",
        };

        let field_type = match &field.field_type {
            FieldTypeIr::Scalar(s) => scalar_to_proto(s).to_string(),
            FieldTypeIr::Message(name) if apply_field_rename => resolve_proto_name(name, rename),
            FieldTypeIr::Message(name) => name.clone(),
        };

        writeln!(
            out,
            "  {}{} {} = {};",
            label, field_type, field.name, field.tag
        )
        .unwrap();
    }

    writeln!(
        out,
        "}}
    "
    )
    .unwrap();
}

///
/// Render the account dispatch message:
///
/// ```protobuf
/// message PumpFunAccount {
///   oneof account {
///     BondingCurve bonding_curve = 1;
///     Global global = 2;
///     ...
///   }
/// }
/// ```
///
fn render_account_dispatch(out: &mut String, message_name: &str, account_types: &[&TypeIr]) {
    writeln!(out, "message {} {{", message_name).unwrap();
    writeln!(out, "  oneof account {{").unwrap();

    for (i, account) in account_types.iter().enumerate() {
        let tag = i + 1;
        let field_name = crate::utils::to_snake_case(&account.name);

        writeln!(out, "    {} {} = {};", account.name, field_name, tag).unwrap();
    }

    writeln!(out, "  }}").unwrap();
    writeln!(out, "}}").unwrap();
}

fn render_oneof_parent(out: &mut String, oneof: &OneofIr, rename: &HashMap<&str, String>) {
    // Only apply the event rename map to EventDispatch oneofs.
    // Instruction and Enum oneofs reference original (non-renamed) message types.
    let apply_rename = matches!(oneof.kind, OneofKindIr::EventDispatch);

    writeln!(out, "message {} {{", oneof.parent_message).unwrap();
    writeln!(out, "  oneof {} {{", oneof.field_name).unwrap();

    for v in &oneof.variants {
        let field_name = crate::utils::to_snake_case(&v.variant_name);

        let msg_type = if apply_rename {
            resolve_proto_name(&v.message_type, rename)
        } else {
            v.message_type.clone()
        };

        writeln!(out, "    {} {} = {};", msg_type, field_name, v.tag).unwrap();
    }

    writeln!(out, "  }}").unwrap();
    writeln!(out, "}}").unwrap();
}

fn scalar_to_proto(s: &ScalarIr) -> &'static str {
    match s {
        ScalarIr::Bool => "bool",
        ScalarIr::U8 | ScalarIr::U16 | ScalarIr::ShortU16 | ScalarIr::Uint32 => "uint32",
        ScalarIr::Uint64 => "uint64",
        ScalarIr::I8 | ScalarIr::I16 | ScalarIr::Int32 => "int32",
        ScalarIr::Int64 => "int64",
        ScalarIr::Float => "float",
        ScalarIr::Double => "double",
        ScalarIr::String => "string",
        ScalarIr::Bytes
        | ScalarIr::SizePrefixedBytes { .. }
        | ScalarIr::FixedBytes(_)
        | ScalarIr::U128
        | ScalarIr::I128 => "bytes",
        ScalarIr::PublicKey => "PublicKey",
    }
}
