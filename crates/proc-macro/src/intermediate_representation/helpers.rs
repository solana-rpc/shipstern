use crate::intermediate_representation::{
    FieldIr, FieldTypeIr, LabelIr, OptionEncodingIr, OptionPrefixIr, ScalarIr, SchemaIr, TypeIr,
    TypeKindIr,
};

/// Common interface for field-like nodes from Codama (both struct fields and instruction arguments).
/// Used to deduplicate logic
pub trait FieldLikeNode {
    fn name(&self) -> &codama_nodes::CamelCaseString;
    fn r#type(&self) -> &codama_nodes::TypeNode;
    fn default_value_strategy(&self) -> Option<codama_nodes::DefaultValueStrategy>;
}

impl FieldLikeNode for codama_nodes::StructFieldTypeNode {
    fn name(&self) -> &codama_nodes::CamelCaseString { &self.name }

    fn r#type(&self) -> &codama_nodes::TypeNode { &self.r#type }

    fn default_value_strategy(&self) -> Option<codama_nodes::DefaultValueStrategy> {
        self.default_value_strategy
    }
}

impl FieldLikeNode for codama_nodes::InstructionArgumentNode {
    fn name(&self) -> &codama_nodes::CamelCaseString { &self.name }

    fn r#type(&self) -> &codama_nodes::TypeNode { &self.r#type }

    fn default_value_strategy(&self) -> Option<codama_nodes::DefaultValueStrategy> {
        self.default_value_strategy
    }
}

