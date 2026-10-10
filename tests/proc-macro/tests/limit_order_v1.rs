use shipstern_core::Pubkey;
use shipstern_proc_macro::include_shipstern_parser;
use shipstern_test_utils::p;

include_shipstern_parser!("../idls/limit_order_v1.json");

/// A list short only by its optional account needs no fill, so a fixed-address
/// slot that carries another key (Token-2022 here) still parses.
#[test]
fn short_list_that_needs_no_fill_is_not_checked() {
    let path = shipstern_core::instruction::Path::new_single(0);
    let token_2022 = p("TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb");

    let accounts = [
        Pubkey::new([1; 32]),
        Pubkey::new([2; 32]),
        Pubkey::new([3; 32]),
        Pubkey::new([4; 32]),
        p("11111111111111111111111111111111"),
        token_2022,
    ];

    let parsed = limit_order::resolve_instruction_default(
        &accounts,
        limit_order::Instructions::CANCEL_ORDER_DISCRIMINATOR,
        &path,
    )
    .expect("a list short only by an optional account parses");

    let limit_order::instruction::Instruction::CancelOrder { accounts, .. } = parsed.instruction
    else {
        panic!("expected CancelOrder");
    };

    assert_eq!(accounts.token_program, token_2022);
    assert_eq!(accounts.input_mint, None);
}
