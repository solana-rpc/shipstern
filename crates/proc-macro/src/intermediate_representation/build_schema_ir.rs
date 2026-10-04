use std::collections::HashSet;

use codama_nodes::{
    DefinedTypeNode, EnumVariantTypeNode, EventNode, NestedTypeNodeTrait, RootNode, TypeNode,
};

use crate::intermediate_representation::SchemaIr;

/// Build an intermediate representation of the IDL schema that we can use to render .proto and rust code.
///
/// Why use IR? So we can unify the parsing. Then we have one source of truth for both the .proto renderer and Rust code.
pub fn build_schema_ir(idl: &RootNode, events: &[EventNode]) -> SchemaIr {
    let mut ir = SchemaIr::default();

    reserve_direct_message_names(idl, events, &mut ir);

    let mut defined_types = idl.program.defined_types.clone();
    defined_types.extend(linked_event_payloads(idl, events));

    crate::intermediate_representation::build_defined_types(&defined_types, &mut ir);

    crate::intermediate_representation::build_accounts(&idl.program.accounts, &mut ir);

    crate::intermediate_representation::build_instructions_schema(
        &idl.program.instructions,
        &mut ir,
    );

    if cfg!(feature = "program-events") && !events.is_empty() {
        crate::intermediate_representation::build_events_schema(events, &mut ir);
    }

    ir
}

/// Reserve names that are emitted directly by the account, instruction, and
/// event builders before inline helpers are materialized. Helpers are the only
/// generated messages whose names can be changed without changing parser APIs;
/// direct messages must keep their established names.
fn reserve_direct_message_names(idl: &RootNode, events: &[EventNode], ir: &mut SchemaIr) {
    let program_name = crate::utils::to_pascal_case(&idl.program.name);
    let mut names = vec!["PublicKey".to_string()];

    for account in &idl.program.accounts {
        names.push(crate::utils::to_pascal_case(&account.name));
    }

    if !idl.program.accounts.is_empty() {
        let default_wrapper = format!("{program_name}Account");
        let wrapper_name = if idl
            .program
            .accounts
            .iter()
            .any(|account| crate::utils::to_pascal_case(&account.name) == default_wrapper)
        {
            format!("{program_name}AccountOutput")
        } else {
            default_wrapper
        };

        names.push(wrapper_name);
    }

    for instruction in &idl.program.instructions {
        let name = crate::utils::to_pascal_case(&instruction.name);

        names.extend([
            name.clone(),
            format!("{name}Accounts"),
            format!("{name}Args"),
        ]);
    }

    if !idl.program.instructions.is_empty() {
        names.push("Instructions".to_string());
    }

    if cfg!(feature = "program-events") && !events.is_empty() {
        for event in events {
            let name = crate::utils::to_pascal_case(&event.name);

            names.extend([
                name.clone(),
                format!("{name}Accounts"),
                format!("{name}Args"),
            ]);
        }

        names.push("ProgramEvents".to_string());

        if !idl.program.instructions.is_empty() {
            names.push("ProgramEventOutput".to_string());
        }
    }

    ir.reserve_type_names(names);
}

