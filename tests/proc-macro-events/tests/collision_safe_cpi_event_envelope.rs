//!
//! The envelope collision guard must stay silent when instructions share a
//! discriminator (`0x09`, told apart by account count) and the envelope is `0xfe`.
//! A guard that fired here would fail compilation.
//!

use std::sync::Arc;

use shipstern_core::{
    instruction::{InstructionShared, InstructionUpdate, Path},
    Parser, Pubkey,
};
use shipstern_proc_macro::include_shipstern_parser;

include_shipstern_parser!("../idls/collision_safe_cpi_event_envelope.json");

const AMOUNT_IN: u64 = 4_242;

fn program_id() -> Pubkey { Pubkey::new(collision_safe::PROGRAM_ID) }

/// `0x09` discriminator followed by the borsh `amountIn`.
fn swap_data() -> Vec<u8> {
    let mut data = vec![0x09];
    data.extend_from_slice(&AMOUNT_IN.to_le_bytes());
    data
}

fn swap_update(account_count: usize) -> InstructionUpdate {
    InstructionUpdate {
        program: program_id(),
        accounts: vec![program_id(); account_count],
        data: swap_data(),
        shared: Arc::new(InstructionShared::default()),
        inner: vec![],
        path: Path::new_single(0),
        log_range: 0..0,
    }
}

/// Three accounts must route to the larger variant, highest count first.
#[tokio::test]
async fn shared_discriminator_resolves_to_the_wider_variant() {
    let parser = collision_safe::InstructionParser;
    let output = parser
        .parse(&swap_update(3))
        .await
        .expect("swapBig should parse");

    let instruction = output.instruction.expect("an instruction was parsed");

    assert!(
        matches!(
            instruction.instruction,
            collision_safe::instruction::Instruction::SwapBig { .. }
        ),
        "expected SwapBig, got {:?}",
        instruction.instruction,
    );
}

/// Two accounts must fall back to the narrower variant.
#[tokio::test]
async fn shared_discriminator_falls_back_to_the_narrower_variant() {
    let parser = collision_safe::InstructionParser;
    let output = parser
        .parse(&swap_update(2))
        .await
        .expect("swapSmall should parse");

    let instruction = output.instruction.expect("an instruction was parsed");

    assert!(
        matches!(
            instruction.instruction,
            collision_safe::instruction::Instruction::SwapSmall { .. }
        ),
        "expected SwapSmall, got {:?}",
        instruction.instruction,
    );
}

/// The envelope is live in this program, so the guard genuinely ran.
#[tokio::test]
async fn envelope_still_parses_events() {
    let mut event_data = vec![0xfe, 0xd1, 0xd2];
    event_data.extend_from_slice(&AMOUNT_IN.to_le_bytes());

    let shared = Arc::new(InstructionShared::default());

    let inner = InstructionUpdate {
        program: program_id(),
        accounts: vec![],
        data: event_data,
        shared: Arc::clone(&shared),
        inner: vec![],
        path: Path::new_single(0),
        log_range: 0..0,
    };

    let update = InstructionUpdate {
        program: program_id(),
        accounts: vec![program_id(); 2],
        data: swap_data(),
        shared,
        inner: vec![inner],
        path: Path::new_single(0),
        log_range: 0..0,
    };

    let parser = collision_safe::InstructionParser;
    let output = parser.parse(&update).await.expect("should parse");

    assert_eq!(output.program_events.len(), 1);

    let collision_safe::event::Event::DeltaEvent { args, .. } = &output.program_events[0].event;

    assert_eq!(args.amount, AMOUNT_IN);
}
