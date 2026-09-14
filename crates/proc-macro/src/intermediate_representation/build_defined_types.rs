use codama_nodes::{DefinedTypeNode, NestedTypeNode, TypeNode};

use crate::intermediate_representation::{
    helpers::{build_fields_ir, materialize_type},
    FieldIr, OneofIr, OneofVariantIr, SchemaIr, TypeIr, TypeKindIr,
};

/// Converts Codama `defined_types` into our internal Intermediate Representation (IR).
///
/// In the IDL, `defined_types` are user-defined structs, enums, or tuples.
pub fn build_defined_types(defined_types: &[DefinedTypeNode], ir: &mut SchemaIr) {
    // Register every raw defined type before materializing anything. Fixed
    // options and aliases can then resolve forward references safely.
    for defined_type in defined_types {
        let name = crate::utils::to_pascal_case(&defined_type.name);

        ir.register_defined_type(name, (*defined_type.r#type).clone());
    }

    // Concrete user-defined types must keep their names even when generated
    // helpers are emitted before the type itself.
    ir.reserve_type_names(defined_types.iter().filter_map(|defined_type| {
        let is_concrete = match &*defined_type.r#type {
            TypeNode::Struct(_) | TypeNode::Enum(_) => true,
            TypeNode::Tuple(tuple) => tuple.items.len() != 1,
            _ => false,
        };

        is_concrete.then(|| crate::utils::to_pascal_case(&defined_type.name))
    }));

    // Register aliases before materializing them. `materialize_type_alias`
    // resolves nested aliases recursively, including forward references.
    let mut alias_names = Vec::new();

    for defined_type in defined_types {
        let name = crate::utils::to_pascal_case(&defined_type.name);

        match &*defined_type.r#type {
            TypeNode::Tuple(tuple_type) if tuple_type.items.len() == 1 => {
                ir.register_type_alias(
                    name.clone(),
                    tuple_type.items[0].clone(),
                    format!("{name}Item0"),
                );
                alias_names.push(name);
            },
            TypeNode::Tuple(_) => {},
            other if !matches!(other, TypeNode::Struct(_) | TypeNode::Enum(_)) => {
                ir.register_type_alias(name.clone(), other.clone(), name.clone());
                alias_names.push(name);
            },
            _ => {},
        }
    }

    for name in alias_names {
        ir.materialize_type_alias(&name);
    }

    // Process concrete types after aliases. They may reference any registered
    // alias regardless of declaration order.
    for defined_type in defined_types {
        let name = crate::utils::to_pascal_case(&defined_type.name);

        match &*defined_type.r#type {
            TypeNode::Struct(struct_type) => build_defined_type_struct(&name, struct_type, ir),
            TypeNode::Enum(enum_type) => build_defined_type_enum(&name, enum_type, ir),
            TypeNode::Tuple(tuple_type) if tuple_type.items.len() != 1 => {
                build_defined_type_tuple(&name, tuple_type, ir)
            },
            _ => {},
        }
    }
}

///
/// IDL example:
///
/// ```text
/// defined type Fees {
///   lp_fee_bps: u64,
///   protocol_fee_bps: u64,
/// }
/// ```
///
/// IR result:
///
/// ```rust, ignore
/// TypeIr {
///   name: "Fees",
///   fields: [
///     { name: "lp_fee_bps",       tag: 1, field_type: Uint64 },
///     { name: "protocol_fee_bps", tag: 2, field_type: Uint64 },
///   ],
///   kind: DefinedType
/// }
/// ```
///
fn build_defined_type_struct(
    name: &str,
    struct_type_node: &codama_nodes::StructTypeNode,
    ir: &mut SchemaIr,
) {
    let fields = build_fields_ir(name, &struct_type_node.fields, ir, TypeKindIr::Helper);

    ir.types.push(TypeIr {
        name: name.to_string(),
        fields,
        kind: TypeKindIr::DefinedType,
    });
}

///
/// IDL example:
///
/// ```text
/// defined type Limits = (u64, string);
/// ```
///
/// IR result:
///
/// ```rust, ignore
/// TypeIr {
///   name: "Limits",
///   fields: [
///     { name: "item_0", tag: 1, field_type: Uint64 },
///     { name: "item_1", tag: 2, field_type: String },
///   ],
///   kind: DefinedType
/// }
/// ```
///
fn build_defined_type_tuple(
    name: &str,
    tuple_type_node: &codama_nodes::TupleTypeNode,
    ir: &mut SchemaIr,
) {
    debug_assert_ne!(tuple_type_node.items.len(), 1);

    let mut fields = Vec::with_capacity(tuple_type_node.items.len());

    for (i, item) in tuple_type_node.items.iter().enumerate() {
        let item_name = format!("{}Item{}", name, i);
        let (label, field_type) = materialize_type(&item_name, item, ir, &TypeKindIr::Helper);

        fields.push(FieldIr {
            name: format!("item_{}", i),
            tag: (i + 1) as u32,
            label,
            field_type,
        });
    }

    ir.types.push(TypeIr {
        name: name.to_string(),
        fields,
        kind: TypeKindIr::DefinedType,
    });
}

