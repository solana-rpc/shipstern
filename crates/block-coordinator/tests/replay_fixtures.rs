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

    let updates = from_complete_capture_boundary(&updates);
    let expected = ExpectedState::from_updates(&updates);
    let (confirmed, result) = run_coordinator_with_updates(&updates).await;
    result.expect("Complete fixture replay failed");

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

    let updates = from_complete_capture_boundary(&updates);
    let (plain, result) = run_coordinator_with_updates(&updates).await;
    result.expect("Complete fixture replay failed");
    let (competing, result) = run_coordinator_with_updates(&with_competing_bank(&updates)).await;
    result.expect("Competing-bank fixture replay failed");

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

/// The untrimmed capture starts during slot 454787399, with its first 1,032 entries absent.
/// Earlier commitment-only updates also lack block data. A finite replay must report a gap
/// instead of publishing the partial slot's complete descendants.
#[tokio::test]
async fn replay_partial_capture_reports_gap_before_publishing_descendants() {
    let updates: Vec<_> = FixtureReader::new(&fixture_path())
        .expect("Failed to open fixture")
        .collect();
    let partial_slot = updates
        .iter()
        .find_map(|update| match &update.update_oneof {
            Some(UpdateOneof::Entry(entry)) => Some(entry.slot),
            _ => None,
        })
        .expect("Fixture has no entries");
    let (confirmed, result) = run_coordinator_with_updates(&updates).await;

    assert!(
        matches!(result, Err(CoordinatorError::UnresolvedBlock { slot, .. }) if slot <= partial_slot),
        "Partial capture must fail before publishing descendants of slot {partial_slot}: \
         {result:?}"
    );
    assert!(
        confirmed.iter().all(|slot| slot.slot < partial_slot),
        "A missing captured block must not be skipped to publish its descendants"
    );
}

/// Select an explicit ingestion boundary at the first bank whose creation was captured.
/// The binary fixture stays intact: only successful replay tests exclude earlier partial slots,
/// including their later metadata and commitment updates.
fn from_complete_capture_boundary(updates: &[SubscribeUpdate]) -> Vec<SubscribeUpdate> {
    let first_created_slot = updates
        .iter()
        .find_map(|update| match &update.update_oneof {
            Some(UpdateOneof::Slot(status))
                if status.status == SlotStatus::SlotCreatedBank as i32 =>
            {
                Some(status.slot)
            },
            _ => None,
        })
        .expect("Fixture has no captured bank creation");

    updates
        .iter()
        .filter(|update| match &update.update_oneof {
            Some(UpdateOneof::Account(account)) => account.slot >= first_created_slot,
            Some(UpdateOneof::Slot(status)) => status.slot >= first_created_slot,
            Some(UpdateOneof::Entry(entry)) => entry.slot >= first_created_slot,
            Some(UpdateOneof::BlockMeta(meta)) => meta.slot >= first_created_slot,
            _ => true,
        })
        .cloned()
        .collect()
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

async fn run_coordinator_with_updates(
    updates: &[SubscribeUpdate],
) -> (Vec<InstructionSlot<u64>>, Result<(), CoordinatorError>) {
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

    let result = finish_coordinator_and_drain_remaining_instructions(
        input_tx,
        parsed_tx,
        coordinator_task,
        &mut output_rx,
        &mut confirmed,
    )
    .await;

    (confirmed, result)
}

async fn finish_coordinator_and_drain_remaining_instructions(
    input_tx: mpsc::Sender<CoordinatorInput>,
    parsed_tx: mpsc::Sender<CoordinatorMessage<u64>>,
    coordinator_task: tokio::task::JoinHandle<Result<(), CoordinatorError>>,
    output_rx: &mut mpsc::Receiver<InstructionSlot<u64>>,
    confirmed: &mut Vec<InstructionSlot<u64>>,
) -> Result<(), CoordinatorError> {
    drop(input_tx);
    drop(parsed_tx);

    while let Some(slot) = output_rx.recv().await {
        confirmed.push(slot);
    }

    coordinator_task.await.expect("coordinator task panicked")
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
