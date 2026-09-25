use shipstern_proc_macro::include_shipstern_parser;

// A raw Anchor IDL, converted in-process: no Codama JSON in between.
include_shipstern_parser!("../../crates/codama-from-anchor/tests/fixtures/dex_v1.anchor.json");

#[test]
fn a_parser_generated_from_an_anchor_idl_decodes_instructions() {
    // The bytes dex_v1.anchor.json declares; without this pin a wrong generated
    // discriminator would still round-trip through the parser.
    assert_eq!(simple_dex::Instructions::SWAP_DISCRIMINATOR, &[
        248, 198, 158, 145, 225, 117, 135, 200
    ],);

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
