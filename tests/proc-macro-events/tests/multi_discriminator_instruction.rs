//!
//! Instructions keep keying on their first discriminator only, even with a
//! `0xfe` event envelope in the same program, so the event-side chain logic
//! can't leak into the instruction path.
//!

use std::sync::Arc;

use shipstern_core::{
    instruction::{InstructionShared, InstructionUpdate, Path},
    Parser, Pubkey,
};
use shipstern_proc_macro::include_shipstern_parser;

include_shipstern_parser!("../idls/multi_discriminator_instruction.json");

/// Byte 3 of the little-endian encoding is `0x00`, so offset 4 of the
/// instruction data is not `0xbb`, the second declared discriminator.
const AMOUNT_IN: u64 = 4_242;

fn program_id() -> Pubkey { Pubkey::new(multi_disc_instruction::PROGRAM_ID) }

fn instruction_update(inner: Vec<InstructionUpdate>) -> InstructionUpdate {
    let mut data = vec![0xaa];
    data.extend_from_slice(&AMOUNT_IN.to_le_bytes());

    debug_assert_ne!(data[4], 0xbb, "fixture must not accidentally satisfy bb@4");

    InstructionUpdate {
        program: program_id(),
        accounts: vec![program_id()],
        data,
        shared: Arc::new(InstructionShared::default()),
        inner,
        path: Path::new_single(0),
        log_range: 0..0,
    }
}

///
/// Only the first discriminator gates the instruction.
///
/// The IDL declares `aa`@0 and `bb`@4, but the payload puts `0x00` at offset 4.
/// The instruction must still parse: were the event-side `&&` chain applied to
/// instructions, this would be filtered instead.
///
#[tokio::test]
async fn instruction_keys_on_first_discriminator_only() {
    let parser = multi_disc_instruction::InstructionParser;
    let output = parser
        .parse(&instruction_update(vec![]))
        .await
        .expect("instruction should parse on its first discriminator");

    let instruction = output.instruction.expect("an instruction was parsed");

    let multi_disc_instruction::instruction::Instruction::DoubleTagged { args, .. } =
        instruction.instruction;

    assert_eq!(args.amount_in, AMOUNT_IN);
}

/// The envelope is live in the same program, so both paths are exercised.
#[tokio::test]
async fn enveloped_event_still_parses_alongside() {
    let mut event_data = vec![0xfe, 0xf1, 0xf2];
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

    let parser = multi_disc_instruction::InstructionParser;
    let output = parser
        .parse(&instruction_update(vec![inner]))
        .await
        .expect("should parse");

    assert_eq!(output.program_events.len(), 1);

    let multi_disc_instruction::event::Event::ZetaEvent { args, .. } =
        &output.program_events[0].event;

    assert_eq!(args.amount, AMOUNT_IN);
}
