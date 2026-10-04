use shipstern_proc_macro::include_shipstern_parser;

include_shipstern_parser!("../idls/map_and_set.json");

#[test]
fn decodes_map_and_set_arguments() {
    use map_and_set::PutArgsScoresEntry as Entry;

    // Borsh HashMap<u8, u16> {1: 770, 4: 1541} and HashSet<u8> {3, 9}: a u32
    // count, then entries or items. The trailing u64 checks nothing shifted.
    let mut wire = vec![1];
    wire.extend(2_u32.to_le_bytes());
    wire.extend([1, 2, 3, 4, 5, 6]);
    wire.extend(2_u32.to_le_bytes());
    wire.extend([3, 9]);
    wire.extend(42_u64.to_le_bytes());

    let path = shipstern_core::instruction::Path::new_single(0);
    let parsed = map_and_set::resolve_instruction_default(&[], &wire, &path).unwrap();
    let map_and_set::instruction::Instruction::Put { args, .. } = parsed.instruction;

    let entry = |key, value| Entry { key, value };

    assert_eq!(args.scores, vec![entry(1, 770), entry(4, 1541)]);
    assert_eq!(args.tags, vec![3, 9]);
    assert_eq!(args.tail, 42);
}
