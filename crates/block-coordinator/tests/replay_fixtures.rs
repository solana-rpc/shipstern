//! Replay tests using captured geyser fixture data.
//!
//! Uses `tests/fixtures/sample.bin` (captured 09-oct-2026, with bank IDs).
//!
//! ```bash
//! cargo test -p shipstern-block-coordinator
//! FIXTURE_PATH=/custom/path.bin cargo test -p shipstern-block-coordinator
//! ```

use std::{collections::HashMap, env, path::PathBuf};

use shipstern_block_coordinator::{
    AccountCommitAt, BlockMachineCoordinator, CoordinatorError, CoordinatorInput,
    CoordinatorMessage, FixtureReader, InstructionRecordSortKey, InstructionSlot,
};
use tokio::sync::mpsc;
use yellowstone_grpc_proto::geyser::{subscribe_update::UpdateOneof, SlotStatus, SubscribeUpdate};

fn fixture_path() -> PathBuf {
    env::var("FIXTURE_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.bin")
        })
}

// =============================================================================
// Coordinator Replay Test
// =============================================================================

#[tokio::test]
async fn replay_fixture_through_coordinator() {
    let updates: Vec<_> = FixtureReader::new(&fixture_path())
        .expect("Failed to open fixture")
        .collect();

    let expected = ExpectedState::from_updates(&updates);
    let confirmed = run_coordinator_with_updates(&updates).await;

    assert!(!confirmed.is_empty(), "Should produce confirmed slots");
    assert_strictly_ascending(&confirmed);

    for slot in &confirmed {
        assert_slot_metadata(slot, &expected);
    }
}

/// A second bank that is never confirmed shares every slot. The output must equal the
/// plain replay, so no record, count or blockhash of the other bank reaches it.
#[tokio::test]
async fn replay_fixture_with_competing_bank_flushes_only_confirmed_bank() {
    let updates: Vec<_> = FixtureReader::new(&fixture_path())
        .expect("Failed to open fixture")
        .collect();

    let plain = run_coordinator_with_updates(&updates).await;
    let competing = run_coordinator_with_updates(&with_competing_bank(&updates)).await;

    let summary = |slots: &[InstructionSlot<u64>]| {
        slots
            .iter()
            .map(|s| {
                (
                    s.slot,
                    s.blockhash,
                    s.executed_transaction_count,
                    s.records.clone(),
                )
            })
            .collect::<Vec<_>>()
    };
    assert!(!plain.is_empty());
    assert_eq!(summary(&competing), summary(&plain));
}

/// Copy each bank-scoped update to a second bank with its own blockhash, sent right
/// after the original, the way two candidate blocks interleave on the stream.
fn with_competing_bank(updates: &[SubscribeUpdate]) -> Vec<SubscribeUpdate> {
    const OTHER_BANK: u64 = 1 << 40;
    let mut out = Vec::with_capacity(updates.len() * 2);

    for update in updates {
        out.push(update.clone());

        let mut copy = update.clone();
        match copy.update_oneof.as_mut() {
            Some(UpdateOneof::Entry(entry)) => entry.bank_id += OTHER_BANK,
            Some(UpdateOneof::BlockMeta(meta)) => {
                meta.bank_id += OTHER_BANK;
                meta.blockhash = solana_hash::Hash::new_unique().to_string();
            },
            Some(UpdateOneof::Slot(slot))
                if slot.status == SlotStatus::SlotCreatedBank as i32
                    || slot.status == SlotStatus::SlotProcessed as i32 =>
            {
                slot.bank_id = slot.bank_id.map(|id| id + OTHER_BANK);
            },
            _ => continue,
        }
        out.push(copy);
    }

    out
}

struct ExpectedState {
    tx_counts: HashMap<u64, u64>,
    parents: HashMap<u64, u64>,
}

impl ExpectedState {
    fn from_updates(updates: &[SubscribeUpdate]) -> Self {
        let mut tx_counts: HashMap<u64, u64> = HashMap::new();
        let mut parents: HashMap<u64, u64> = HashMap::new();

        for update in updates {
            let Some(ref oneof) = update.update_oneof else {
                continue;
            };
            match oneof {
                UpdateOneof::Entry(e) => {
                    *tx_counts.entry(e.slot).or_default() += e.executed_transaction_count;
                },
                UpdateOneof::BlockMeta(m) => {
                    parents.insert(m.slot, m.parent_slot);
                },
                _ => {},
            }
        }

        Self { tx_counts, parents }
    }
}

