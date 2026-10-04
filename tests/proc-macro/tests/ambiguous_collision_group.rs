use shipstern_core::Pubkey;
use shipstern_proc_macro::include_shipstern_parser;

include_shipstern_parser!("../idls/ambiguous_collision_group.json");

/// `wideA` and `wideB` share the discriminator and two accounts, so two accounts
/// are ambiguous; `narrow` (one account) must not catch them.
#[test]
fn a_lower_count_instruction_does_not_catch_an_ambiguous_count() {
    let path = shipstern_core::instruction::Path::new_single(0);
    let key = Pubkey::new([1; 32]);

    let err = ambiguous_collision_group::resolve_instruction_default(&[key, key], &[0x01], &path)
        .expect_err("two accounts fit both wideA and wideB");

    assert!(
        err.to_string()
            .starts_with("Ambiguous instruction: variants [wideA, wideB] share"),
        "{err}"
    );

    let narrow = ambiguous_collision_group::resolve_instruction_default(&[key], &[0x01], &path)
        .expect("one account resolves to narrow");

    assert!(matches!(
        narrow.instruction,
        ambiguous_collision_group::instruction::Instruction::Narrow { .. }
    ));
}