/// Event payloads that a type, account, argument or event links to. Converters keep
/// them out of `definedTypes`, so the link would dangle (#335) or hit an enum helper (#336).
fn linked_event_payloads(idl: &RootNode, events: &[EventNode]) -> Vec<DefinedTypeNode> {
    let program = &idl.program;
    let mut links = HashSet::new();

    let account_fields = program
        .accounts
        .iter()
        .flat_map(|account| &account.data.get_nested_type_node().fields)
        .map(|field| &*field.r#type);

    let arguments = program
        .instructions
        .iter()
        .flat_map(|ix| &ix.arguments)
        .map(|argument| &*argument.r#type);

    program
        .defined_types
        .iter()
        .map(|defined_type| &*defined_type.r#type)
        .chain(account_fields)
        .chain(arguments)
        .chain(events.iter().map(|event| &*event.data))
        .for_each(|root| collect_links(root, &mut links));

    for defined_type in &program.defined_types {
        links.remove(&crate::utils::to_pascal_case(&defined_type.name));
    }

    events
        .iter()
        .filter(|event| links.contains(&crate::utils::to_pascal_case(&event.name)))
        .filter_map(|event| {
            // Skip rather than panic: this also runs without `program-events`.
            let payload =
                crate::intermediate_representation::helpers::event_struct(&event.data).ok()?;

            Some(DefinedTypeNode::new(event.name.clone(), payload.clone()))
        })
        .collect()
}

/// Every type name `ty` links to, through the nodes that `materialize_type`, the
/// enum builder and `unwrap_event_struct` descend into.
fn collect_links(ty: &TypeNode, links: &mut HashSet<String>) {
    match ty {
        TypeNode::Link(link) => {
            links.insert(crate::utils::to_pascal_case(&link.name));
        },

        TypeNode::Struct(s) => {
            for field in &s.fields {
                collect_links(&field.r#type, links);
            }
        },

        TypeNode::Tuple(t) => {
            for item in &t.items {
                collect_links(item, links);
            }
        },

        TypeNode::Enum(e) => {
            for variant in &e.variants {
                match variant {
                    EnumVariantTypeNode::Struct(v) => {
                        for field in &v.r#struct.get_nested_type_node().fields {
                            collect_links(&field.r#type, links);
                        }
                    },
                    EnumVariantTypeNode::Tuple(v) => {
                        for item in &v.tuple.get_nested_type_node().items {
                            collect_links(item, links);
                        }
                    },
                    EnumVariantTypeNode::Empty(_) => {},
                }
            }
        },

        TypeNode::Map(m) => {
            collect_links(&m.key, links);
            collect_links(&m.value, links);
        },

        TypeNode::Option(o) => collect_links(&o.item, links),
        TypeNode::Array(a) => collect_links(&a.item, links),
        TypeNode::Set(s) => collect_links(&s.item, links),
        TypeNode::HiddenPrefix(w) => collect_links(&w.r#type, links),
        TypeNode::HiddenSuffix(w) => collect_links(&w.r#type, links),
        TypeNode::FixedSize(w) => collect_links(&w.r#type, links),
        TypeNode::SizePrefix(w) => collect_links(&w.r#type, links),

        _ => {},
    }
}

#[cfg(test)]
mod tests {
    use codama_nodes::{
        DefinedTypeLinkNode, FixedSizeTypeNode, HiddenSuffixTypeNode, InstructionArgumentNode,
        InstructionNode, MapTypeNode, NumberTypeNode, ProgramNode, SetTypeNode, SizePrefixTypeNode,
        StructFieldTypeNode, StructTypeNode, U32, U64,
    };

    use super::*;

    #[test]
    fn links_in_maps_sets_and_wrapped_events_emit_struct_payloads_only() {
        let link = DefinedTypeLinkNode::new;
        let payload =
            || StructTypeNode::new(vec![StructFieldTypeNode::new("x", NumberTypeNode::le(U64))]);

        let commit = InstructionNode {
            name: "commit".into(),
            arguments: vec![
                InstructionArgumentNode::new(
                    "map",
                    MapTypeNode::fixed(NumberTypeNode::le(U32), link("inMap"), 1),
                ),
                InstructionArgumentNode::new("set", SetTypeNode::fixed(link("inSet"), 1)),
                InstructionArgumentNode::new("count", link("notStruct")),
            ],
            ..InstructionNode::default()
        };

        let outer = StructTypeNode::new(vec![StructFieldTypeNode::new("inner", link("inWrapper"))]);
        let outer = SizePrefixTypeNode::<TypeNode>::new(outer, NumberTypeNode::le(U32));
        let outer = FixedSizeTypeNode::<TypeNode>::new(outer, 8);
        let outer = HiddenSuffixTypeNode::<TypeNode>::new(outer, vec![]);

        let events = vec![
            EventNode::new("inMap", payload()),
            EventNode::new("inSet", payload()),
            EventNode::new("inWrapper", payload()),
            EventNode::new("outer", outer),
            EventNode::new("notStruct", NumberTypeNode::le(U64)),
        ];

        let idl = RootNode::new(
            ProgramNode::new("p", "11111111111111111111111111111111").add_instruction(commit),
        );

        let names: Vec<_> = linked_event_payloads(&idl, &events)
            .into_iter()
            .map(|defined_type| defined_type.name.to_string())
            .collect();

        assert_eq!(names, ["inMap", "inSet", "inWrapper"]);
    }
}
