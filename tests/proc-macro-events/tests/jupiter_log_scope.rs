use std::sync::Arc;

use base64::Engine;
use borsh::to_vec;
use shipstern_test_utils::p;
use shipstern_core::{
    bs58,
    instruction::{InstructionShared, InstructionUpdate, Path},
    ParseError, Parser,
};
use shipstern_kafka_sink::{
    KafkaSink, KafkaSinkBuilder, PreparedRecord, RawInstructionEvent, RecordKind,
};
use shipstern_proc_macro::include_shipstern_parser;

include_shipstern_parser!("../idls/jupiter_log_scope_regression.json");

const JUPITER_PROGRAM_ID: &str = "JUP6LkbZbjS1jKKwapdHNy74zcZ3tLUZoi5QNyVTaV4";
const GOOSEFX_PROGRAM_ID: &str = "GAMMA7meSFWaBXF25oSUgmGRwaW6sCMFLmBNiMSdbHVT";
const VALID_SHARED_ACCOUNTS_ROUTE_V2_TX: &str =
    "3fhsnU6MHGEPCgFdfaFLLBg5eqFTtwcqeEqUajmri2K5fKzrX958w3pjbW8h8z9uzK3GyNxRTKQkYrYbf2rwiguM";
const MALFORMED_SHARED_ACCOUNTS_ROUTE_V2_TX: &str =
    "5CqpVdYB7g5MruzfSggVDJXseLkb9YFpGzUpJtYCpjw2GJPEVStZPckski7xVM8driX93WczMG3u7YPMJnXpr6dP";
const ROUTE_DISCRIMINATOR: [u8; 8] = [1, 2, 3, 4, 5, 6, 7, 8];
const UNKNOWN_ROUTE_DISCRIMINATOR: [u8; 8] = [9, 9, 9, 9, 9, 9, 9, 9];
const SWAP_EVENT_DISCRIMINATOR: [u8; 8] = [64, 198, 205, 232, 38, 8, 113, 226];

fn signature_bytes(signature: &str) -> Vec<u8> {
    bs58::decode(signature)
        .into_vec()
        .expect("test signature should be valid base58")
}

fn jupiter_instruction_with_event(
    data: Vec<u8>,
    signature: &str,
    slot: u64,
    tx_index: u64,
    ix_index: u32,
) -> (InstructionUpdate, Vec<u8>) {
    let signature = signature_bytes(signature);
    let logs = vec![
        format!("Program {JUPITER_PROGRAM_ID} invoke [1]"),
        format!("Program data: {}", swap_event_log_payload()),
        format!("Program {JUPITER_PROGRAM_ID} success"),
    ];
    let shared = Arc::new(InstructionShared {
        slot,
        signature: signature.clone(),
        txn_index: tx_index,
        log_messages: logs,
        ..InstructionShared::default()
    });

    (
        InstructionUpdate {
            program: p(JUPITER_PROGRAM_ID),
            accounts: Vec::new(),
            data,
            shared,
            inner: Vec::new(),
            path: Path::new_single(ix_index),
            log_range: 0..3,
        },
        signature,
    )
}

fn sink_with_fallback() -> KafkaSink {
    KafkaSinkBuilder::new()
        .instruction_parser_with_fallback(
            jupiter_regression::InstructionParser,
            "jupiter",
            "ordered.jupiter.instructions",
            "ordered.failed.jupiter.instructions",
        )
        .build()
}

fn header<'a>(record: &'a PreparedRecord, key: &str) -> &'a str {
    record
        .headers
        .iter()
        .find(|header| header.key == key)
        .map(|header| header.value.as_str())
        .expect("expected record header")
}

fn swap_event_log_payload() -> String {
    let args = jupiter_regression::event::SwapEventArgs {
        amm: p("GAMMA7meSFWaBXF25oSUgmGRwaW6sCMFLmBNiMSdbHVT"),
        input_mint: p("So11111111111111111111111111111111111111112"),
        input_amount: 42,
        output_mint: p("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v"),
        output_amount: 1337,
    };
    let mut payload = SWAP_EVENT_DISCRIMINATOR.to_vec();
    payload.extend(to_vec(&args).expect("serialize swap event args"));
    base64::engine::general_purpose::STANDARD.encode(payload)
}

/// Regression for DFlow tx
/// 4VBS4Uy26RrYvzCbDcyG8eh7YXdy6tZxja8AK4EWYCmqbTfqFNykLt1KyZfQ2xyYqdzSMLkGmb9aC3RiKLi9Vdtn
///
/// The bug was that a nested GooseFX `"Program data:"` line inside a Jupiter
/// invoke could be decoded as Jupiter's `swapEvent` because the discriminator
/// bytes collided.
#[test]
fn ignores_nested_goosefx_program_data_for_jupiter_swap_event() {
    let encoded = swap_event_log_payload();
    let logs = vec![
        format!("Program {JUPITER_PROGRAM_ID} invoke [1]"),
        "Program log: Instruction: Route".to_string(),
        format!("Program {GOOSEFX_PROGRAM_ID} invoke [2]"),
        format!("Program data: {encoded}"),
        format!("Program {GOOSEFX_PROGRAM_ID} success"),
        format!("Program {JUPITER_PROGRAM_ID} success"),
    ];

    let events = jupiter_regression::resolve_events_from_logs(&logs);
    assert!(
        events.is_empty(),
        "nested GooseFX program data must not be decoded as Jupiter swap events"
    );
}