async fn run_coordinator_with_updates(updates: &[SubscribeUpdate]) -> Vec<InstructionSlot<u64>> {
    let (input_tx, input_rx) = mpsc::channel(4096);
    let (parsed_tx, parsed_rx) = mpsc::channel::<CoordinatorMessage<u64>>(4096);
    let (output_tx, mut output_rx) = mpsc::channel::<InstructionSlot<u64>>(256);

    let coordinator_task = tokio::spawn(BlockMachineCoordinator::run(
        input_rx,
        parsed_rx,
        Some(output_tx),
        None,
        AccountCommitAt::Confirmed,
        true,
    ));

    for update in updates {
        // Send AccountEventSeen for Account events.
        if let Some(UpdateOneof::Account(acct)) = &update.update_oneof
            && let Some(bank_id) = acct.bank_id
        {
            input_tx
                .send(CoordinatorInput::AccountEventSeen {
                    slot: acct.slot,
                    bank_id,
                })
                .await
                .unwrap();
        }

        // Forward BlockSM-relevant events to the coordinator
        let is_block_sm_event = matches!(
            update.update_oneof,
            Some(UpdateOneof::Entry(_) | UpdateOneof::Slot(_) | UpdateOneof::BlockMeta(_))
        );

        if is_block_sm_event {
            input_tx
                .send(CoordinatorInput::GeyserUpdate(Box::new(update.clone())))
                .await
                .unwrap();
        }

        // Simulate all transactions being parsed for this slot, plus one record that
        // names the bank it came from.
        if let Some(UpdateOneof::BlockMeta(meta)) = &update.update_oneof {
            parsed_tx
                .send(CoordinatorMessage::InstructionParsed {
                    slot: meta.slot,
                    bank_id: meta.bank_id,
                    key: InstructionRecordSortKey::new(0, vec![0]),
                    record: meta.bank_id,
                })
                .await
                .unwrap();
            let tx_count = meta.executed_transaction_count;
            for _ in 0..tx_count {
                parsed_tx
                    .send(CoordinatorMessage::TransactionParsed {
                        slot: meta.slot,
                        bank_id: meta.bank_id,
                    })
                    .await
                    .unwrap();
            }
        }
    }

    let mut confirmed = Vec::new();

    finish_coordinator_and_drain_remaining_instructions(
        input_tx,
        parsed_tx,
        coordinator_task,
        &mut output_rx,
        &mut confirmed,
    )
    .await;

    confirmed
}

async fn finish_coordinator_and_drain_remaining_instructions(
    input_tx: mpsc::Sender<CoordinatorInput>,
    parsed_tx: mpsc::Sender<CoordinatorMessage<u64>>,
    coordinator_task: tokio::task::JoinHandle<Result<(), CoordinatorError>>,
    output_rx: &mut mpsc::Receiver<InstructionSlot<u64>>,
    confirmed: &mut Vec<InstructionSlot<u64>>,
) {
    drop(input_tx);
    drop(parsed_tx);

    while let Some(slot) = output_rx.recv().await {
        confirmed.push(slot);
    }

    coordinator_task
        .await
        .expect("coordinator task panicked")
        .expect("coordinator failed");
}

fn assert_strictly_ascending(slots: &[InstructionSlot<u64>]) {
    for pair in slots.windows(2) {
        assert!(
            pair[0].slot < pair[1].slot,
            "Slots must be ascending: {} >= {}",
            pair[0].slot,
            pair[1].slot
        );
    }
}

fn assert_slot_metadata(slot: &InstructionSlot<u64>, expected: &ExpectedState) {
    assert_ne!(
        slot.blockhash,
        solana_hash::Hash::default(),
        "Slot {} has zero blockhash",
        slot.slot
    );

    if let Some(&expected_tx) = expected.tx_counts.get(&slot.slot) {
        assert_eq!(
            slot.executed_transaction_count, expected_tx,
            "Slot {} tx count mismatch",
            slot.slot
        );
    }

    if let Some(&expected_parent) = expected.parents.get(&slot.slot) {
        assert_eq!(
            slot.parent_slot, expected_parent,
            "Slot {} parent mismatch",
            slot.slot
        );
    }
}
