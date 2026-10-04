use prost::Message;
use prost_reflect::{DescriptorPool, DynamicMessage, Value};
use shipstern_proc_macro::include_shipstern_parser;
use shipstern_test_utils::check_protobuf_format;

include_shipstern_parser!("../idls/instruction_name_collisions.json");

fn message_field(message: &DynamicMessage, name: &str) -> DynamicMessage {
    message
        .get_field_by_name(name)
        .and_then(|value| value.as_message().cloned())
        .unwrap_or_else(|| panic!("no message field `{name}`"))
}

/// Instruction `swap` emits `Swap` and `SwapAccounts`, the names of two defined types.
/// Both stay in the schema, and `Instructions` names the instruction ones.
#[test]
fn instruction_messages_named_like_defined_types_are_kept() {
    let schema = instruction_name_collisions::PROTOBUF_SCHEMA;

    check_protobuf_format(schema);

    assert!(schema.contains("message Swap {\n  oneof kind {"));
    assert!(schema.contains("message SwapAccounts {\n  uint32 count = 1;"));
    assert!(schema.contains("    IxSwap swap = 1;"));

    let path = shipstern_core::instruction::Path::new_single(0);
    let mut data = 5u64.to_le_bytes().to_vec();
    data.push(1);

    let parsed = instruction_name_collisions::resolve_instruction_default(
        &[shipstern_core::Pubkey::new([7; 32])],
        &data,
        &path,
    )
    .expect("swap should parse");
    let encoded = parsed.encode_to_vec();

    let descriptor = protox_parse::parse("schema.proto", schema).expect("schema parse failed");
    let mut pool = DescriptorPool::new();

    pool.add_file_descriptor_proto(descriptor)
        .expect("descriptor add failed");

    let instructions_descriptor = pool
        .get_message_by_name("instruction_name_collisions.Instructions")
        .expect("Instructions message missing from schema");

    let decoded = DynamicMessage::decode(instructions_descriptor, encoded.as_slice())
        .expect("schema-driven decode failed for generated instruction bytes");
    let args = message_field(&message_field(&decoded, "swap"), "args");

    assert_eq!(
        args.get_field_by_name("amount").as_deref(),
        Some(&Value::U64(5))
    );
    assert!(message_field(&args, "mode").has_field_by_name("exact_out"));
}
