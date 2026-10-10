use shipstern_core::Pubkey;
use shipstern_proc_macro::include_shipstern_parser;

include_shipstern_parser!("../idls/anchor/dex_v1.anchor.json");
include_shipstern_parser!("../idls/anchor/anchor_external.codama.json");

#[test]
fn a_parser_generated_from_an_anchor_idl_decodes_instructions() {
    let path = shipstern_core::instruction::Path::new_single(0);

    let mut data = simple_dex::Instructions::SWAP_DISCRIMINATOR.to_vec();
    data.extend_from_slice(&1_000_u64.to_le_bytes());
    data.extend_from_slice(&900_u64.to_le_bytes());

    let parsed = simple_dex::resolve_instruction_default(&[], &data, &path)
        .expect("swap discriminator should match");

    let simple_dex::instruction::Instruction::Swap { args, .. } = parsed.instruction;

    assert_eq!(args.amount_in, 1_000);
    assert_eq!(args.min_amount_out, 900);
}

/// A fixed slot that carries its own address is not a shift, even when a later
/// missing slot has the same address, so that slot is filled.
#[test]
fn a_repeated_fixed_address_is_filled() {
    let path = shipstern_core::instruction::Path::new_single(0);
    let program = shipstern_test_utils::p("Externa111111111111111111111111111111111111");

    let mut data = external::Instructions::UPDATE_NON_INSTRUCTION_COMPOSITE2_DISCRIMINATOR.to_vec();
    data.extend_from_slice(&7_u32.to_le_bytes());

    let accounts = [program, Pubkey::new([1; 32]), Pubkey::new([2; 32])];
    let parsed = external::resolve_instruction_default(&accounts, &data, &path)
        .expect("a list short by a repeated fixed address parses");

    let external::instruction::Instruction::UpdateNonInstructionComposite2 { accounts, .. } =
        parsed.instruction
    else {
        panic!("expected UpdateNonInstructionComposite2");
    };

    assert_eq!(
        accounts.non_instruction_update_with_different_ident_program,
        program
    );
}
