//!
//! Regression: envelope `fe`@0 and event discriminator `a1a2`@4 put the payload
//! at offset 4. The old fixed 8-byte strip landed inside the borsh payload.
//!

use std::sync::Arc;

use shipstern_core::{
    instruction::{InstructionShared, InstructionUpdate, Path},
    Parser, Pubkey,
};
use shipstern_proc_macro::include_shipstern_parser;

include_shipstern_parser!("../idls/padded_cpi_event_envelope.json");

/// Envelope tag, three padding bytes, event discriminator.
const ENVELOPE_AND_DISCRIMINATOR: [u8; 6] = [0xfe, 0x00, 0x00, 0x00, 0xa1, 0xa2];

/// Event discriminator alone, as a `Program data:` log line carries it.
const DISCRIMINATOR_ONLY: [u8; 2] = [0xa1, 0xa2];

const AMOUNT: u64 = 7_654_321;

fn program_id() -> Pubkey { Pubkey::new(padded_envelope::PROGRAM_ID) }

/// Build a top-level instruction carrying one self-CPI event as an inner instruction.
fn update_with_cpi_event(event_data: Vec<u8>) -> InstructionUpdate {
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

    InstructionUpdate {
        program: program_id(),
        accounts: vec![],
        // The outer instruction is `doThing`, whose discriminator the IDL declares.
        data: vec![1, 2, 3, 4, 5, 6, 7, 8],
        shared,
        inner: vec![inner],
        path: Path::new_single(0),
        log_range: 0..0,
    }
}

///
/// The CPI path must strip the IDL's `payload_offset` (4), not a fixed 8.
///
/// Stripping 8 would consume `fe 00 00 00 a1 a2` plus the first two borsh
/// bytes, so no discriminator matches and no event is produced.
///
#[tokio::test]
async fn cpi_path_strips_idl_payload_offset() {
    let mut data = ENVELOPE_AND_DISCRIMINATOR.to_vec();
    data.extend_from_slice(&AMOUNT.to_le_bytes());

    let parser = padded_envelope::InstructionParser;
    let output = parser
        .parse(&update_with_cpi_event(data))
        .await
        .expect("padded CPI event should parse");

    assert_eq!(
        output.program_events.len(),
        1,
        "expected exactly one event from the inner self-CPI instruction",
    );

    let padded_envelope::event::Event::AlphaEvent { args, .. } = &output.program_events[0].event;

    assert_eq!(args.amount, AMOUNT);
}

///
/// The log path sees no envelope, so the same IDL offsets must still match.
///
/// `emit!` writes `[event discriminator][borsh payload]` with neither envelope
/// nor padding, which is why the discriminators are rebased onto the
/// envelope-stripped layout rather than handled per path.
///
#[test]
fn log_path_matches_without_envelope() {
    use base64::Engine;

    let mut data = DISCRIMINATOR_ONLY.to_vec();
    data.extend_from_slice(&AMOUNT.to_le_bytes());

    let encoded = base64::engine::general_purpose::STANDARD.encode(&data);

    let program = program_id().to_string();
    let logs = vec![
        format!("Program {program} invoke [1]"),
        format!("Program data: {encoded}"),
        format!("Program {program} success"),
    ];

    let events = padded_envelope::resolve_events_from_logs(&logs);

    assert_eq!(events.len(), 1, "expected one event from the log line");

    let padded_envelope::event::Event::AlphaEvent { args, .. } = &events[0].event;

    assert_eq!(args.amount, AMOUNT);
}