///
/// Build IR fields from any field-like nodes, handling inline tuple and struct
/// definitions by materializing new messages for them.
///
/// E.g. for a struct like:
///
/// ```rust, ignore
///   struct MyStruct {
///     field1: u64,
///     field2: (u64, String),
///   }
/// ```
///
/// We materialize:
///
/// ```protobuf
/// message MyStructField2Tuple {
///   uint64 item_0 = 1;
///   string item_1 = 2;
/// }
///
/// message MyStruct {
///   uint64 field1 = 1;
///   MyStructField2Tuple field2 = 2;
/// }
/// ```
///
pub fn build_fields_ir(
    parent_name: &str,
    fields: &[impl FieldLikeNode],
    ir: &mut SchemaIr,
    tuple_msg_kind: TypeKindIr,
) -> Vec<FieldIr> {
    let mut out = Vec::new();
    let mut tag: u32 = 0;
    let mut seen_names: std::collections::HashMap<String, u32> = std::collections::HashMap::new();

    for field in fields {
        if field.default_value_strategy() == Some(codama_nodes::DefaultValueStrategy::Omitted) {
            continue;
        }

        tag += 1;

        // Handle duplicate field names by appending a suffix (e.g. `field`, `field_2`, `field_3`, etc.)
        let field_name = {
            let base_name = crate::utils::to_snake_case(field.name());

            let count = seen_names.entry(base_name.clone()).or_insert(0);

            *count += 1;

            if *count > 1 {
                format!("{}_{}", base_name, count)
            } else {
                base_name
            }
        };

        let base_name = format!(
            "{}{}",
            parent_name,
            crate::utils::to_pascal_case(&field_name)
        );
        let (label, field_type) = materialize_type(&base_name, field.r#type(), ir, &tuple_msg_kind);

        out.push(FieldIr {
            name: field_name,
            tag,
            label,
            field_type,
        });
    }

    out
}

/// Materialize a Codama type whenever its protobuf representation needs a
/// named message. This is recursive so inline structs continue to work when
/// they are nested in arrays, tuples, options, or combinations of wrappers.
///
/// Direct inline structs and options containing one keep the names emitted by
/// the original implementation. Additional wrapper messages get suffixes to
/// avoid reusing a name at a different nesting depth.
pub fn materialize_type(
    base_name: &str,
    type_node: &codama_nodes::TypeNode,
    ir: &mut SchemaIr,
    kind: &TypeKindIr,
) -> (LabelIr, FieldTypeIr) {
    use codama_nodes::TypeNode as T;

    match type_node {
        T::Struct(struct_type) => {
            let struct_name = materialize_struct_message(base_name, struct_type, ir, kind.clone());
            (LabelIr::Singular, FieldTypeIr::Message(struct_name))
        },

        T::Tuple(tuple) => {
            let tuple_name = format!("{}Tuple", base_name);
            let tuple_name = materialize_tuple_message(&tuple_name, tuple, ir, kind);
            (LabelIr::Singular, FieldTypeIr::Message(tuple_name))
        },

        T::Option(option) => {
            let option_encoding = option_encoding(option, ir, base_name);
            let inner_base = if matches!(&*option.item, T::Struct(_)) {
                base_name.to_string()
            } else {
                format!("{}Value", base_name)
            };
            let (inner_label, inner_type) = materialize_type(&inner_base, &option.item, ir, kind);

            if matches!(inner_label, LabelIr::Singular) {
                return (LabelIr::Optional(option_encoding), inner_type);
            }

            let wrapper_name = ir.push_unique_type(TypeIr {
                name: format!("{}Option", base_name),
                fields: vec![FieldIr {
                    name: "value".to_string(),
                    tag: 1,
                    label: inner_label,
                    field_type: inner_type,
                }],
                kind: kind.clone(),
            });

            (
                LabelIr::Optional(option_encoding),
                FieldTypeIr::Message(wrapper_name),
            )
        },

        T::Array(array) => {
            let outer_label = match &*array.count {
                codama_nodes::CountNode::Fixed(fixed) => {
                    LabelIr::FixedArray(crate::utils::as_index(fixed.value))
                },
                _ => LabelIr::Repeated,
            };

            // Array<struct> can use the field helper directly. More complex
            // items get a distinct name so nested arrays do not collide with
            // their enclosing wrapper.
            let inner_base = if matches!(&*array.item, T::Struct(_)) {
                base_name.to_string()
            } else {
                format!("{}Item", base_name)
            };
            let (inner_label, inner_type) = materialize_type(&inner_base, &array.item, ir, kind);

            if matches!(inner_label, LabelIr::Singular) {
                return (outer_label, inner_type);
            }

            let wrapper_name = ir.push_unique_type(TypeIr {
                name: format!("{}Inner", base_name),
                fields: vec![FieldIr {
                    name: "items".to_string(),
                    tag: 1,
                    label: inner_label,
                    field_type: inner_type,
                }],
                kind: kind.clone(),
            });

            (outer_label, FieldTypeIr::Message(wrapper_name))
        },

        T::Link(link) => {
            let name = crate::utils::to_pascal_case(&link.name);

            if ir.has_type_alias_definition(&name) {
                ir.materialize_type_alias(&name)
            } else {
                ir.resolve_field(LabelIr::Singular, FieldTypeIr::Message(name))
            }
        },

        other => ir.resolve_field(LabelIr::Singular, map_type(other)),
    }
}

fn option_encoding(
    option: &codama_nodes::OptionTypeNode,
    ir: &SchemaIr,
    base_name: &str,
) -> OptionEncodingIr {
    use codama_nodes::{Endianness, NestedTypeNodeTrait, NumberFormat};

    let prefix_type = option.prefix.get_nested_type_node();
    let prefix = match prefix_type.format {
        NumberFormat::ShortU16 => OptionPrefixIr::ShortU16,
        NumberFormat::U8 | NumberFormat::I8 => OptionPrefixIr::FixedWidth {
            byte_len: 1,
            one_value: 1,
            big_endian: false,
        },
        NumberFormat::U16 | NumberFormat::I16 => OptionPrefixIr::FixedWidth {
            byte_len: 2,
            one_value: 1,
            big_endian: prefix_type.endian == Endianness::Be,
        },
        NumberFormat::U32 | NumberFormat::I32 => OptionPrefixIr::FixedWidth {
            byte_len: 4,
            one_value: 1,
            big_endian: prefix_type.endian == Endianness::Be,
        },
        NumberFormat::U64 | NumberFormat::I64 => OptionPrefixIr::FixedWidth {
            byte_len: 8,
            one_value: 1,
            big_endian: prefix_type.endian == Endianness::Be,
        },
        NumberFormat::U128 | NumberFormat::I128 => OptionPrefixIr::FixedWidth {
            byte_len: 16,
            one_value: 1,
            big_endian: prefix_type.endian == Endianness::Be,
        },
        NumberFormat::F32 => OptionPrefixIr::FixedWidth {
            byte_len: 4,
            one_value: 1.0f32.to_bits() as u128,
            big_endian: prefix_type.endian == Endianness::Be,
        },
        NumberFormat::F64 => OptionPrefixIr::FixedWidth {
            byte_len: 8,
            one_value: 1.0f64.to_bits() as u128,
            big_endian: prefix_type.endian == Endianness::Be,
        },
    };

    let none_padding = option.fixed.unwrap_or(false).then(|| {
        ir.fixed_size_of_type(&option.item)
            .unwrap_or_else(|| panic!("fixed option `{base_name}` has a variable-size item"))
    });

    OptionEncodingIr {
        prefix,
        none_padding,
    }
}

/// Materialize an inline Codama struct as a named helper message.
fn materialize_struct_message(
    struct_msg_name: &str,
    struct_type: &codama_nodes::StructTypeNode,
    ir: &mut SchemaIr,
    type_kind: TypeKindIr,
) -> String {
    let fields = build_fields_ir(struct_msg_name, &struct_type.fields, ir, type_kind.clone());

    ir.push_unique_type(TypeIr {
        name: struct_msg_name.to_string(),
        fields,
        kind: type_kind,
    })
}

///
/// Recursively materialize a tuple type as a new IR message, registering it in the schema.
///
/// Each tuple item becomes a field named `item_0`, `item_1`, etc.
/// Nested tuples are recursively materialized as their own messages.
///
/// E.g. for a nested tuple `(u64, (bool, String))` with name "MyTuple":
///
/// ```protobuf
/// message MyTupleItem1Tuple {
///   bool item_0 = 1;
///   string item_1 = 2;
/// }
///
/// message MyTuple {
///   uint64 item_0 = 1;
///   MyTupleItem1Tuple item_1 = 2;
/// }
/// ```
///
fn materialize_tuple_message(
    tuple_msg_name: &str,
    tuple: &codama_nodes::TupleTypeNode,
    ir: &mut SchemaIr,
    kind: &TypeKindIr,
) -> String {
    let mut fields = Vec::new();

    for (i, item) in tuple.items.iter().enumerate() {
        let item_name = format!("item_{}", i);
        let tag = (i + 1) as u32;

        let item_base_name = format!(
            "{}{}",
            tuple_msg_name,
            crate::utils::to_pascal_case(&item_name)
        );
        let (label, field_type) = materialize_type(&item_base_name, item, ir, kind);

        fields.push(FieldIr {
            name: item_name,
            tag,
            label,
            field_type,
        });
    }

    ir.push_unique_type(TypeIr {
        name: tuple_msg_name.to_string(),
        fields,
        kind: kind.clone(),
    })
}

/// Map a single Codama type node to its IR scalar/message type.
///
/// Wrappers like Option/Array and Tuple are handled by `materialize_type`.
fn map_type(t: &codama_nodes::TypeNode) -> FieldTypeIr {
    use codama_nodes::{NumberFormat as NF, TypeNode as T};

    match t {
        T::String(_) => FieldTypeIr::Scalar(ScalarIr::String),
        T::SizePrefix(sp) => match sp.r#type.as_ref() {
            T::Bytes(_) => FieldTypeIr::Scalar(ScalarIr::Bytes),
            _ => FieldTypeIr::Scalar(ScalarIr::String),
        },
        T::Bytes(_) => FieldTypeIr::Scalar(ScalarIr::Bytes),
        T::PublicKey(_) => FieldTypeIr::Scalar(ScalarIr::PublicKey),
        T::Boolean(_) => FieldTypeIr::Scalar(ScalarIr::Bool),

        T::Number(n) => match n.format {
            NF::U8 => FieldTypeIr::Scalar(ScalarIr::U8),
            NF::U16 => FieldTypeIr::Scalar(ScalarIr::U16),
            NF::ShortU16 => FieldTypeIr::Scalar(ScalarIr::ShortU16),
            NF::U32 => FieldTypeIr::Scalar(ScalarIr::Uint32),
            NF::U64 => FieldTypeIr::Scalar(ScalarIr::Uint64),
            NF::I8 => FieldTypeIr::Scalar(ScalarIr::I8),
            NF::I16 => FieldTypeIr::Scalar(ScalarIr::I16),
            NF::I32 => FieldTypeIr::Scalar(ScalarIr::Int32),
            NF::I64 => FieldTypeIr::Scalar(ScalarIr::Int64),
            NF::F32 => FieldTypeIr::Scalar(ScalarIr::Float),
            NF::F64 => FieldTypeIr::Scalar(ScalarIr::Double),
            NF::U128 => FieldTypeIr::Scalar(ScalarIr::U128),
            NF::I128 => FieldTypeIr::Scalar(ScalarIr::I128),
        },

        T::Link(link) => FieldTypeIr::Message(crate::utils::to_pascal_case(&link.name)),

        // FixedSize bytes/string => fixed-size bytes (no length prefix on-chain)
        T::FixedSize(node) => match *node.r#type {
            T::Bytes(_) | T::String(_) => FieldTypeIr::Scalar(ScalarIr::FixedBytes(node.size)),

            _ => panic!("map_type not implemented for FixedSize {:?}", node.r#type),
        },

        // Tuple must be handled at call-site via materialize_tuple_message
        T::Tuple(_) => {
            panic!("map_type() called with Tuple; handle tuple via materialize_tuple_message")
        },

        T::Option(o) => map_type(&o.item),
        T::Array(a) => map_type(&a.item),

        // Maps have no direct proto equivalent with complex key/value types;
        // serialize the entire map as a raw byte blob.
        T::Map(_) => FieldTypeIr::Scalar(ScalarIr::Bytes),

        other => panic!("map_type not implemented for {:?}", other),
    }
}

