//!
//! An event chain with a gap: envelope `fe`@0, then `a1a2`@4 and `b7`@8, so the
//! payload starts after the last discriminator (rebased offset 5), not the first.
//! The gap bytes are `cc cc` so reading from a wrong offset decodes a wrong `u64`.
//!

use std::sync::Arc;

use shipstern_core::{
    instruction::{InstructionShared, InstructionUpdate, Path},
    Parser, Pubkey,
};
use shipstern_proc_macro::include_shipstern_parser;

include_shipstern_parser!("../idls/chained_cpi_event_envelope.json");

/// `fe` envelope, three pad bytes, `a1a2`, two gap bytes, `b7`.
const ENVELOPE_AND_CHAIN: [u8; 9] = [0xfe, 0x00, 0x00, 0x00, 0xa1, 0xa2, 0xcc, 0xcc, 0xb7];

/// Rebased offset the payload must be read from: after `b7`, not after `a1a2`.
const EXPECTED_PAYLOAD_OFFSET: usize = 5;

const AMOUNT: u64 = 0x0102_0304_0506_0708;

fn program_id() -> Pubkey { Pubkey::new(chained_envelope::PROGRAM_ID) }

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
        data: vec![1, 2, 3, 4, 5, 6, 7, 8],
        shared,
        inner: vec![inner],
        path: Path::new_single(0),
        log_range: 0..0,
    }
}

///
/// Every link in the chain must be checked, and the payload read after the last.
///
/// The assertion pins the payload's byte position: `AMOUNT` only decodes
/// correctly when deserialization starts at rebased offset 5. Starting at 2,
/// where a contiguous chain would put it, yields `0xcc_cc_b7_...` instead.
///
#[tokio::test]
async fn chain_reads_payload_after_last_discriminator() {
    let mut data = ENVELOPE_AND_CHAIN.to_vec();
    data.extend_from_slice(&AMOUNT.to_le_bytes());

    // Guard the fixture itself: the payload must sit where the test claims.
    let payload_offset_in_stripped = ENVELOPE_AND_CHAIN.len() - 4;
    assert_eq!(
        payload_offset_in_stripped, EXPECTED_PAYLOAD_OFFSET,
        "fixture layout drifted from the offset under test",
    );

    let parser = chained_envelope::InstructionParser;
    let output = parser
        .parse(&update_with_cpi_event(data))
        .await
        .expect("chained CPI event should parse");

    assert_eq!(output.program_events.len(), 1);

    let chained_envelope::event::Event::GammaEvent { args, .. } = &output.program_events[0].event;

    assert_eq!(
        args.amount, AMOUNT,
        "payload must be read from rebased offset {EXPECTED_PAYLOAD_OFFSET}",
    );
}

///
/// A trailing discriminator that does not match must reject the event.
///
/// Without the `&&` chain only `a1a2` would be checked, so this buffer, which
/// differs by a single byte at the `b7` position, would parse and hand back a
/// wrong payload.
///
#[tokio::test]
async fn chain_rejects_when_trailing_discriminator_differs() {
    let mut chain = ENVELOPE_AND_CHAIN;
    chain[8] = 0xb8;

    let mut data = chain.to_vec();
    data.extend_from_slice(&AMOUNT.to_le_bytes());

    let parser = chained_envelope::InstructionParser;
    let output = parser
        .parse(&update_with_cpi_event(data))
        .await
        .expect("instruction itself still parses");

    assert!(
        output.program_events.is_empty(),
        "a mismatched trailing discriminator must not yield an event",
    );
}

///
/// The same chain, minus the envelope, must match on the log path.
///
#[test]
fn chain_matches_on_log_path() {
    use base64::Engine;

    let mut data = ENVELOPE_AND_CHAIN[4..].to_vec();
    data.extend_from_slice(&AMOUNT.to_le_bytes());

    let encoded = base64::engine::general_purpose::STANDARD.encode(&data);

    let program = program_id().to_string();
    let logs = vec![
        format!("Program {program} invoke [1]"),
        format!("Program data: {encoded}"),
        format!("Program {program} success"),
    ];

    let events = chained_envelope::resolve_events_from_logs(&logs);

    assert_eq!(events.len(), 1);

    let chained_envelope::event::Event::GammaEvent { args, .. } = &events[0].event;

    assert_eq!(args.amount, AMOUNT);
}