#[test]
fn program_log_lines_do_not_close_the_invocation() {
    let encoded = swap_event_log_payload();
    let logs = vec![
        format!("Program {JUPITER_PROGRAM_ID} invoke [1]"),
        "Program log: Swap success".to_string(),
        "Program log: Transfer failed: insufficient funds".to_string(),
        "Program log: Program X invoke [2]".to_string(),
        format!("Program data: {encoded}"),
        format!("Program {JUPITER_PROGRAM_ID} success"),
    ];

    let events = jupiter_regression::resolve_events_from_logs(&logs);
    assert_eq!(events.len(), 1, "log payloads must not pop or push the invocation stack");
}

#[test]
fn parses_top_level_jupiter_program_data_for_swap_event() {
    let encoded = swap_event_log_payload();
    let logs = vec![
        format!("Program {JUPITER_PROGRAM_ID} invoke [1]"),
        format!("Program data: {encoded}"),
        format!("Program {JUPITER_PROGRAM_ID} success"),
    ];

    let events = jupiter_regression::resolve_events_from_logs(&logs);
    assert_eq!(
        events.len(),
        1,
        "top-level Jupiter program data should parse"
    );

    let (accounts, args) = match &events[0].event {
        jupiter_regression::event::Event::SwapEvent { accounts, args } => (accounts, args),
    };

    assert_eq!(
        accounts,
        &jupiter_regression::event::SwapEventAccounts {
            remaining_accounts: vec![],
        }
    );
    assert_eq!(args.amm, p(GOOSEFX_PROGRAM_ID));
    assert_eq!(
        args.input_mint,
        p("So11111111111111111111111111111111111111112")
    );
    assert_eq!(args.input_amount, 42);
    assert_eq!(
        args.output_mint,
        p("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v")
    );
    assert_eq!(args.output_amount, 1337);
}

/// Regression for the Janus Jupiter records around these two txs:
/// `3fhs...rwiguM` decoded as instruction+event, while `5Cqp...pr6dP`
/// previously decoded as event-only instead of routing to the failed topic.
#[tokio::test]
async fn event_enabled_parser_keeps_valid_instruction_on_decoded_topic() {
    let (ix, signature) = jupiter_instruction_with_event(
        ROUTE_DISCRIMINATOR.to_vec(),
        VALID_SHARED_ACCOUNTS_ROUTE_V2_TX,
        428_748_897,
        520,
        4,
    );

    let output = jupiter_regression::InstructionParser
        .parse(&ix)
        .await
        .expect("valid instruction with event should parse");

    assert!(output.instruction.is_some());
    assert_eq!(output.program_events.len(), 1);

    let sink = sink_with_fallback();
    let (record, had_error) = sink
        .parse_instruction(428_748_897, 520, &signature, &ix.path, &ix)
        .await;
    let record = record.expect("decoded record should be emitted");

    assert!(!had_error);
    assert_eq!(record.topic, "ordered.jupiter.instructions");
    assert!(record.is_decoded);
    assert_eq!(record.kind, RecordKind::Instruction);
}

#[tokio::test]
async fn event_enabled_parser_routes_instruction_errors_to_failed_topic() {
    let (ix, signature) = jupiter_instruction_with_event(
        UNKNOWN_ROUTE_DISCRIMINATOR.to_vec(),
        MALFORMED_SHARED_ACCOUNTS_ROUTE_V2_TX,
        428_748_899,
        761,
        7,
    );

    let parse_error = jupiter_regression::InstructionParser
        .parse(&ix)
        .await
        .expect_err("malformed instruction should fail before events can make it look decoded");
    println!("parser error: {parse_error:?}");
    println!("parser error display: {parse_error}");
    assert!(matches!(
        parse_error,
        ParseError::DiscriminatorNotFound(_)
    ));

    let sink = sink_with_fallback();
    let (record, had_error) = sink
        .parse_instruction(428_748_899, 761, &signature, &ix.path, &ix)
        .await;
    let record = record.expect("failed record should be emitted");

    assert!(had_error);
    assert_eq!(record.topic, "ordered.failed.jupiter.instructions");
    assert!(!record.is_decoded);
    assert_eq!(record.kind, RecordKind::Instruction);
    assert!(header(&record, "parse_error").contains("DiscriminatorNotFound"));
    println!("fallback topic: {}", record.topic);
    println!("fallback headers: {:?}", record.headers);
    println!(
        "fallback payload: {}",
        String::from_utf8_lossy(&record.payload)
    );

    let event: RawInstructionEvent =
        serde_json::from_slice(&record.payload).expect("fallback payload should be JSON");
    assert_eq!(event.slot, 428_748_899);
    assert_eq!(event.signature, MALFORMED_SHARED_ACCOUNTS_ROUTE_V2_TX);
    assert_eq!(event.program_id, JUPITER_PROGRAM_ID);
    assert_eq!(
        event.data,
        bs58::encode(UNKNOWN_ROUTE_DISCRIMINATOR).into_string()
    );
}

#[test]
fn check_json_serialization() {
    let args = jupiter_regression::event::SwapEventArgs::default();
    let json_str = serde_json::to_string(&args).expect("failed to json serialize");
    let _: jupiter_regression::event::SwapEventArgs =
        serde_json::from_str(&json_str).expect("failed to json deserialize");
}
