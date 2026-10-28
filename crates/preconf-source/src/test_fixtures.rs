//! Real mainnet transactions for the tests, from slot 458800021, fetched with
//! `getTransaction` as base64 (the bytes the feed sends) and as json (the node's parse).

use std::str::FromStr;

use base64::Engine;
use serde_json::Value;
use solana_message::{
    compiled_instruction::CompiledInstruction, legacy, MessageHeader, VersionedMessage,
};
use solana_pubkey::Pubkey;
use solana_transaction::{versioned::VersionedTransaction, Signature};
use triton_preconfs_proto::preconfs::{
    bam_update, harmonic_update, BamTransaction, BamUpdate, ExecutionResult, HarmonicSlotEnd,
    HarmonicSlotStart, HarmonicTransaction, HarmonicUpdate,
};

use crate::framer::FeedUpdate;

pub(crate) const LEGACY: &str =
    "3n9pFv6QzK86Be1gUuefTPHUoWhKskWChXHi32tbY7padzpih5qVwD7QG6gqVDihWm9ZsA3d1gE7tNjGGjJDNhk5";
pub(crate) const V0: &str =
    "EuQxHB9EmpfUyKjHkpcAYrbnE4dhsJ2yRw6fff3R15n2duBYQSruefFJXr8j4AMbhCpfEUA9DnHgQfAc3etw8wF";
pub(crate) const V1: &str =
    "5ofW99KE7vYaMhJ6uGRjqECvpDKgMxKgRtuC5GKNsN9v66Z2NMQoJdwUByQKVpzdd4QDur2DDqqQ1bSfTSDipEy";

pub(crate) fn fixture(signature: &str) -> (Vec<u8>, Value) {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/");
    let raw = std::fs::read_to_string(format!("{dir}{signature}.base64")).unwrap();
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(raw.trim())
        .unwrap();
    let json = std::fs::read_to_string(format!("{dir}{signature}.json")).unwrap();
    (bytes, serde_json::from_str(&json).unwrap())
}

/// A legacy transaction with one vote instruction, the shape the vote checker accepts.
pub(crate) fn vote_transaction() -> VersionedTransaction {
    let vote_program = Pubkey::from_str("Vote111111111111111111111111111111111111111").unwrap();
    VersionedTransaction {
        signatures: vec![Signature::default()],
        message: VersionedMessage::Legacy(legacy::Message {
            header: MessageHeader {
                num_required_signatures: 1,
                num_readonly_signed_accounts: 0,
                num_readonly_unsigned_accounts: 1,
            },
            account_keys: vec![Pubkey::new_unique(), vote_program],
            recent_blockhash: Default::default(),
            instructions: vec![CompiledInstruction {
                program_id_index: 1,
                accounts: vec![0],
                data: vec![],
            }],
        }),
    }
}

pub(crate) fn transaction(
    slot: u64,
    bytes: Vec<u8>,
    result: Option<ExecutionResult>,
) -> FeedUpdate {
    FeedUpdate::Harmonic(HarmonicUpdate {
        filters: vec!["p".to_string()],
        payload: Some(harmonic_update::Payload::Transaction(HarmonicTransaction {
            transaction: bytes.into(),
            slot,
            result: result.map(|r| r as i32),
            region: "fra".to_string(),
            seq: 0,
        })),
    })
}

pub(crate) fn bam_transaction(
    slot: u64,
    bytes: Vec<u8>,
    result: Option<ExecutionResult>,
) -> FeedUpdate {
    FeedUpdate::Bam(BamUpdate {
        filters: vec!["p".to_string()],
        payload: Some(bam_update::Payload::Transaction(BamTransaction {
            transaction: bytes.into(),
            slot,
            result: result.map(|r| r as i32),
            ..Default::default()
        })),
    })
}

pub(crate) fn start(slot: u64) -> FeedUpdate {
    FeedUpdate::Harmonic(HarmonicUpdate {
        filters: vec![],
        payload: Some(harmonic_update::Payload::SlotStart(HarmonicSlotStart {
            slot,
            region: "fra".to_string(),
        })),
    })
}

pub(crate) fn end(slot: u64) -> FeedUpdate {
    FeedUpdate::Harmonic(HarmonicUpdate {
        filters: vec![],
        payload: Some(harmonic_update::Payload::SlotEnd(HarmonicSlotEnd {
            slot,
            region: "fra".to_string(),
        })),
    })
}