/// Unwrap Codama's NestedTypeNode wrappers to get to the underlying StructTypeNode for accounts.
///
/// We expect the account data to always be a struct, but Codama wraps it in various ways (e.g. for fixed-size arrays) so we need to unwrap those.
pub fn unwrap_nested_struct(
    node: &codama_nodes::NestedTypeNode<codama_nodes::StructTypeNode>,
) -> &codama_nodes::StructTypeNode {
    use codama_nodes::NestedTypeNode as N;

    match node {
        N::Value(v) => v,

        N::FixedSize(w) => unwrap_nested_struct(&w.r#type),
        N::HiddenPrefix(w) => unwrap_nested_struct(&w.r#type),
        N::HiddenSuffix(w) => unwrap_nested_struct(&w.r#type),
        N::SizePrefix(w) => unwrap_nested_struct(&w.r#type),

        // If Codama adds more wrappers later, we want to fail loudly.
        other => panic!("Unsupported AccountNode.data wrapper: {:?}", other),
    }
}

/// Unwrap a Codama TypeNode to the underlying StructTypeNode for events.
///
/// Event data is a TypeNode that may be wrapped in a HiddenPrefix (for the discriminator bytes).
pub fn unwrap_event_struct(node: &codama_nodes::TypeNode) -> &codama_nodes::StructTypeNode {
    use codama_nodes::TypeNode as T;

    match node {
        T::Struct(s) => s,
        T::HiddenPrefix(w) => unwrap_event_struct(&w.r#type),
        T::HiddenSuffix(w) => unwrap_event_struct(&w.r#type),
        T::FixedSize(w) => unwrap_event_struct(&w.r#type),
        T::SizePrefix(w) => unwrap_event_struct(&w.r#type),
        other => panic!("Unsupported EventNode.data wrapper: {:?}", other),
    }
}
