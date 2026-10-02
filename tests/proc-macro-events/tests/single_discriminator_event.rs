//!
//! An event with no envelope (`e1e2`@0) behaves as before on both paths: nothing
//! is rebased and the CPI path falls back to Anchor's default envelope.
//!

use std::sync::Arc;

use shipstern_core::{
    instruction::{InstructionShared, InstructionUpdate, Path},
    Parser, Pubkey,
};
use shipstern_proc_macro::include_shipstern_parser;

/// Anchor's `emit_cpi!` tag: `EVENT_IX_TAG.to_le_bytes()`.
const ANCHOR_EVENT_IX_TAG: [u8; 8] = [0xe4, 0x45, 0xa5, 0x2e, 0x51, 0xcb, 0x9a, 0x1d];

/// The event's own discriminator, which sits at offset 0 of the payload.
const EVENT_DISCRIMINATOR: [u8; 2] = [0xe1, 0xe2];

const AMOUNT: u64 = 1_234_567;

include_shipstern_parser!("../idls/single_discriminator_event.json");

fn program_id() -> Pubkey { Pubkey::new(plain_envelope::PROGRAM_ID) }

///
/// With no envelope in the IDL, the CPI path must still strip Anchor's tag.
///
#[tokio::test]
async fn cpi_path_falls_back_to_the_anchor_envelope() {
    let mut event_data = ANCHOR_EVENT_IX_TAG.to_vec();
    event_data.extend_from_slice(&EVENT_DISCRIMINATOR);
    event_data.extend_from_slice(&AMOUNT.to_le_bytes());

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
        accounts: vec![],
        data: vec![1, 2, 3, 4, 5, 6, 7, 8],
        shared,
        inner: vec![inner],
        path: Path::new_single(0),
        log_range: 0..0,
    };

    let parser = plain_envelope::InstructionParser;
    let output = parser.parse(&update).await.expect("should parse");

    assert_eq!(output.program_events.len(), 1);

    let plain_envelope::event::Event::EpsilonEvent { args, .. } = &output.program_events[0].event;

    assert_eq!(args.amount, AMOUNT);
}

///
/// The log path matches the discriminator at offset 0, unchanged.
///
#[test]
fn log_path_matches_at_offset_zero() {
    use base64::Engine;

    let mut data = EVENT_DISCRIMINATOR.to_vec();
    data.extend_from_slice(&AMOUNT.to_le_bytes());

    let encoded = base64::engine::general_purpose::STANDARD.encode(&data);

    let program = program_id().to_string();
    let logs = vec![
        format!("Program {program} invoke [1]"),
        format!("Program data: {encoded}"),
        format!("Program {program} success"),
    ];

    let events = plain_envelope::resolve_events_from_logs(&logs);

    assert_eq!(events.len(), 1);

    let plain_envelope::event::Event::EpsilonEvent { args, .. } = &events[0].event;

    assert_eq!(args.amount, AMOUNT);
}
