use codama_nodes::EventNode;

use crate::intermediate_representation::{
    helpers::build_fields_ir, FieldIr, FieldTypeIr, LabelIr, OneofIr, OneofVariantIr, ScalarIr,
    SchemaIr, TypeIr, TypeKindIr,
};

///
/// Converts Codama `events` into IR messages.
///
/// Events use the same 3-message pattern as instructions:
///
///   1) <EventName>Accounts  → always empty (+ remaining_accounts)
///   2) <EventName>Args      → event data fields
///   3) <EventName>          → wrapper combining accounts + args
///
/// At the end, we create a global dispatch:
///
///   ProgramEvents { oneof event { ... } }
///
/// This mirrors the `Instructions` dispatch for instructions.
///
pub fn build_events_schema(events: &[EventNode], ir: &mut SchemaIr) {
    for ev in events {
        build_event_messages(ev, ir);
    }

    build_event_dispatch_oneof(events, ir);
}

fn build_event_messages(ev: &EventNode, ir: &mut SchemaIr) {
    let ev_name = crate::utils::to_pascal_case(&ev.name);

    let accounts_name = format!("{ev_name}Accounts");
    let args_name = format!("{ev_name}Args");
    let payload_name = ev_name.clone();

    // Events have no accounts — only remaining_accounts.
    let account_fields = vec![FieldIr {
        name: "remaining_accounts".to_string(),
        tag: 1,
        label: LabelIr::Repeated,
        field_type: FieldTypeIr::Scalar(ScalarIr::PublicKey),
    }];

    ir.types.push(TypeIr {
        name: accounts_name.clone(),
        fields: account_fields,
        kind: TypeKindIr::Event,
    });

    let struct_node = crate::intermediate_representation::helpers::unwrap_event_struct(&ev.data);
    let arg_fields = build_fields_ir(
        &args_name,
        &struct_node.fields,
        &ev.discriminators,
        ir,
        TypeKindIr::Helper,
    );

    ir.types.push(TypeIr {
        name: args_name.clone(),
        fields: arg_fields,
        kind: TypeKindIr::Event,
    });

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
        kind: TypeKindIr::Event,
    });
}

fn build_event_dispatch_oneof(events: &[EventNode], ir: &mut SchemaIr) {
    let parent_name = "ProgramEvents".to_string();

    let variants: Vec<OneofVariantIr> = events
        .iter()
        .enumerate()
        .map(|(i, ev)| {
            let ev_name = crate::utils::to_pascal_case(&ev.name);

            OneofVariantIr {
                tag: (i + 1) as u32,
                variant_name: ev_name.clone(),
                message_type: ev_name.clone(),
            }
        })
        .collect();

    ir.oneofs.push(OneofIr {
        parent_message: parent_name,
        field_name: "event".to_string(),
        variants,
        kind: crate::intermediate_representation::OneofKindIr::EventDispatch,
    });
}
