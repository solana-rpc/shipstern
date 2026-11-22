//!
//! Regression: a numeric envelope constant is as wide as its declared type.
//!
//! The envelope is the `u32` `0x01020304`, so its wire bytes are `04 03 02 01`. It was
//! narrowed to its first byte, so any inner instruction starting with `04` passed as the
//! envelope.
//!

use std::sync::Arc;

use shipstern_core::{
    instruction::{InstructionShared, InstructionUpdate, Path},
    Parser, Pubkey,
};
use shipstern_proc_macro::include_shipstern_parser;

include_shipstern_parser!("../idls/numeric_cpi_event_envelope.json");

const ENVELOPE: [u8; 4] = 0x0102_0304_u32.to_le_bytes();

const AMOUNT: u64 = 7_654_321;

fn program_id() -> Pubkey { Pubkey::new(numeric_envelope::PROGRAM_ID) }

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

fn cpi_event(envelope: [u8; 4]) -> Vec<u8> {
    let mut data = envelope.to_vec();
    data.push(0xa1);
    data.extend_from_slice(&AMOUNT.to_le_bytes());
    data
}

#[tokio::test]
async fn full_width_envelope_is_accepted() {
    let output = numeric_envelope::InstructionParser
        .parse(&update_with_cpi_event(cpi_event(ENVELOPE)))
        .await
        .expect("enveloped CPI event should parse");

    assert_eq!(output.program_events.len(), 1);

    let numeric_envelope::event::Event::AlphaEvent { args, .. } = &output.program_events[0].event;

    assert_eq!(args.amount, AMOUNT);
}

#[tokio::test]
async fn envelope_matching_only_the_first_byte_is_rejected() {
    let output = numeric_envelope::InstructionParser
        .parse(&update_with_cpi_event(cpi_event([0x04, 0x99, 0x99, 0x99])))
        .await
        .expect("an unrelated inner instruction is not an error");

    assert!(output.program_events.is_empty());
}
