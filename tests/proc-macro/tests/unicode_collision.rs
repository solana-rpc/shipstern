use shipstern_proc_macro::include_shipstern_parser;

include_shipstern_parser!("../idls/unicode_collision.json");

///
/// `ä` and `Ä` both fold to `Ä` in the constant name, so neither gets a
/// constant (an ambiguous name is worse than none). Parsing is unaffected.
///
#[test]
fn colliding_const_names_are_skipped_without_breaking_parsing() {
    let path = shipstern_core::instruction::Path::new_single(0);

    let first =
        unicode_collision::resolve_instruction_default(&[], &[0x11, 0x22, 0x33, 0x44], &path)
            .expect("first instruction should still resolve");

    let second =
        unicode_collision::resolve_instruction_default(&[], &[0x55, 0x66, 0x77, 0x88], &path)
            .expect("second instruction should still resolve");

    assert_ne!(
        std::mem::discriminant(&first.instruction),
        std::mem::discriminant(&second.instruction),
        "the two instructions must remain distinguishable"
    );
}