///
/// IDL example:
///
/// ```text
/// enum OrderType {
///   Market,
///   Limit(u64),
///   Stop { price: u64 }
/// }
/// ```
///
/// IR result:
///
/// ```rust, ignore
/// // Helper types:
/// TypeIr { name: "OrderTypeMarket", fields: [] }
/// TypeIr { name: "OrderTypeLimit",  fields: [item_0: Uint64] }
/// TypeIr { name: "OrderTypeStop",   fields: [price: Uint64] }
///
/// // Parent enum type:
/// TypeIr { name: "OrderType", fields: [], kind: DefinedType }
///
/// // Oneof:
/// OneofIr {
///   parent_message: "OrderType",
///   field_name: "kind",
///   variants: [
///     { tag: 1, variant_name: "Market", message_type: "OrderTypeMarket" },
///     { tag: 2, variant_name: "Limit",  message_type: "OrderTypeLimit" },
///     { tag: 3, variant_name: "Stop",   message_type: "OrderTypeStop" },
///   ]
/// }
/// ```
///
fn build_defined_type_enum(name: &str, en: &codama_nodes::EnumTypeNode, ir: &mut SchemaIr) {
    let enum_name = name.to_string();
    let mut variants = Vec::with_capacity(en.variants.len());

    for (i, variant) in en.variants.iter().enumerate() {
        let tag = (i + 1) as u32;

        let (variant_name, payload_fields) = build_enum_variant_payload(&enum_name, variant, ir);

        let payload_name = ir.push_unique_type(TypeIr {
            name: format!("{}{}", enum_name, variant_name),
            fields: payload_fields,
            kind: TypeKindIr::Helper,
        });

        variants.push(OneofVariantIr {
            tag,
            variant_name,
            message_type: payload_name,
        });
    }

    // Parent enum type (fields empty; renderer injects the oneof field)
    ir.types.push(TypeIr {
        name: enum_name.clone(),
        fields: vec![],
        kind: TypeKindIr::DefinedType,
    });

    ir.oneofs.push(OneofIr {
        parent_message: enum_name,
        field_name: "kind".to_string(),
        variants,
        kind: crate::intermediate_representation::OneofKindIr::Enum,
    });
}

/// Builds the payload fields for an enum variant (Empty / Tuple / Struct),
/// and returns the variant name (PascalCase) + fields for the payload message.
fn build_enum_variant_payload(
    enum_name: &str,
    variant: &codama_nodes::EnumVariantTypeNode,
    ir: &mut SchemaIr,
) -> (String, Vec<FieldIr>) {
    match variant {
        codama_nodes::EnumVariantTypeNode::Empty(variant) => {
            (crate::utils::to_pascal_case(&variant.name), vec![])
        },

        codama_nodes::EnumVariantTypeNode::Tuple(variant) => {
            let variant_name = crate::utils::to_pascal_case(&variant.name);
            let payload_name = format!("{}{}", enum_name, variant_name);

            let tuple = match &variant.tuple {
                NestedTypeNode::Value(tuple) => tuple,
                _ => panic!("enum tuple variant payload must be NestedTypeNode::Value"),
            };

            (
                variant_name,
                build_tuple_payload_fields(&payload_name, &tuple.items, ir),
            )
        },

        codama_nodes::EnumVariantTypeNode::Struct(variant) => {
            let variant_name = crate::utils::to_pascal_case(&variant.name);

            let struct_type = match &variant.r#struct {
                NestedTypeNode::Value(struct_type) => struct_type,
                _ => panic!("enum struct variant payload must be NestedTypeNode::Value"),
            };

            let payload_name = format!("{}{}", enum_name, variant_name);

            let fields =
                build_fields_ir(&payload_name, &struct_type.fields, ir, TypeKindIr::Helper);

            (variant_name, fields)
        },
    }
}

/// For tuple-variant payloads: generate item_0..item_n fields.
fn build_tuple_payload_fields(
    parent_name: &str,
    items: &[codama_nodes::TypeNode],
    ir: &mut SchemaIr,
) -> Vec<FieldIr> {
    let mut fields = Vec::with_capacity(items.len());

    for (j, item) in items.iter().enumerate() {
        let item_name = format!("item_{}", j);
        let item_base_name = format!(
            "{}{}",
            parent_name,
            crate::utils::to_pascal_case(&item_name)
        );
        let (label, field_type) = materialize_type(&item_base_name, item, ir, &TypeKindIr::Helper);

        fields.push(FieldIr {
            name: item_name,
            tag: (j + 1) as u32,
            label,
            field_type,
        });
    }

    fields
}
