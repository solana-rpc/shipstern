use std::collections::{HashMap, HashSet};

use codama_nodes::{NestedTypeNodeTrait, NumberFormat, TypeNode};

#[derive(Debug, Clone, Default)]
pub struct SchemaIr {
    pub types: Vec<TypeIr>,
    pub oneofs: Vec<OneofIr>,

    /// Defined types that are represented as a field rather than a message
    /// are inlined as their field label and inner type.
    ///
    /// For example, `optionBool` (a tuple with one bool) becomes a direct
    /// `bool` field instead of a wrapper struct with `item_0`. Keeping the
    /// label is important for aliases such as `Option<InlineStruct>` and
    /// `Array<Tuple>`.
    pub type_aliases: HashMap<String, TypeAliasIr>,

    /// Raw aliases are registered before they are materialized so an alias may
    /// safely refer to another alias declared later in the IDL.
    type_alias_definitions: HashMap<String, TypeAliasDefinitionIr>,
    defined_type_nodes: HashMap<String, TypeNode>,
    reserved_type_names: HashSet<String>,
    materializing_type_aliases: HashSet<String>,
}

#[derive(Debug, Clone)]
struct TypeAliasDefinitionIr {
    type_node: TypeNode,
    base_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypeKindIr {
    DefinedType,
    Account { len: Option<usize> },
    Instruction,
    Event,
    Helper,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeIr {
    pub name: String,
    pub fields: Vec<FieldIr>,
    // should be used later to determine how to render (e.g. accounts vs instruction args vs helper type)
    #[allow(dead_code)]
    pub kind: TypeKindIr,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldIr {
    pub name: String,
    pub tag: u32,
    pub label: LabelIr,
    pub field_type: FieldTypeIr,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeAliasIr {
    pub label: LabelIr,
    pub field_type: FieldTypeIr,
}

/// The on-chain encoding for a Codama option. Rust's native Borsh `Option`
/// only covers the default u8 prefix with no fixed-size None padding.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct OptionEncodingIr {
    pub prefix: OptionPrefixIr,
    /// Fixed Codama options write this many zero bytes after a None prefix.
    pub none_padding: Option<usize>,
}

impl OptionEncodingIr {
    pub fn uses_native_borsh(&self) -> bool {
        self.prefix.is_default() && self.none_padding.is_none()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OptionPrefixIr {
    FixedWidth {
        byte_len: usize,
        one_value: u128,
        big_endian: bool,
    },
    ShortU16,
}

impl Default for OptionPrefixIr {
    fn default() -> Self {
        Self::FixedWidth {
            byte_len: 1,
            one_value: 1,
            big_endian: false,
        }
    }
}

impl OptionPrefixIr {
    pub fn is_default(&self) -> bool {
        matches!(self, Self::FixedWidth {
            byte_len: 1,
            one_value: 1,
            big_endian: false,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LabelIr {
    Singular,
    Optional(OptionEncodingIr),
    Repeated,
    /// Fixed-count array (no length prefix on-chain). The value is the element count.
    /// Rendered as `Vec<T>` / `repeated` in proto for compatibility, but borsh
    /// reads/writes exactly N elements without a length prefix.
    FixedArray(usize),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FieldTypeIr {
    Scalar(ScalarIr),
    Message(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScalarIr {
    Bool,

    /// On-chain u8 (1 byte). Widened to Rust u32 / proto uint32 for prost compatibility.
    U8,
    /// On-chain u16 (2 bytes). Widened to Rust u32 / proto uint32 for prost compatibility.
    U16,
    /// Solana compact u16 (1-3 bytes). Widened to Rust u32 / proto uint32.
    ShortU16,
    /// On-chain u32 (4 bytes). Native match with Rust u32 / proto uint32.
    Uint32,
    /// On-chain u64 (8 bytes). Native match with Rust u64 / proto uint64.
    Uint64,

    /// On-chain i8 (1 byte). Widened to Rust i32 / proto int32 for prost compatibility.
    I8,
    /// On-chain i16 (2 bytes). Widened to Rust i32 / proto int32 for prost compatibility.
    I16,
    /// On-chain i32 (4 bytes). Native match with Rust i32 / proto int32.
    Int32,
    /// On-chain i64 (8 bytes). Native match with Rust i64 / proto int64.
    Int64,

    /// On-chain u128 (16 bytes). Native Rust u128, proto-encoded as `bytes` (16 LE bytes).
    U128,
    /// On-chain i128 (16 bytes). Native Rust i128, proto-encoded as `bytes` (16 LE bytes).
    I128,

    Float,
    Double,
    String,
    Bytes,
    /// Bytes with an explicit Codama length prefix; still Vec<u8> / protobuf bytes.
    SizePrefixedBytes {
        format: codama_nodes::NumberFormat,
        endian: codama_nodes::Endianness,
    },
    /// UTF-8 string with an explicit Codama length prefix; still String / protobuf string.
    SizePrefixedString {
        format: codama_nodes::NumberFormat,
        endian: codama_nodes::Endianness,
    },

    /// Fixed-size byte array with known size (no length prefix on-chain).
    /// Stored as `Vec<u8>` in Rust for prost compatibility, but borsh
    /// reads/writes exactly N bytes. Used for fixed byte arrays, etc.
    FixedBytes(usize),

    /// 32-byte pubkey. Same borsh as FixedBytes(32) but rendered as a
    /// `PublicKey` message wrapper (`message PublicKey { bytes value = 1; }`)
    /// instead of raw bytes.
    PublicKey,
}

// Enum we need to know what kind of oneof this is for rendering purposes
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OneofKindIr {
    /// The top-level instruction dispatcher (e.g. PumpFun { oneof instruction { ... } })
    InstructionDispatch,
    /// The top-level event dispatcher (e.g. ProgramEvents { oneof event { ... } })
    EventDispatch,
    /// A user-defined enum (e.g. OrderType { oneof kind { ... } })
    Enum,
}

#[derive(Debug, Clone)]
pub struct OneofIr {
    pub parent_message: String, // e.g. "PumpFun" or "MyEnum"
    pub field_name: String,     // e.g. "instruction" or "kind"
    pub variants: Vec<OneofVariantIr>,
    pub kind: OneofKindIr,
}

#[derive(Debug, Clone)]
pub struct OneofVariantIr {
    pub tag: u32,
    pub variant_name: String, // e.g. "CreateInstruction" or "VariantA"
    pub message_type: String, // e.g. "CreateInstruction" or "MyEnumVariantA"
}

impl SchemaIr {
    pub fn register_defined_type(&mut self, name: String, type_node: TypeNode) {
        self.defined_type_nodes.insert(name, type_node);
    }

    pub fn reserve_type_names(&mut self, names: impl IntoIterator<Item = String>) {
        self.reserved_type_names.extend(names);
    }

    pub fn register_type_alias(&mut self, name: String, type_node: TypeNode, base_name: String) {
        self.type_alias_definitions
            .insert(name, TypeAliasDefinitionIr {
                type_node,
                base_name,
            });
    }

    pub fn has_type_alias_definition(&self, name: &str) -> bool {
        self.type_alias_definitions.contains_key(name)
    }

    /// Materialize an alias on demand so aliases can reference aliases declared
    /// later in an IDL. Nested wrappers remain explicit rather than being
    /// flattened into an invalid Rust type.
    pub fn materialize_type_alias(&mut self, name: &str) -> (LabelIr, FieldTypeIr) {
        if let Some(alias) = self.type_aliases.get(name) {
            return (alias.label.clone(), alias.field_type.clone());
        }

        if !self.materializing_type_aliases.insert(name.to_string()) {
            panic!("cyclic defined type alias `{name}`");
        }

        let definition = self
            .type_alias_definitions
            .get(name)
            .unwrap_or_else(|| panic!("unknown defined type alias `{name}`"))
            .clone();
        let (label, field_type) = crate::intermediate_representation::helpers::materialize_type(
            &definition.base_name,
            &definition.type_node,
            self,
            &TypeKindIr::Helper,
        );

        self.materializing_type_aliases.remove(name);
        self.type_aliases.insert(name.to_string(), TypeAliasIr {
            label: label.clone(),
            field_type: field_type.clone(),
        });

        (label, field_type)
    }

    /// Returns the exact wire size of a fixed-size Codama type, if known.
    /// Fixed options need this to consume the same number of bytes for `None`
    /// as they do for `Some`.
    pub fn fixed_size_of_type(&self, type_node: &TypeNode) -> Option<usize> {
        self.fixed_size_of_type_inner(type_node, &mut HashSet::new())
    }

    fn fixed_size_of_type_inner(
        &self,
        type_node: &TypeNode,
        visiting_links: &mut HashSet<String>,
    ) -> Option<usize> {
        use codama_nodes::{CountNode, DefaultValueStrategy, TypeNode as T};

        match type_node {
            T::Boolean(_) => Some(1),
            T::PublicKey(_) => Some(32),
            T::Number(number) => number_wire_size(number.format),
            T::FixedSize(fixed) => Some(fixed.size),
            T::Struct(struct_type) => struct_type
                .fields
                .iter()
                .filter(|field| field.default_value_strategy != Some(DefaultValueStrategy::Omitted))
                .try_fold(0usize, |size, field| {
                    size.checked_add(self.fixed_size_of_type_inner(&field.r#type, visiting_links)?)
                }),
            T::Tuple(tuple) => tuple.items.iter().try_fold(0usize, |size, item| {
                size.checked_add(self.fixed_size_of_type_inner(item, visiting_links)?)
            }),
            T::Array(array) => match &*array.count {
                CountNode::Fixed(count) => self
                    .fixed_size_of_type_inner(&array.item, visiting_links)?
                    .checked_mul(crate::utils::as_index(count.value)),
                _ => None,
            },
            T::Option(option) if option.fixed.unwrap_or(false) => {
                option_prefix_wire_size(option.prefix.get_nested_type_node().format)?
                    .checked_add(self.fixed_size_of_type_inner(&option.item, visiting_links)?)
            },
            T::Link(link) => {
                let name = crate::utils::to_pascal_case(&link.name);

                if !visiting_links.insert(name.clone()) {
                    return None;
                }

                let result = self
                    .defined_type_nodes
                    .get(&name)
                    .and_then(|node| self.fixed_size_of_type_inner(node, visiting_links));
                visiting_links.remove(&name);
                result
            },
            _ => None,
        }
    }

    /// Insert a generated helper type, allocating a numeric suffix when its
    /// requested name is already used by a different type.
    pub fn push_unique_type(&mut self, mut msg: TypeIr) -> String {
        let base_name = msg.name.clone();
        let mut suffix = 2;

        loop {
            if self.reserved_type_names.contains(&msg.name) {
                msg.name = format!("{base_name}{suffix}");
                suffix += 1;
                continue;
            }

            if let Some(existing) = self.types.iter().find(|existing| existing.name == msg.name) {
                if existing == &msg {
                    return msg.name;
                }
            } else if !self.type_aliases.contains_key(&msg.name) {
                let name = msg.name.clone();
                self.types.push(msg);
                return name;
            }

            msg.name = format!("{base_name}{suffix}");
            suffix += 1;
        }
    }

    /// Resolve a direct reference to a defined type alias.
    ///
    /// The alias label must be returned together with its field type. Using
    /// only the inner type loses the wire-level Option/Array wrapper and can
    /// make generated code deserialize the wrong bytes.
    pub fn resolve_field(&self, label: LabelIr, field_type: FieldTypeIr) -> (LabelIr, FieldTypeIr) {
        if !matches!(label, LabelIr::Singular) {
            return (label, field_type);
        }

        let mut current_type = field_type;
        let mut seen = std::collections::HashSet::new();

        loop {
            let FieldTypeIr::Message(name) = &current_type else {
                return (LabelIr::Singular, current_type);
            };

            let Some(alias) = self.type_aliases.get(name) else {
                return (LabelIr::Singular, current_type);
            };

            // Malformed/cyclic aliases should not make macro expansion loop.
            if !seen.insert(name.clone()) {
                return (LabelIr::Singular, current_type);
            }

            if !matches!(alias.label, LabelIr::Singular) {
                return (alias.label.clone(), alias.field_type.clone());
            }

            current_type = alias.field_type.clone();
        }
    }

    /// Collect the set of top-level (defined type / helper / account) names
    /// that collide with instruction or event types.
    ///
    /// When an instruction named "swap" generates `SwapArgs` but a defined
    /// type `swapArgs` also maps to `SwapArgs`, the collision set contains
    /// `"SwapArgs"`. The renderer uses this to always emit `super::SwapArgs`
    /// for such names inside submodules.
    ///
    pub fn colliding_names(&self) -> std::collections::HashSet<String> {
        use std::collections::HashSet;

        let top_level: HashSet<&str> = self
            .types
            .iter()
            .filter(|t| !matches!(t.kind, TypeKindIr::Instruction | TypeKindIr::Event))
            .map(|t| t.name.as_str())
            .collect();

        self.types
            .iter()
            .filter(|t| matches!(t.kind, TypeKindIr::Instruction | TypeKindIr::Event))
            .filter(|t| top_level.contains(t.name.as_str()))
            .map(|t| t.name.clone())
            .collect()
    }
}

fn number_wire_size(format: NumberFormat) -> Option<usize> {
    match format {
        NumberFormat::U8 | NumberFormat::I8 => Some(1),
        NumberFormat::U16 | NumberFormat::I16 => Some(2),
        NumberFormat::U32 | NumberFormat::I32 | NumberFormat::F32 => Some(4),
        NumberFormat::U64 | NumberFormat::I64 | NumberFormat::F64 => Some(8),
        NumberFormat::U128 | NumberFormat::I128 => Some(16),
        NumberFormat::ShortU16 => None,
    }
}

fn option_prefix_wire_size(format: NumberFormat) -> Option<usize> {
    match format {
        // Option presence is encoded as 0 or 1, both of which are a one-byte
        // short_u16 value even though arbitrary short_u16 values are variable.
        NumberFormat::ShortU16 => Some(1),
        other => number_wire_size(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schema() -> SchemaIr { SchemaIr::default() }

    fn message(name: &str, field_name: &str) -> TypeIr {
        TypeIr {
            name: name.to_string(),
            fields: vec![FieldIr {
                name: field_name.to_string(),
                tag: 1,
                label: LabelIr::Singular,
                field_type: FieldTypeIr::Scalar(ScalarIr::U8),
            }],
            kind: TypeKindIr::Helper,
        }
    }

    #[test]
    fn identical_type_definitions_are_deduplicated() {
        let mut ir = schema();
        assert_eq!(ir.push_unique_type(message("Shared", "value")), "Shared");
        assert_eq!(ir.push_unique_type(message("Shared", "value")), "Shared");

        assert_eq!(ir.types.len(), 1);
    }

    #[test]
    fn conflicting_type_definitions_get_unique_names() {
        let mut ir = schema();
        assert_eq!(ir.push_unique_type(message("Shared", "value")), "Shared");
        assert_eq!(
            ir.push_unique_type(message("Shared", "other_value")),
            "Shared2"
        );

        assert_eq!(ir.types.len(), 2);
    }

    #[test]
    fn reserved_type_names_force_helpers_to_use_a_suffix() {
        let mut ir = schema();
        ir.reserve_type_names(["FooOption".to_string()]);

        assert_eq!(
            ir.push_unique_type(message("FooOption", "value")),
            "FooOption2"
        );
    }
}
