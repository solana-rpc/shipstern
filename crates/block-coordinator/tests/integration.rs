//! Integration tests for BlockMachineCoordinator.
//!
//! ## Complete State Machine
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────────────────────┐
//! │                        COORDINATOR STATE MACHINE                            │
//! ├─────────────────────────────────────────────────────────────────────────────┤
//! │                                                                             │
//! │  STATE:                                                                     │
//! │    buffer: BTreeMap<(Slot, BankId), SlotBuffer> // pending banks            │
//! │    discarded_slots: BTreeMap<Slot, Reason> // dead/forked/incomplete        │
//! │    discarded_banks: BTreeMap<(Slot, BankId), Reason>                        │
//! │    confirmed_banks: BTreeMap<Slot, BankId> // one winner per slot           │
//! │    last_flushed_slot: Option<Slot>        // for gap detection              │
//! │                                                                             │
//! │  SLOT BUFFER:                                                               │
//! │    expected_tx_count: Option<u64>         // from FrozenBlock               │
//! │    parsed_tx_count: u64                   // from TransactionParsed         │
//! │    instruction_commitment_reached: bool   // from Confirmed event           │
//! │    expected_account_count: Option<u64>    // from AccountEventSeen          │
//! │    account_processed_count: u64           // from AccountRecordParsed /     │
//! │                                           //   AccountFiltered / Error      │
//! │    instruction_records: BTreeMap<InstructionRecordSortKey, R>               │
//! │    account_records: BTreeMap<AccountRecordSortKey, R>               │
//! │    parent_slot, blockhash                                                   │
//! │                                                                             │
//! └─────────────────────────────────────────────────────────────────────────────┘
//!
//! ┌─────────────────────────────────────────────────────────────────────────────┐
//! │                           INPUT PROCESSING                                  │
//! ├─────────────────────────────────────────────────────────────────────────────┤
//! │                                                                             │
//! │  ┌─────────────────┐                                                        │
//! │  │ Parsed Message  │                                                        │
//! │  └────────┬────────┘                                                        │
//! │           │                                                                 │
//! │           ▼                                                                 │
//! │  ┌────────────────────┐  YES   ┌─────────────────────────────────┐          │
//! │  │ bank discarded?    ├───────►│ DROP + WARN (non-canonical)     │          │
//! │  └────────┬───────────┘        │ [discarded_slot_ignores_parsed] │          │
//! │           │ NO                 └─────────────────────────────────┘          │
//! │           ▼                                                                 │
//! │  ┌──────────────────────────────┐YES┌──────────────────────────────────────┐│
//! │  │ slot <= last_instruction_    ├──►│ DROP + WARN                          ││
//! │  │ flushed_slot?                │   │ Instruction event after flush frontier││
//! │  └────────┬─────────────────────┘   └──────────────────────────────────────┘│
//! │           │ NO                 └─────────────────────────────────┘          │
//! │           ▼                                                                 │
//! │  ┌────────────────────┐                                                     │
//! │  │ Buffer the message │      [parsed_messages_before_lifecycle_are_buffered]│
//! │  │ (creates slot if   │        [two_gate_flush_end_to_end]                  │
//! │  │  not exists)       │                                                     │
//! │  └────────────────────┘                                                     │
//! │                                                                             │
//! └─────────────────────────────────────────────────────────────────────────────┘
//!
//! ┌─────────────────────────────────────────────────────────────────────────────┐
//! │                           FLUSH DECISION TREE                               │
//! ├─────────────────────────────────────────────────────────────────────────────┤
//! │                                                                             │
//! │  drain_instruction_flushable() then drain_account_flushable()               │
//! │  called once per event batch in run()                                       │
//! │                                                                             │
//! │  FOR each slot in buffer (ascending order):                                 │
//! │                                                                             │
//! │  ┌──────────────────────────────────────────────┐                           │
//! │  │ INSTRUCTION CHECK: tx_parse_complete?        │                           │
//! │  │ (parsed_tx_count >= expected_tx_count)       │                           │
//! │  └──────────────────────┬───────────────────────┘                           │
//! │                         │                                                   │
//! │              NO ◄───────┴───────► YES                                       │
//! │              │                    │                                         │
//! │              ▼                    ▼                                         │
//! │  ┌─────────────────┐    ┌──────────────────────────────────────────────┐    │
//! │  │ STOP            │    │ INSTRUCTION CHECK:                           │    │
//! │  │ [incomplete     │    │ instruction_commitment_reached?              │    │
//! │  │  _blocks_       │    └──────────────────────┬───────────────────────┘    │
//! │  │  subsequent]    │                           │                           │
//! │  └─────────────────┘                NO ◄───────┴───────► YES               │
//! │                                     │                    │                  │
//! │                                     ▼                    ▼                  │
//! │                     ┌─────────────────┐    ┌───────────────────────────┐    │
//! │                     │ STOP            │    │ ACCOUNT CHECK:            │    │
//! │                     │ [sequential     │    │ account_parse_complete?   │    │
//! │                     │  _flush_order]  │    │ (account_processed_count  │    │
//! │                     └─────────────────┘    │  >= expected_account_cnt) │    │
//! │                                            └─────────────┬─────────────┘    │
//! │                                                          │                  │
//! │                                               NO ◄───────┴───────► YES      │
//! │                                               │                    │        │
//! │                                               ▼                    ▼        │
//! │                                  ┌─────────────────┐  ┌────────────────┐    │
//! │                                  │ STOP            │  │ INSTRUCTION    │    │
//! │                                  │ [account_gate_  │  │ GAP CHECK:     │    │
//! │                                  │  flush_without  │  │ parent already │    │
//! │                                  │  _account_cnt]  │  │ published?     │    │
//! │                                  └─────────────────┘  └───────┬────────┘    │
//! │                                                               │             │
//! │                                                    NO ◄───────┴───────► YES │
//! │                                                    │                    │   │
//! │                                                    ▼                    ▼   │
//! │                                             ┌─────────────┐    ┌─────────┐  │
//! │                                             │ STOP        │    │ FLUSH   │  │
//! │                                             │ [gap_in_    │    │ SLOT    │  │
//! │                                             │  sequence_  │    │ Update  │  │
//! │                                             │  blocks]    │    │ last_   │  │
//! │                                             └─────────────┘    │ flushed │  │
//! │                                                                └─────────┘  │
//! │                                                                             │
//! └─────────────────────────────────────────────────────────────────────────────┘
//!
//! NOTE:
//!   The implementation uses two independent drains, each preserving its parent
//!   frontier. A required parent cannot be discarded just to publish its child.
//!   Parsing and commitment gates remain independent for each output.
//!   When `require_tx_gate=false`, `expected_tx_count` is forced to 0, so
//!   `tx_parse_complete` is immediately true once BlockFrozen is seen.
//!
//! ┌─────────────────────────────────────────────────────────────────────────────┐
//! │                           DISCARD SCENARIOS                                 │
//! ├─────────────────────────────────────────────────────────────────────────────┤
//! │                                                                             │
//! │  1. DEAD SLOT                          [dead_slot_discarded]                │
//! │     SlotLifecycle::Dead ──► DeadSlotDetected ──► discard_slot()             │
//! │                                                                             │
//! │  2. FORKED SLOT                        [sibling_fork_via_finalized]         │
//! │     Sibling finalized ──► ForksDetected ──► skip the slot until it confirms │
//! │                                                                             │
//! │  3. UNRESOLVED CANONICAL BLOCK                                              │
//! │     Required data still missing after grace ──► error; no child flush       │
//! │                                                                             │
//! │  discard_slot() actions:                                                    │
//! │    • Add to discarded_slots with the reason                                 │
//! │    • Drop banks: Dead frozen+reported, Forked none, Incomplete all          │
//! │    • both drain_* functions called in run() after event                     │
//! │                                                                             │
//! └─────────────────────────────────────────────────────────────────────────────┘
//! ```
//!
//! The test list with what each test checks is in `tests/README.md`.
//!
//! ## Fork Detection Details
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────────────────────┐
//! │                         Fork Detection Triggers                             │
//! ├─────────────────────────────────────────────────────────────────────────────┤
//! │                                                                             │
//! │  1. Dead Slot                        2. Sibling Finalization                │
//! │  ───────────                         ──────────────────────                 │
//! │                                                                             │
//! │      Parent                              Parent (rooted)                    │
//! │         │                                    │                              │
//! │    ┌────┴────┐                          ┌────┴────┐                         │
//! │    ▼         ▼                          ▼         ▼                         │
//! │  Slot A   Slot B                    Slot A    Slot B                        │
//! │  (dead)   (ok)                   (finalized)  (forked!)                     │
//! │    │                                             │                          │
//! │    ▼                                             ▼                          │
//! │  DeadSlotDetected(A)                       ForksDetected(B)                 │
//! │                                                                             │
//! │  Note: In practice, confirmed slots won't be forked because confirmation    │
//! │  requires supermajority (2/3+) votes — siblings can't both get enough.      │
//! │                                                                             │
//! └─────────────────────────────────────────────────────────────────────────────┘
//! ```

use std::time::Duration;

use shipstern_block_coordinator::{
    AccountCommitAt, AccountRecordSortKey, AccountSlot, BlockMachineCoordinator, CoordinatorError,
    CoordinatorInput, CoordinatorMessage, InstructionRecordSortKey, InstructionSlot,
    MISSING_PARENT_GRACE_SLOTS,
};
use solana_hash::Hash;
use tokio::sync::mpsc;
use yellowstone_grpc_proto::{
    geyser::{
        subscribe_update::UpdateOneof, SlotStatus, SubscribeUpdate, SubscribeUpdateBlockMeta,
        SubscribeUpdateEntry, SubscribeUpdateEntryUpdateParent, SubscribeUpdateSlot,
    },
    prelude::{BlockHeight, UnixTimestamp},
};

// =============================================================================
// Test Harness
// =============================================================================

struct TestHarness {
    input_tx: mpsc::Sender<CoordinatorInput>,
    parsed_tx: mpsc::Sender<CoordinatorMessage<String>>,
    output_rx: mpsc::Receiver<InstructionSlot<String>>,
    account_output_rx: mpsc::Receiver<AccountSlot<String>>,
    coordinator: tokio::task::JoinHandle<Result<(), CoordinatorError>>,
}

impl TestHarness {
    fn spawn() -> Self { Self::spawn_with_options(true, AccountCommitAt::Confirmed) }

    fn spawn_with_require_tx_gate(require_tx_gate: bool) -> Self {
        Self::spawn_with_options(require_tx_gate, AccountCommitAt::Confirmed)
    }

    fn spawn_with_options(require_tx_gate: bool, account_commit_at: AccountCommitAt) -> Self {
        let (input_tx, input_rx) = mpsc::channel(256);
        let (parsed_tx, parsed_rx) = mpsc::channel(256);
        let (output_tx, output_rx) = mpsc::channel(64);
        let (account_output_tx, account_output_rx) = mpsc::channel(64);

        let coordinator = tokio::spawn(BlockMachineCoordinator::run(
            input_rx,
            parsed_rx,
            Some(output_tx),
            Some(account_output_tx),
            account_commit_at,
            require_tx_gate,
        ));

        Self {
            input_tx,
            parsed_tx,
            output_rx,
            account_output_rx,
            coordinator,
        }
    }

    fn slot(&self, slot: u64) -> SlotBuilder {
        SlotBuilder::new(self.input_tx.clone(), self.parsed_tx.clone(), slot)
    }

    async fn expect_flush(&mut self, slot: u64) -> FlushAssertion {
        let ix_slot = tokio::time::timeout(Duration::from_secs(2), self.output_rx.recv())
            .await
            .expect("Timed out waiting for flush")
            .expect("Channel closed");

        assert_eq!(ix_slot.slot, slot, "Expected slot {slot} to flush");
        FlushAssertion(ix_slot)
    }

    async fn expect_account_flush(&mut self, slot: u64) -> AccountFlushAssertion {
        let acct_slot = tokio::time::timeout(Duration::from_secs(2), self.account_output_rx.recv())
            .await
            .expect("Timed out waiting for account flush")
            .expect("Account channel closed");

        assert_eq!(
            acct_slot.slot, slot,
            "Expected account slot {slot} to flush"
        );
        AccountFlushAssertion(acct_slot)
    }

    async fn expect_no_flush(&mut self) {
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert!(self.output_rx.try_recv().is_err(), "Unexpected flush");
    }

    async fn expect_unresolved(&mut self, slot: u64, observed_slot: u64) {
        let error = tokio::time::timeout(Duration::from_secs(2), &mut self.coordinator)
            .await
            .expect("Coordinator must stop before skipping the unresolved block")
            .expect("Coordinator must not panic")
            .expect_err("Missing canonical data must require recovery");
        assert!(
            matches!(
                error,
                CoordinatorError::UnresolvedBlock {
                    slot: missing,
                    observed_slot: observed,
                } if missing == slot && observed == observed_slot
            ),
            "Expected missing block {slot} at observed slot {observed_slot}, got {error}"
        );
        let diagnostic = error.to_string();
        assert!(diagnostic.contains(&slot.to_string()));
        assert!(diagnostic.contains(&observed_slot.to_string()));
        assert!(
            diagnostic.contains("replay") || diagnostic.contains("backfill"),
            "The error must explain how to recover the missing block"
        );
        assert!(
            matches!(
                self.output_rx.try_recv(),
                Err(mpsc::error::TryRecvError::Disconnected)
            ),
            "Neither the child nor any later block may be published across the gap"
        );
        while let Ok(output) = self.account_output_rx.try_recv() {
            assert!(
                output.slot < slot,
                "Account output for slot {} crossed unresolved block {slot}",
                output.slot
            );
        }
        assert!(matches!(
            self.account_output_rx.try_recv(),
            Err(mpsc::error::TryRecvError::Disconnected)
        ));
    }

    async fn expect_no_account_flush(&mut self) {
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert!(
            self.account_output_rx.try_recv().is_err(),
            "Unexpected account flush"
        );
    }
}

// =============================================================================
// SubscribeUpdate Builders
// =============================================================================

fn make_slot_update(slot: u64, parent: u64, status: SlotStatus) -> SubscribeUpdate {
    SubscribeUpdate {
        filters: vec![],
        created_at: None,
        update_oneof: Some(UpdateOneof::Slot(SubscribeUpdateSlot {
            slot,
            parent: Some(parent),
            status: status.into(),
            dead_error: None,
            bank_id: Some(slot),
        })),
    }
}

fn make_entry_update(slot: u64, index: u64, tx_count: u64) -> SubscribeUpdate {
    SubscribeUpdate {
        filters: vec![],
        created_at: None,
        update_oneof: Some(UpdateOneof::Entry(SubscribeUpdateEntry {
            slot,
            index,
            num_hashes: 1,
            hash: Hash::new_unique().to_bytes().to_vec(),
            executed_transaction_count: tx_count,
            starting_transaction_index: 0,
            bank_id: slot,
        })),
    }
}

/// Stable identities let ordinary synthetic blocks name the actual hash of their parent.
fn block_hash(slot: u64) -> Hash { bank_block_hash(slot, slot) }

fn bank_block_hash(slot: u64, bank_id: u64) -> Hash {
    let mut bytes = [0_u8; 32];
    bytes[..8].copy_from_slice(&slot.to_le_bytes());
    bytes[8..16].copy_from_slice(&bank_id.to_le_bytes());
    bytes[31] = 1;
    Hash::new_from_array(bytes)
}

fn with_parent_blockhash(mut update: SubscribeUpdate, parent_blockhash: Hash) -> SubscribeUpdate {
    let Some(UpdateOneof::BlockMeta(meta)) = update.update_oneof.as_mut() else {
        panic!("Expected BlockMeta");
    };
    meta.parent_blockhash = parent_blockhash.to_string();
    update
}

fn make_block_meta_update(
    slot: u64,
    parent: u64,
    tx_count: u64,
    blockhash: &Hash,
) -> SubscribeUpdate {
    SubscribeUpdate {
        filters: vec![],
        created_at: None,
        update_oneof: Some(UpdateOneof::BlockMeta(SubscribeUpdateBlockMeta {
            slot,
            blockhash: bs58::encode(blockhash.as_ref()).into_string(),
            rewards: None,
            block_time: Some(UnixTimestamp {
                timestamp: 1700000000,
            }),
            block_height: Some(BlockHeight {
                block_height: slot - 1,
            }),
            parent_slot: parent,
            parent_blockhash: block_hash(parent).to_string(),
            executed_transaction_count: tx_count,
            entries_count: 1,
            bank_id: slot,
        })),
    }
}

fn with_entries_count(mut update: SubscribeUpdate, entries_count: u64) -> SubscribeUpdate {
    let Some(UpdateOneof::BlockMeta(meta)) = update.update_oneof.as_mut() else {
        panic!("Expected BlockMeta");
    };
    meta.entries_count = entries_count;
    update
}

async fn send_to_bank(harness: &TestHarness, update: SubscribeUpdate, bank_id: u64) {
    harness
        .input_tx
        .send(CoordinatorInput::GeyserUpdate(Box::new(with_bank_id(
            update, bank_id,
        ))))
        .await
        .unwrap();
}

fn with_bank_id(mut update: SubscribeUpdate, bank_id: u64) -> SubscribeUpdate {
    match update.update_oneof.as_mut().unwrap() {
        UpdateOneof::Slot(slot) => slot.bank_id = Some(bank_id),
        UpdateOneof::Entry(entry) => entry.bank_id = bank_id,
        UpdateOneof::BlockMeta(meta) => meta.bank_id = bank_id,
        _ => unreachable!(),
    }
    update
}

/// Two banks share slot 42. Bank 8 is confirmed while its transaction is still in
/// the handler. A late BlockMeta for the losing bank must not drop bank 8's records.
#[tokio::test]
async fn late_block_meta_for_losing_bank_keeps_confirmed_bank() {
    let mut harness = TestHarness::spawn();
    let (slot, parent) = (42, 41);
    let loser_blockhash = Hash::new_unique();

    // The loser goes last with the same keys, so a record that leaks into bank 8 replaces
    // bank 8's own record.
    for (bank_id, value, blockhash) in [
        (8, "winner", Hash::new_unique()),
        (7, "loser", loser_blockhash),
    ] {
        for update in [
            make_slot_update(slot, parent, SlotStatus::SlotCreatedBank),
            make_entry_update(slot, 0, 1),
            make_block_meta_update(slot, parent, 1, &blockhash),
        ] {
            send_to_bank(&harness, update, bank_id).await;
        }
        harness
            .input_tx
            .send(CoordinatorInput::AccountEventSeen { slot, bank_id })
            .await
            .unwrap();
        for message in [
            CoordinatorMessage::InstructionParsed {
                slot,
                bank_id,
                key: InstructionRecordSortKey::new(0, vec![0]),
                record: value.to_string(),
            },
            CoordinatorMessage::AccountParsed {
                slot,
                bank_id,
                key: AccountRecordSortKey::new(1, [1; 32]),
                record: value.to_string(),
            },
        ] {
            harness.parsed_tx.send(message).await.unwrap();
        }
    }
    // A loser count that leaks into bank 8 would flush bank 8 before its own count.
    harness
        .parsed_tx
        .send(CoordinatorMessage::TransactionParsed { slot, bank_id: 7 })
        .await
        .unwrap();

    for (update, bank_id) in [
        (make_slot_update(slot, parent, SlotStatus::SlotConfirmed), 8),
        (make_block_meta_update(slot, parent, 1, &loser_blockhash), 7),
    ] {
        send_to_bank(&harness, update, bank_id).await;
    }
    harness.expect_no_flush().await;

    harness
        .parsed_tx
        .send(CoordinatorMessage::TransactionParsed { slot, bank_id: 8 })
        .await
        .unwrap();

    harness.expect_flush(slot).await.records(&["winner"]);
    harness
        .expect_account_flush(slot)
        .await
        .records(&["winner"]);
}

// =============================================================================
// Flush Assertion
// =============================================================================

struct FlushAssertion(InstructionSlot<String>);

impl FlushAssertion {
    fn records(self, expected: &[&str]) {
        let expected: Vec<String> = expected.iter().map(|s| s.to_string()).collect();
        assert_eq!(self.0.records, expected, "Records mismatch");
    }

    fn tx_count(self, expected: u64) -> Self {
        assert_eq!(
            self.0.executed_transaction_count, expected,
            "Tx count mismatch"
        );
        self
    }

    fn parent(self, expected: u64) -> Self {
        assert_eq!(self.0.parent_slot, expected, "Parent mismatch");
        self
    }

    fn empty(self) {
        assert!(self.0.records.is_empty(), "Expected no records");
        assert_eq!(self.0.executed_transaction_count, 0, "Tx count mismatch");
    }
}

struct AccountFlushAssertion(AccountSlot<String>);

impl AccountFlushAssertion {
    fn records(self, expected: &[&str]) {
        let expected: Vec<String> = expected.iter().map(|s| s.to_string()).collect();
        assert_eq!(self.0.records, expected, "Account records mismatch");
    }
}

// =============================================================================
// Slot Builder
// =============================================================================

struct SlotBuilder {
    input_tx: mpsc::Sender<CoordinatorInput>,
    parsed_tx: mpsc::Sender<CoordinatorMessage<String>>,
    slot: u64,
    parent: u64,
    records: Vec<(InstructionRecordSortKey, String)>,
}

impl SlotBuilder {
    fn new(
        input_tx: mpsc::Sender<CoordinatorInput>,
        parsed_tx: mpsc::Sender<CoordinatorMessage<String>>,
        slot: u64,
    ) -> Self {
        Self {
            input_tx,
            parsed_tx,
            slot,
            parent: slot.saturating_sub(1),
            records: vec![],
        }
    }

    fn parent(mut self, parent: u64) -> Self {
        self.parent = parent;
        self
    }

    fn record(mut self, value: &str) -> Self {
        let tx_index = self.records.len() as u64;
        self.records.push((
            InstructionRecordSortKey::new(tx_index, vec![0]),
            value.to_string(),
        ));
        self
    }

    fn record_at(mut self, tx_index: u64, ix_path: Vec<usize>, value: &str) -> Self {
        self.records.push((
            InstructionRecordSortKey::new(tx_index, ix_path),
            value.to_string(),
        ));
        self
    }

    async fn empty(self) -> Slot { self.send_lifecycle(0).await }

    async fn parsed(self) -> Slot {
        let tx_count = self.records.len().max(1) as u64;
        let slot = self.send_lifecycle(tx_count).await;
        tokio::time::sleep(Duration::from_millis(5)).await;
        slot
    }

    async fn pending(self, expected_tx: u64) -> Slot {
        self.send_lifecycle_without_parsed(expected_tx).await
    }

    /// Send all block lifecycle events (FirstShredReceived, CreatedBank, Entry, Completed, BlockMeta).
    async fn send_block_events(&self, tx_count: u64) {
        let blockhash = block_hash(self.slot);

        for status in [
            SlotStatus::SlotFirstShredReceived,
            SlotStatus::SlotCreatedBank,
        ] {
            self.input_tx
                .send(CoordinatorInput::GeyserUpdate(Box::new(make_slot_update(
                    self.slot,
                    self.parent,
                    status,
                ))))
                .await
                .unwrap();
        }

        self.input_tx
            .send(CoordinatorInput::GeyserUpdate(Box::new(make_entry_update(
                self.slot, 0, tx_count,
            ))))
            .await
            .unwrap();

        self.input_tx
            .send(CoordinatorInput::GeyserUpdate(Box::new(make_slot_update(
                self.slot,
                self.parent,
                SlotStatus::SlotCompleted,
            ))))
            .await
            .unwrap();

        self.input_tx
            .send(CoordinatorInput::GeyserUpdate(Box::new(
                make_block_meta_update(self.slot, self.parent, tx_count, &blockhash),
            )))
            .await
            .unwrap();
    }

    async fn send_lifecycle_without_parsed(self, tx_count: u64) -> Slot {
        self.send_block_events(tx_count).await;
        tokio::time::sleep(Duration::from_millis(5)).await;

        Slot {
            input_tx: self.input_tx,
            parsed_tx: self.parsed_tx,
            slot: self.slot,
            parent: self.parent,
            next_tx_index: std::sync::atomic::AtomicU64::new(0),
        }
    }

    async fn send_lifecycle(self, tx_count: u64) -> Slot {
        self.send_block_events(tx_count).await;

        // Send parsed records
        for (key, record) in &self.records {
            self.parsed_tx
                .send(CoordinatorMessage::InstructionParsed {
                    slot: self.slot,
                    bank_id: self.slot,
                    key: key.clone(),
                    record: record.clone(),
                })
                .await
                .unwrap();
        }

        // Send TransactionParsed signals
        for _ in 0..tx_count {
            self.parsed_tx
                .send(CoordinatorMessage::TransactionParsed {
                    slot: self.slot,
                    bank_id: self.slot,
                })
                .await
                .unwrap();
        }

        Slot {
            input_tx: self.input_tx,
            parsed_tx: self.parsed_tx,
            slot: self.slot,
            parent: self.parent,
            next_tx_index: std::sync::atomic::AtomicU64::new(tx_count),
        }
    }
}

// =============================================================================
// Slot (post-lifecycle handle)
// =============================================================================

struct Slot {
    input_tx: mpsc::Sender<CoordinatorInput>,
    parsed_tx: mpsc::Sender<CoordinatorMessage<String>>,
    slot: u64,
    parent: u64,
    next_tx_index: std::sync::atomic::AtomicU64,
}

impl Slot {
    /// Confirm and freeze account count for the account gate.
    async fn confirm(&self) {
        tokio::time::sleep(Duration::from_millis(5)).await;
        self.send_commitment(SlotStatus::SlotConfirmed).await;
    }

    /// Send N AccountEventSeen signals then Slot(Confirmed).
    async fn confirm_with_accounts(&self, account_count: u64) {
        tokio::time::sleep(Duration::from_millis(5)).await;
        for _ in 0..account_count {
            self.input_tx
                .send(CoordinatorInput::AccountEventSeen {
                    slot: self.slot,
                    bank_id: self.slot,
                })
                .await
                .unwrap();
        }
        self.send_commitment(SlotStatus::SlotConfirmed).await;
    }

    async fn finalize(&self) {
        tokio::time::sleep(Duration::from_millis(5)).await;
        self.send_commitment(SlotStatus::SlotFinalized).await;
    }

    async fn send_commitment(&self, status: SlotStatus) {
        self.input_tx
            .send(CoordinatorInput::GeyserUpdate(Box::new(make_slot_update(
                self.slot,
                self.parent,
                status,
            ))))
            .await
            .unwrap();
    }

    async fn kill(&self) {
        self.input_tx
            .send(CoordinatorInput::GeyserUpdate(Box::new(make_slot_update(
                self.slot,
                self.parent,
                SlotStatus::SlotDead,
            ))))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(5)).await;
    }

    async fn late_record(&self, value: &str) {
        let tx_index = self
            .next_tx_index
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.parsed_tx
            .send(CoordinatorMessage::InstructionParsed {
                slot: self.slot,
                bank_id: self.slot,
                key: InstructionRecordSortKey::new(tx_index, vec![0]),
                record: value.to_string(),
            })
            .await
            .unwrap();
        self.parsed_tx
            .send(CoordinatorMessage::TransactionParsed {
                slot: self.slot,
                bank_id: self.slot,
            })
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

// Standalone helper for sending records to arbitrary slots (no Slot handle needed)
async fn send_record_to_slot(
    parsed_tx: &mpsc::Sender<CoordinatorMessage<String>>,
    slot: u64,
    value: &str,
) {
    parsed_tx
        .send(CoordinatorMessage::InstructionParsed {
            slot,
            bank_id: slot,
            key: InstructionRecordSortKey::new(0, vec![0]),
            record: value.to_string(),
        })
        .await
        .unwrap();
    parsed_tx
        .send(CoordinatorMessage::TransactionParsed {
            slot,
            bank_id: slot,
        })
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(5)).await;
}

// =============================================================================
// Tests
// =============================================================================

#[tokio::test]
async fn two_gate_flush_end_to_end() {
    let mut harness = TestHarness::spawn();

    let slot = harness
        .slot(100)
        .parent(99)
        .record_at(1, vec![0], "tx1-ix0")
        .record_at(0, vec![0, 1], "tx0-cpi")
        .record_at(0, vec![0], "tx0-ix0")
        .send_lifecycle(2)
        .await;

    harness.expect_no_flush().await;
    slot.confirm().await;

    harness
        .expect_flush(100)
        .await
        .parent(99)
        .tx_count(2)
        .records(&["tx0-ix0", "tx0-cpi", "tx1-ix0"]);
}

#[tokio::test]
async fn sequential_flush_order() {
    let mut harness = TestHarness::spawn();

    let slot_101 = harness.slot(101).parent(100).pending(1).await;
    let slot_102 = harness
        .slot(102)
        .parent(101)
        .record("rec-102")
        .parsed()
        .await;

    slot_102.confirm().await;
    // The child's hash can now confirm its parent, but the parent's parser must still
    // finish before either block is published.
    harness.expect_no_flush().await;

    slot_101.confirm().await;
    harness.expect_no_flush().await;
    slot_101.late_record("rec-101").await;

    harness.expect_flush(101).await.records(&["rec-101"]);
    harness.expect_flush(102).await.records(&["rec-102"]);
}

#[tokio::test]
async fn dead_slot_discarded() {
    let mut harness = TestHarness::spawn();

    let slot = harness
        .slot(100)
        .parent(99)
        .record("will-die")
        .parsed()
        .await;
    slot.kill().await;

    harness.expect_no_flush().await;
}

#[tokio::test]
async fn dead_slot_spares_sibling() {
    let mut harness = TestHarness::spawn();

    // Parent slot — confirmed and flushed first.
    let parent = harness.slot(100).parent(99).empty().await;
    parent.confirm().await;
    harness.expect_flush(100).await;

    // Two siblings (same parent=100). Killing one should NOT propagate to the other.
    let blocker = harness
        .slot(101)
        .parent(100)
        .record("blocker")
        .parsed()
        .await;
    let waiting = harness
        .slot(102)
        .parent(100)
        .record("survives")
        .parsed()
        .await;

    // Confirming 102 first would make the block machine skip sibling 101 by itself.
    blocker.kill().await;
    waiting.confirm().await;

    harness.expect_flush(102).await.records(&["survives"]);
}

#[tokio::test]
async fn empty_slot_flushes() {
    let mut harness = TestHarness::spawn();

    let slot = harness.slot(100).parent(99).empty().await;
    slot.confirm().await;

    harness.expect_flush(100).await.empty();
}

#[tokio::test]
async fn incomplete_confirmed_block_stops_before_child_flush() {
    let mut harness = TestHarness::spawn();

    // Establish the ingestion boundary before introducing the missing block.
    let first = harness.slot(99).parent(98).empty().await;
    first.confirm().await;
    harness.expect_flush(99).await.empty();

    for update in [
        make_slot_update(100, 99, SlotStatus::SlotCreatedBank),
        make_block_meta_update(100, 99, 0, &block_hash(100)),
        make_slot_update(100, 99, SlotStatus::SlotConfirmed),
    ] {
        send_to_bank(&harness, update, 100).await;
    }
    send_record_to_slot(&harness.parsed_tx, 100, "missing-entry").await;
    harness.expect_no_flush().await;

    let next = harness.slot(101).parent(100).empty().await;
    next.confirm().await;
    harness.expect_no_flush().await;

    let deadline = 100 + MISSING_PARENT_GRACE_SLOTS + 1;
    for slot in 102..=deadline {
        for update in [
            make_entry_update(slot, 0, 0),
            make_block_meta_update(slot, slot - 1, 0, &block_hash(slot)),
        ] {
            send_to_bank(&harness, update, slot).await;
        }
    }

    harness.expect_unresolved(100, deadline).await;

    // Recovery must replay the missing block; later live data cannot silently skip it.
    assert!(harness
        .input_tx
        .send(CoordinatorInput::GeyserUpdate(Box::new(make_entry_update(
            100, 0, 0
        ))))
        .await
        .is_err());
}

async fn assert_metadata_before_entries(parser_finishes_first: bool) {
    let mut harness = TestHarness::spawn();
    for update in [
        make_slot_update(100, 99, SlotStatus::SlotCreatedBank),
        with_entries_count(make_block_meta_update(100, 99, 2, &block_hash(100)), 2),
        make_slot_update(100, 99, SlotStatus::SlotConfirmed),
    ] {
        send_to_bank(&harness, update, 100).await;
    }
    if parser_finishes_first {
        for index in 0..2 {
            harness
                .parsed_tx
                .send(CoordinatorMessage::InstructionParsed {
                    slot: 100,
                    bank_id: 100,
                    key: InstructionRecordSortKey::new(index, vec![0]),
                    record: format!("record-{index}"),
                })
                .await
                .unwrap();
            harness
                .parsed_tx
                .send(CoordinatorMessage::TransactionParsed {
                    slot: 100,
                    bank_id: 100,
                })
                .await
                .unwrap();
        }
    }
    harness.expect_no_flush().await;
    harness.expect_no_account_flush().await;

    send_to_bank(&harness, make_entry_update(100, 0, 1), 100).await;
    harness.expect_no_flush().await;
    send_to_bank(&harness, make_entry_update(100, 1, 1), 100).await;

    if !parser_finishes_first {
        // Receiving every entry satisfies reconstruction, but not the independent parser gate.
        harness.expect_no_flush().await;
        for index in 0..2 {
            harness
                .parsed_tx
                .send(CoordinatorMessage::InstructionParsed {
                    slot: 100,
                    bank_id: 100,
                    key: InstructionRecordSortKey::new(index, vec![0]),
                    record: format!("record-{index}"),
                })
                .await
                .unwrap();
            harness
                .parsed_tx
                .send(CoordinatorMessage::TransactionParsed {
                    slot: 100,
                    bank_id: 100,
                })
                .await
                .unwrap();
            if index == 0 {
                harness.expect_no_flush().await;
            }
        }
    }
    harness
        .expect_flush(100)
        .await
        .parent(99)
        .tx_count(2)
        .records(&["record-0", "record-1"]);
    harness.expect_account_flush(100).await.records(&[]);
    harness.expect_no_flush().await;
}

#[tokio::test]
async fn metadata_and_confirmation_before_entries_with_parser_already_done() {
    assert_metadata_before_entries(true).await;
}

#[tokio::test]
async fn metadata_and_confirmation_before_entries_still_wait_for_parser() {
    assert_metadata_before_entries(false).await;
}

#[tokio::test]
async fn out_of_order_duplicate_entries_do_not_complete_pending_metadata() {
    let mut harness = TestHarness::spawn();
    let last_entry = make_entry_update(100, 2, 1);
    for update in [
        make_slot_update(100, 99, SlotStatus::SlotCreatedBank),
        last_entry.clone(),
        with_entries_count(make_block_meta_update(100, 99, 2, &block_hash(100)), 3),
        make_slot_update(100, 99, SlotStatus::SlotConfirmed),
        last_entry,
    ] {
        send_to_bank(&harness, update, 100).await;
    }
    for _ in 0..2 {
        harness
            .parsed_tx
            .send(CoordinatorMessage::TransactionParsed {
                slot: 100,
                bank_id: 100,
            })
            .await
            .unwrap();
    }
    harness.expect_no_flush().await;

    let first_entry = make_entry_update(100, 0, 1);
    send_to_bank(&harness, first_entry.clone(), 100).await;
    send_to_bank(&harness, first_entry, 100).await;
    // Both transaction counts now match, but the zero-transaction entry is still missing.
    harness.expect_no_flush().await;
    send_to_bank(&harness, make_entry_update(100, 1, 0), 100).await;
    harness.expect_flush(100).await.tx_count(2).records(&[]);
    harness.expect_no_flush().await;
}

#[tokio::test]
async fn pending_metadata_does_not_mix_entries_from_competing_banks() {
    let mut harness = TestHarness::spawn();
    for (bank_id, entry_index, record) in [(1000, 0, "loser"), (1001, 1, "winner")] {
        for update in [
            make_slot_update(100, 99, SlotStatus::SlotCreatedBank),
            make_entry_update(100, entry_index, 1),
            with_entries_count(make_block_meta_update(100, 99, 2, &block_hash(100)), 2),
        ] {
            send_to_bank(&harness, update, bank_id).await;
        }
        harness
            .parsed_tx
            .send(CoordinatorMessage::InstructionParsed {
                slot: 100,
                bank_id,
                key: InstructionRecordSortKey::new(0, vec![0]),
                record: record.to_string(),
            })
            .await
            .unwrap();
        for _ in 0..2 {
            harness
                .parsed_tx
                .send(CoordinatorMessage::TransactionParsed { slot: 100, bank_id })
                .await
                .unwrap();
        }
    }
    send_to_bank(
        &harness,
        make_slot_update(100, 99, SlotStatus::SlotConfirmed),
        1001,
    )
    .await;
    // Combining bank 1000's entry 0 and bank 1001's entry 1 would incorrectly complete it.
    harness.expect_no_flush().await;
    send_to_bank(&harness, make_entry_update(100, 0, 1), 1001).await;
    harness
        .expect_flush(100)
        .await
        .tx_count(2)
        .records(&["winner"]);
    // Completing the losing bank afterwards cannot create a second output.
    send_to_bank(&harness, make_entry_update(100, 1, 1), 1000).await;
    harness.expect_no_flush().await;
}

#[tokio::test]
async fn cleared_pending_metadata_does_not_block_replacement_bank() {
    let mut harness = TestHarness::spawn();
    for update in [
        make_slot_update(100, 99, SlotStatus::SlotCreatedBank),
        make_block_meta_update(100, 99, 1, &block_hash(100)),
    ] {
        send_to_bank(&harness, update, 1000).await;
    }
    harness
        .input_tx
        .send(CoordinatorInput::GeyserUpdate(Box::new(SubscribeUpdate {
            update_oneof: Some(UpdateOneof::EntryUpdateParent(
                SubscribeUpdateEntryUpdateParent {
                    slot: 100,
                    cleared_bank_id: 1000,
                    parent_slot: 98,
                    ..Default::default()
                },
            )),
            ..Default::default()
        })))
        .await
        .unwrap();
    // Let invalidation reach the coordinator before delayed parsed output arrives.
    drop(
        harness
            .input_tx
            .reserve_many(harness.input_tx.max_capacity())
            .await
            .unwrap(),
    );
    for message in [
        CoordinatorMessage::InstructionParsed {
            slot: 100,
            bank_id: 1000,
            key: InstructionRecordSortKey::new(0, vec![0]),
            record: "cleared".to_string(),
        },
        CoordinatorMessage::TransactionParsed {
            slot: 100,
            bank_id: 1000,
        },
    ] {
        harness.parsed_tx.send(message).await.unwrap();
    }
    for update in [
        make_entry_update(100, 0, 1),
        make_slot_update(100, 99, SlotStatus::SlotProcessed),
    ] {
        send_to_bank(&harness, update, 1000).await;
    }
    let replacement = harness
        .slot(100)
        .parent(98)
        .record("replacement")
        .parsed()
        .await;
    replacement.confirm().await;
    harness
        .expect_flush(100)
        .await
        .parent(98)
        .records(&["replacement"]);
    harness.expect_no_flush().await;
}

#[tokio::test]
async fn dead_metadata_first_bank_cannot_revive_and_replacement_can_flush() {
    let mut harness = TestHarness::spawn();
    // No CreatedBank or Entry has registered this bank inside the wrapper yet.
    for update in [
        make_block_meta_update(100, 99, 1, &block_hash(100)),
        make_slot_update(100, 99, SlotStatus::SlotDead),
    ] {
        send_to_bank(&harness, update, 1000).await;
    }
    drop(
        harness
            .input_tx
            .reserve_many(harness.input_tx.max_capacity())
            .await
            .unwrap(),
    );

    for message in [
        CoordinatorMessage::InstructionParsed {
            slot: 100,
            bank_id: 1000,
            key: InstructionRecordSortKey::new(0, vec![0]),
            record: "dead".to_string(),
        },
        CoordinatorMessage::TransactionParsed {
            slot: 100,
            bank_id: 1000,
        },
    ] {
        harness.parsed_tx.send(message).await.unwrap();
    }
    for update in [
        make_entry_update(100, 0, 1),
        make_block_meta_update(100, 99, 1, &block_hash(100)),
        make_slot_update(100, 99, SlotStatus::SlotProcessed),
    ] {
        send_to_bank(&harness, update, 1000).await;
    }
    harness.expect_no_flush().await;

    let replacement = harness
        .slot(100)
        .parent(99)
        .record("replacement")
        .parsed()
        .await;
    replacement.confirm().await;
    harness.expect_flush(100).await.records(&["replacement"]);
    harness.expect_no_flush().await;
}

#[tokio::test]
async fn frozen_loser_does_not_hide_missing_entries_of_confirmed_winner() {
    let mut harness = TestHarness::spawn();
    let root = harness.slot(99).parent(98).empty().await;
    root.confirm().await;
    harness.expect_flush(99).await.empty();
    for update in [
        make_slot_update(100, 99, SlotStatus::SlotCreatedBank),
        make_entry_update(100, 0, 0),
        make_block_meta_update(100, 99, 0, &bank_block_hash(100, 1000)),
    ] {
        send_to_bank(&harness, update, 1000).await;
    }
    for update in [
        make_slot_update(100, 99, SlotStatus::SlotCreatedBank),
        make_block_meta_update(100, 99, 1, &block_hash(100)),
        make_slot_update(100, 99, SlotStatus::SlotConfirmed),
    ] {
        send_to_bank(&harness, update, 1001).await;
    }
    for message in [
        CoordinatorMessage::InstructionParsed {
            slot: 100,
            bank_id: 1001,
            key: InstructionRecordSortKey::new(0, vec![0]),
            record: "missing-winner-entry".to_string(),
        },
        CoordinatorMessage::TransactionParsed {
            slot: 100,
            bank_id: 1001,
        },
    ] {
        harness.parsed_tx.send(message).await.unwrap();
    }
    let child = harness.slot(101).parent(100).empty().await;
    child.confirm().await;
    harness.expect_no_flush().await;

    let deadline = 100 + MISSING_PARENT_GRACE_SLOTS + 1;
    for slot in 102..=deadline {
        for update in [
            make_entry_update(slot, 0, 0),
            make_block_meta_update(slot, slot - 1, 0, &block_hash(slot)),
        ] {
            send_to_bank(&harness, update, slot).await;
        }
    }
    // An incomplete winner requires recovery; the complete loser cannot fill the gap.
    harness.expect_unresolved(100, deadline).await;
}

#[tokio::test]
async fn confirmed_child_waits_for_late_parent_entries() {
    let mut harness = TestHarness::spawn();
    let root = harness.slot(99).parent(98).empty().await;
    root.confirm().await;
    harness.expect_flush(99).await.empty();
    for update in [
        make_slot_update(100, 99, SlotStatus::SlotCreatedBank),
        make_block_meta_update(100, 99, 1, &block_hash(100)),
        make_slot_update(100, 99, SlotStatus::SlotConfirmed),
    ] {
        send_to_bank(&harness, update, 100).await;
    }
    send_record_to_slot(&harness.parsed_tx, 100, "parent").await;
    let child = harness.slot(101).parent(100).record("child").parsed().await;
    child.confirm().await;
    harness.expect_no_flush().await;

    send_to_bank(&harness, make_entry_update(100, 0, 1), 100).await;
    harness
        .expect_flush(100)
        .await
        .tx_count(1)
        .records(&["parent"]);
    harness
        .expect_flush(101)
        .await
        .tx_count(1)
        .records(&["child"]);
    harness.expect_no_flush().await;
}

#[tokio::test]
async fn discarded_slot_ignores_parsed_messages() {
    let mut harness = TestHarness::spawn();

    let doomed = harness.slot(100).parent(99).record("doomed").parsed().await;
    let survivor = harness.slot(101).parent(99).empty().await;

    doomed.kill().await;
    doomed.late_record("should-be-ignored").await;
    harness.expect_no_flush().await;

    survivor.late_record("valid").await;
    survivor.confirm().await;

    harness.expect_flush(101).await.records(&["valid"]);
}

#[tokio::test]
async fn sibling_fork_via_finalized() {
    let mut harness = TestHarness::spawn();

    let parent = harness.slot(100).parent(99).empty().await;
    parent.confirm().await;

    let winner = harness
        .slot(101)
        .parent(100)
        .record("winner")
        .parsed()
        .await;
    let _loser = harness.slot(102).parent(100).record("loser").parsed().await;

    winner.confirm().await;
    harness.expect_flush(100).await;
    harness.expect_flush(101).await.records(&["winner"]);

    // The forked sibling keeps its records but must not block a later slot on the winner.
    winner.finalize().await;
    let next = harness.slot(103).parent(101).empty().await;
    next.confirm().await;
    harness.expect_flush(103).await;
}

// =============================================================================
// Edge Case Tests
// =============================================================================

#[tokio::test]
async fn parsed_messages_before_lifecycle_are_buffered() {
    let mut harness = TestHarness::spawn();

    send_record_to_slot(&harness.parsed_tx, 100, "early-bird").await;

    let slot = harness.slot(100).parent(99).empty().await;
    slot.confirm().await;

    harness.expect_flush(100).await.records(&["early-bird"]);
}

#[tokio::test]
async fn incomplete_slot_blocks_subsequent() {
    let mut harness = TestHarness::spawn();

    let incomplete = harness.slot(100).parent(99).pending(3).await;
    let complete = harness.slot(101).parent(100).record("ready").parsed().await;

    complete.confirm().await;
    harness.expect_no_flush().await;

    incomplete.late_record("a").await;
    incomplete.late_record("b").await;
    incomplete.late_record("c").await;
    incomplete.confirm().await;

    harness.expect_flush(100).await.records(&["a", "b", "c"]);
    harness.expect_flush(101).await.records(&["ready"]);
}

#[tokio::test]
async fn gap_in_sequence_blocks_flush() {
    let mut harness = TestHarness::spawn();

    let slot_100 = harness.slot(100).parent(99).empty().await;
    let slot_102 = harness
        .slot(102)
        .parent(101)
        .record("waiting")
        .parsed()
        .await;

    slot_100.confirm().await;
    harness.expect_flush(100).await;

    slot_102.confirm().await;
    harness.expect_no_flush().await;

    let slot_101 = harness.slot(101).parent(100).empty().await;
    slot_101.confirm().await;

    harness.expect_flush(101).await;
    harness.expect_flush(102).await.records(&["waiting"]);
}

/// Observing a bank establishes the startup boundary before any complete block flushes.
#[tokio::test]
async fn first_child_cannot_anchor_over_an_already_observed_parent() {
    let mut harness = TestHarness::spawn();
    send_to_bank(
        &harness,
        make_slot_update(100, 99, SlotStatus::SlotCreatedBank),
        100,
    )
    .await;
    let child = harness.slot(101).parent(100).empty().await;
    child.confirm().await;
    harness.expect_no_flush().await;
    harness.expect_no_account_flush().await;

    let parent = harness.slot(100).parent(99).empty().await;
    parent.confirm().await;
    harness.expect_flush(100).await.empty();
    harness.expect_flush(101).await.empty();
    harness.expect_account_flush(100).await.records(&[]);
    harness.expect_account_flush(101).await.records(&[]);
}

/// A canonical parent can be absent from every input channel, not just lack metadata.
#[tokio::test]
async fn entirely_missing_parent_stops_before_child_flush() {
    let mut harness = TestHarness::spawn();
    let first = harness.slot(100).parent(99).empty().await;
    first.confirm().await;
    harness.expect_flush(100).await.empty();

    let child = harness.slot(102).parent(101).empty().await;
    child.confirm().await;
    harness.expect_no_flush().await;
    let deadline = 101 + MISSING_PARENT_GRACE_SLOTS + 1;
    for slot in 103..=deadline {
        harness.slot(slot).parent(slot - 1).empty().await;
    }
    harness.expect_unresolved(101, deadline).await;
}

/// A finite or disconnected stream must not report success with a required block missing.
#[tokio::test]
async fn stream_closes_with_missing_parent_returns_error_without_descendant_output() {
    let (input_tx, input_rx) = mpsc::channel(32);
    let (parsed_tx, parsed_rx) = mpsc::channel::<CoordinatorMessage<String>>(1);
    let (output_tx, mut output_rx) = mpsc::channel(8);
    let (account_output_tx, mut account_output_rx) = mpsc::channel(8);
    let coordinator = tokio::spawn(BlockMachineCoordinator::run(
        input_rx,
        parsed_rx,
        Some(output_tx),
        Some(account_output_tx),
        AccountCommitAt::Confirmed,
        true,
    ));
    for update in [
        make_slot_update(100, 99, SlotStatus::SlotCreatedBank),
        make_entry_update(100, 0, 0),
        make_block_meta_update(100, 99, 0, &block_hash(100)),
        make_slot_update(100, 99, SlotStatus::SlotConfirmed),
    ] {
        input_tx
            .send(CoordinatorInput::GeyserUpdate(Box::new(update)))
            .await
            .unwrap();
    }
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), output_rx.recv())
            .await
            .unwrap()
            .unwrap()
            .slot,
        100
    );
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), account_output_rx.recv())
            .await
            .unwrap()
            .unwrap()
            .slot,
        100
    );

    // Slot 101 is never observed at all, but the confirmed chain requires it.
    for slot in [102, 103] {
        for update in [
            make_slot_update(slot, slot - 1, SlotStatus::SlotCreatedBank),
            make_entry_update(slot, 0, 0),
            make_block_meta_update(slot, slot - 1, 0, &block_hash(slot)),
            make_slot_update(slot, slot - 1, SlotStatus::SlotConfirmed),
        ] {
            input_tx
                .send(CoordinatorInput::GeyserUpdate(Box::new(update)))
                .await
                .unwrap();
        }
    }
    drop(input_tx);
    drop(parsed_tx);
    let result = tokio::time::timeout(Duration::from_secs(2), coordinator)
        .await
        .expect("A closed stream must report its canonical gap")
        .expect("Coordinator must not panic");
    assert!(
        matches!(
            result,
            Err(CoordinatorError::UnresolvedBlock {
                slot: 101,
                observed_slot: 103
            })
        ),
        "Missing canonical data must not be reported as a successful shutdown: {result:?}"
    );
    assert!(
        matches!(
            output_rx.try_recv(),
            Err(mpsc::error::TryRecvError::Disconnected)
        ),
        "No descendant instructions may cross the missing parent"
    );
    assert!(
        matches!(
            account_output_rx.try_recv(),
            Err(mpsc::error::TryRecvError::Disconnected)
        ),
        "No descendant accounts may cross the missing parent"
    );
}

/// Instruction publication does not make a missing account parse safe to drop on shutdown.
#[tokio::test]
async fn stream_closes_with_unparsed_accounts_requires_recovery() {
    let (input_tx, input_rx) = mpsc::channel(16);
    let (parsed_tx, parsed_rx) = mpsc::channel::<CoordinatorMessage<String>>(1);
    let (output_tx, mut output_rx) = mpsc::channel(8);
    let (account_output_tx, mut account_output_rx) = mpsc::channel(8);
    let coordinator = tokio::spawn(BlockMachineCoordinator::run(
        input_rx,
        parsed_rx,
        Some(output_tx),
        Some(account_output_tx),
        AccountCommitAt::Confirmed,
        true,
    ));
    for update in [
        make_slot_update(100, 99, SlotStatus::SlotCreatedBank),
        make_entry_update(100, 0, 0),
        make_block_meta_update(100, 99, 0, &block_hash(100)),
    ] {
        input_tx
            .send(CoordinatorInput::GeyserUpdate(Box::new(update)))
            .await
            .unwrap();
    }
    input_tx
        .send(CoordinatorInput::AccountEventSeen {
            slot: 100,
            bank_id: 100,
        })
        .await
        .unwrap();
    input_tx
        .send(CoordinatorInput::GeyserUpdate(Box::new(make_slot_update(
            100,
            99,
            SlotStatus::SlotConfirmed,
        ))))
        .await
        .unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), output_rx.recv())
            .await
            .unwrap()
            .unwrap()
            .slot,
        100
    );
    assert!(
        account_output_rx.try_recv().is_err(),
        "The account parser has not completed"
    );

    drop(input_tx);
    drop(parsed_tx);
    let result = tokio::time::timeout(Duration::from_secs(2), coordinator)
        .await
        .expect("The missing account parse must be reported on stream close")
        .expect("Coordinator must not panic");
    assert!(
        matches!(
            result,
            Err(CoordinatorError::UnresolvedBlock {
                slot: 100,
                observed_slot: 100
            })
        ),
        "An instruction flush must not hide missing account output: {result:?}"
    );
    assert!(
        matches!(
            account_output_rx.try_recv(),
            Err(mpsc::error::TryRecvError::Disconnected)
        ),
        "An incomplete account block must remain unpublished"
    );
    assert!(matches!(
        output_rx.try_recv(),
        Err(mpsc::error::TryRecvError::Disconnected)
    ));
}

/// The raw commitment is authoritative even while late entries delay the wrapper's confirmation.
#[tokio::test]
async fn clearing_raw_confirmed_bank_with_pending_metadata_requires_recovery() {
    let mut harness = TestHarness::spawn();
    for update in [
        make_slot_update(100, 99, SlotStatus::SlotCreatedBank),
        make_block_meta_update(100, 99, 1, &block_hash(100)),
        make_slot_update(100, 99, SlotStatus::SlotConfirmed),
    ] {
        send_to_bank(&harness, update, 1000).await;
    }
    harness.expect_no_flush().await;
    harness
        .input_tx
        .send(CoordinatorInput::GeyserUpdate(Box::new(SubscribeUpdate {
            update_oneof: Some(UpdateOneof::EntryUpdateParent(
                SubscribeUpdateEntryUpdateParent {
                    slot: 100,
                    cleared_bank_id: 1000,
                    parent_slot: 98,
                    ..Default::default()
                },
            )),
            ..Default::default()
        })))
        .await
        .unwrap();
    harness.expect_unresolved(100, 100).await;
}

#[tokio::test]
async fn late_message_for_flushed_slot_is_dropped() {
    let (input_tx, input_rx) = mpsc::channel::<CoordinatorInput>(256);
    let (parsed_tx, parsed_rx) = mpsc::channel(256);
    let (output_tx, mut output_rx) = mpsc::channel(64);

    let handle = tokio::spawn(BlockMachineCoordinator::run(
        input_rx,
        parsed_rx,
        Some(output_tx),
        None,
        AccountCommitAt::Confirmed,
        true,
    ));

    // Create and flush slot 100
    let slot = SlotBuilder::new(input_tx.clone(), parsed_tx.clone(), 100)
        .parent(99)
        .empty()
        .await;

    slot.confirm().await;

    let flushed = tokio::time::timeout(Duration::from_secs(2), output_rx.recv())
        .await
        .expect("Timed out")
        .expect("Channel closed");
    assert_eq!(flushed.slot, 100);

    // Send late message. Restarting from the slot topic would resume after this
    // stale slot anyway, so the coordinator drops it without terminating.
    send_record_to_slot(&parsed_tx, 100, "too-late").await;

    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(
        !handle.is_finished(),
        "Coordinator should keep running after stale parsed events"
    );
    assert!(output_rx.try_recv().is_err(), "Unexpected stale flush");

    drop(slot);
    drop(input_tx);
    drop(parsed_tx);

    let result = tokio::time::timeout(Duration::from_secs(2), handle)
        .await
        .expect("Timed out waiting for coordinator shutdown")
        .expect("task join");
    assert!(
        result.is_ok(),
        "Expected clean coordinator shutdown, got: {result:?}"
    );
}

#[tokio::test]
async fn output_channel_closed_returns_error() {
    let (input_tx, input_rx) = mpsc::channel::<CoordinatorInput>(256);
    let (parsed_tx, parsed_rx) = mpsc::channel(256);
    let (output_tx, output_rx) = mpsc::channel(64);

    let handle = tokio::spawn(BlockMachineCoordinator::run(
        input_rx,
        parsed_rx,
        Some(output_tx),
        None,
        AccountCommitAt::Confirmed,
        true,
    ));

    // Drop the output receiver — the coordinator can't send flushed slots.
    drop(output_rx);

    // Create a ready slot that will try to flush.
    let slot = SlotBuilder::new(input_tx.clone(), parsed_tx.clone(), 100)
        .parent(99)
        .empty()
        .await;

    slot.confirm().await;

    let result = handle.await.expect("task join");
    assert!(
        matches!(
            result,
            Err(CoordinatorError::InstructionOutputChannelClosed { slot: 100 })
        ),
        "Expected InstructionOutputChannelClosed, got: {result:?}"
    );
}

// =============================================================================
// Account Gate Tests
// =============================================================================

#[tokio::test]
async fn account_gate_blocks_flush_until_account_count_received() {
    let mut harness = TestHarness::spawn();

    // Slot with 2 expected accounts — won't flush until account count is delivered.
    let slot = harness.slot(100).parent(99).empty().await;

    // Simulate 2 account handler messages.
    harness
        .parsed_tx
        .send(CoordinatorMessage::AccountParsed {
            slot: 100,
            bank_id: 100,
            key: shipstern_block_coordinator::AccountRecordSortKey::new(1, [1; 32]),
            record: "acct1".to_string(),
        })
        .await
        .unwrap();
    harness
        .parsed_tx
        .send(CoordinatorMessage::AccountParsed {
            slot: 100,
            bank_id: 100,
            key: shipstern_block_coordinator::AccountRecordSortKey::new(2, [2; 32]),
            record: "acct2".to_string(),
        })
        .await
        .unwrap();

    // Confirm with account_count=2 — now all gates satisfied.
    slot.confirm_with_accounts(2).await;

    // Instructions flush immediately (no instruction records, 0 tx).
    harness.expect_flush(100).await;
    // Accounts flush with the 2 records.
    let acct_flushed = harness.expect_account_flush(100).await;
    acct_flushed.records(&["acct1", "acct2"]);
}

#[tokio::test]
async fn account_only_mode_flushes_without_transaction_parsed_messages() {
    let mut harness = TestHarness::spawn_with_require_tx_gate(false);

    // Slot has non-zero tx count from Entry/BlockMeta but sends no TransactionParsed events.
    let slot = harness.slot(100).parent(99).pending(3).await;

    // Freeze account gate with one account event.
    slot.confirm_with_accounts(1).await;

    // Instructions flush immediately (require_tx_gate=false → expected_tx_count=0).
    harness.expect_flush(100).await.tx_count(0);

    // Accounts wait for the 1 expected account to be processed.
    harness.expect_no_account_flush().await;

    // Satisfy with one parsed account record.
    harness
        .parsed_tx
        .send(CoordinatorMessage::AccountParsed {
            slot: 100,
            bank_id: 100,
            key: shipstern_block_coordinator::AccountRecordSortKey::new(1, [1; 32]),
            record: "acct1".to_string(),
        })
        .await
        .unwrap();

    let acct = harness.expect_account_flush(100).await;
    acct.records(&["acct1"]);
}

#[tokio::test]
async fn account_event_after_confirmed_is_warn_not_error() {
    let mut harness = TestHarness::spawn();

    // Create a slot with 1 account event before confirm, so account gate is frozen.
    let slot = harness.slot(100).parent(99).empty().await;
    slot.confirm_with_accounts(1).await;

    // Instructions flush immediately (no tx).
    harness.expect_flush(100).await;

    // Accounts NOT flushed yet (1 expected, 0 processed).
    harness.expect_no_account_flush().await;

    // Send another AccountEventSeen after confirmed — should be warn + drop, not error.
    // (Account gate is frozen at confirm.)
    harness
        .input_tx
        .send(CoordinatorInput::AccountEventSeen {
            slot: 100,
            bank_id: 100,
        })
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(10)).await;

    // The coordinator should still be alive — process the 1 expected account.
    harness
        .parsed_tx
        .send(CoordinatorMessage::AccountParsed {
            slot: 100,
            bank_id: 100,
            key: shipstern_block_coordinator::AccountRecordSortKey::new(1, [1; 32]),
            record: "acct1".to_string(),
        })
        .await
        .unwrap();

    let acct = harness.expect_account_flush(100).await;
    acct.records(&["acct1"]);
}

#[tokio::test]
async fn duplicate_confirm_does_not_change_frozen_count() {
    let mut harness = TestHarness::spawn();

    // Create a slot, send 2 AccountEventSeen, confirm (freezes count=2).
    let slot = harness.slot(100).parent(99).empty().await;
    slot.confirm_with_accounts(2).await;

    // Instructions flush immediately.
    harness.expect_flush(100).await;

    // Confirm again (duplicate) — first-write-wins in set_expected_account_count.
    slot.confirm().await;

    // Send 2 AccountRecordParsed → account slot flushes (count matches 2).
    harness
        .parsed_tx
        .send(CoordinatorMessage::AccountParsed {
            slot: 100,
            bank_id: 100,
            key: shipstern_block_coordinator::AccountRecordSortKey::new(1, [1; 32]),
            record: "acct1".to_string(),
        })
        .await
        .unwrap();
    harness
        .parsed_tx
        .send(CoordinatorMessage::AccountParsed {
            slot: 100,
            bank_id: 100,
            key: shipstern_block_coordinator::AccountRecordSortKey::new(2, [2; 32]),
            record: "acct2".to_string(),
        })
        .await
        .unwrap();

    let acct = harness.expect_account_flush(100).await;
    acct.records(&["acct1", "acct2"]);
}

/// Dead bank 42 of slot 42 has a parsed record. Repair bank 1042 freezes, then loses when
/// 43 skips 42. The dead bank's record must not block 43.
#[tokio::test]
async fn dead_bank_record_does_not_block_after_repair_loses() {
    let mut harness = TestHarness::spawn();
    let parent = harness.slot(41).parent(40).empty().await;
    parent.confirm().await;
    harness.expect_flush(41).await;

    send_to_bank(
        &harness,
        make_slot_update(42, 41, SlotStatus::SlotCreatedBank),
        42,
    )
    .await;
    send_record_to_slot(&harness.parsed_tx, 42, "dead").await;
    send_to_bank(&harness, make_slot_update(42, 41, SlotStatus::SlotDead), 42).await;
    for update in [
        make_slot_update(42, 41, SlotStatus::SlotCreatedBank),
        make_entry_update(42, 0, 0),
        make_block_meta_update(42, 41, 0, &block_hash(42)),
    ] {
        send_to_bank(&harness, update, 1042).await;
    }

    let child = harness.slot(43).parent(41).empty().await;
    child.confirm().await;

    harness.expect_flush(43).await;
}

/// When parent 29 dies, slot 30 is reported as forked. Its bank 3001 names parent 27 and
/// has not frozen yet, so it must still freeze, win and flush.
#[tokio::test]
async fn unfrozen_bank_of_forked_slot_can_still_win() {
    let mut harness = TestHarness::spawn();
    let root = harness.slot(27).parent(26).empty().await;
    root.confirm().await;
    harness.expect_flush(27).await;

    send_to_bank(
        &harness,
        make_slot_update(29, 27, SlotStatus::SlotCreatedBank),
        2900,
    )
    .await;
    for update in [
        make_slot_update(30, 29, SlotStatus::SlotCreatedBank),
        make_entry_update(30, 0, 0),
        make_block_meta_update(30, 29, 0, &block_hash(30)),
    ] {
        send_to_bank(&harness, update, 3000).await;
    }
    send_to_bank(
        &harness,
        make_slot_update(30, 27, SlotStatus::SlotCreatedBank),
        3001,
    )
    .await;
    for message in [
        CoordinatorMessage::InstructionParsed {
            slot: 30,
            bank_id: 3001,
            key: InstructionRecordSortKey::new(0, vec![0]),
            record: "repair".to_string(),
        },
        CoordinatorMessage::TransactionParsed {
            slot: 30,
            bank_id: 3001,
        },
    ] {
        harness.parsed_tx.send(message).await.unwrap();
    }
    send_to_bank(
        &harness,
        make_slot_update(29, 27, SlotStatus::SlotDead),
        2900,
    )
    .await;
    for update in [
        make_entry_update(30, 0, 1),
        make_block_meta_update(30, 27, 1, &block_hash(30)),
        make_slot_update(30, 27, SlotStatus::SlotConfirmed),
    ] {
        send_to_bank(&harness, update, 3001).await;
    }

    harness.expect_flush(30).await.records(&["repair"]);
}

/// Slot 30 is reported forked before its bank freezes, then dies. The block machine sends no
/// dead report after the fork report, so the frozen bank must not block slot 31.
#[tokio::test]
async fn bank_frozen_after_fork_report_does_not_block() {
    let mut harness = TestHarness::spawn();
    let root = harness.slot(27).parent(26).empty().await;
    root.confirm().await;
    harness.expect_flush(27).await;

    for (update, bank_id) in [
        (make_slot_update(29, 27, SlotStatus::SlotCreatedBank), 2900),
        (make_slot_update(30, 29, SlotStatus::SlotCreatedBank), 3000),
        (make_slot_update(30, 29, SlotStatus::SlotProcessed), 3000),
        (make_slot_update(29, 27, SlotStatus::SlotDead), 2900),
        (make_entry_update(30, 0, 0), 3000),
        (make_block_meta_update(30, 29, 0, &block_hash(30)), 3000),
        (make_slot_update(30, 29, SlotStatus::SlotDead), 3000),
    ] {
        send_to_bank(&harness, update, bank_id).await;
    }

    let next = harness.slot(31).parent(27).empty().await;
    next.confirm().await;
    harness.expect_flush(31).await;
}

/// A confirmed child's missing parent must stop the stream rather than be skipped.
#[tokio::test]
async fn lost_parent_block_meta_stops_before_child_flush() {
    let mut harness = TestHarness::spawn();
    let parent = harness.slot(41).parent(40).empty().await;
    parent.confirm().await;
    harness.expect_flush(41).await;

    send_to_bank(
        &harness,
        make_slot_update(42, 41, SlotStatus::SlotCreatedBank),
        42,
    )
    .await;
    let child = harness.slot(43).parent(42).empty().await;
    child.confirm().await;
    harness.expect_no_flush().await;

    for slot in 44..=43 + MISSING_PARENT_GRACE_SLOTS {
        harness.slot(slot).parent(slot - 1).empty().await;
    }
    harness
        .expect_unresolved(42, 43 + MISSING_PARENT_GRACE_SLOTS)
        .await;
    assert!(
        harness
            .input_tx
            .send(CoordinatorInput::GeyserUpdate(Box::new(
                make_block_meta_update(42, 41, 0, &block_hash(42),)
            )))
            .await
            .is_err(),
        "A parent arriving after the deadline must require replay, never a skipped child"
    );
}

/// Slot 5000 never gets its BlockMeta, and its confirmed child 5001 is the first bank after a
/// jump past the frozen window. Progress must still expose the unresolved parent as an error.
#[tokio::test]
async fn lost_parent_after_a_jump_stops_before_child_flush() {
    let mut harness = TestHarness::spawn();
    let first = harness.slot(41).parent(40).empty().await;
    first.confirm().await;
    harness.expect_flush(41).await;

    send_to_bank(
        &harness,
        make_slot_update(5000, 4999, SlotStatus::SlotCreatedBank),
        5000,
    )
    .await;
    let child = harness.slot(5001).parent(5000).empty().await;
    child.confirm().await;
    harness.expect_no_flush().await;

    for slot in 5002..=5001 + MISSING_PARENT_GRACE_SLOTS {
        harness.slot(slot).parent(slot - 1).empty().await;
    }
    harness
        .expect_unresolved(5000, 5001 + MISSING_PARENT_GRACE_SLOTS)
        .await;
}

/// Slot 42's bank 1 froze but lost. The confirmed bank 2 never gets its BlockMeta, so
/// the frozen loser must not hide the lost parent from its child 43.
#[tokio::test]
async fn lost_block_meta_of_winning_bank_stops_before_child_flush() {
    let mut harness = TestHarness::spawn();
    let parent = harness.slot(41).parent(40).empty().await;
    parent.confirm().await;
    harness.expect_flush(41).await;

    for update in [
        make_slot_update(42, 41, SlotStatus::SlotCreatedBank),
        make_entry_update(42, 0, 0),
        make_block_meta_update(42, 41, 0, &bank_block_hash(42, 1)),
    ] {
        send_to_bank(&harness, update, 1).await;
    }
    send_to_bank(
        &harness,
        make_slot_update(42, 41, SlotStatus::SlotCreatedBank),
        2,
    )
    .await;
    let child = harness.slot(43).parent(42).empty().await;
    child.confirm().await;
    send_to_bank(
        &harness,
        make_slot_update(42, 41, SlotStatus::SlotConfirmed),
        2,
    )
    .await;
    harness.expect_no_flush().await;

    for slot in 44..=43 + MISSING_PARENT_GRACE_SLOTS {
        harness.slot(slot).parent(slot - 1).empty().await;
    }
    harness
        .expect_unresolved(42, 43 + MISSING_PARENT_GRACE_SLOTS)
        .await;
}

/// Child 43's records wait while parent 42 has two candidate banks. The winning parent's
/// BlockMeta and confirm arrive inside grace, so its records must precede the child's.
#[tokio::test]
async fn late_parent_block_meta_within_grace_flushes() {
    let mut harness = TestHarness::spawn();
    let parent = harness.slot(41).parent(40).empty().await;
    parent.confirm().await;
    harness.expect_flush(41).await;

    for (bank_id, record) in [(4200, "loser"), (4201, "parent")] {
        for update in [
            make_slot_update(42, 41, SlotStatus::SlotCreatedBank),
            make_entry_update(42, 0, 1),
        ] {
            send_to_bank(&harness, update, bank_id).await;
        }
        for message in [
            CoordinatorMessage::InstructionParsed {
                slot: 42,
                bank_id,
                key: InstructionRecordSortKey::new(0, vec![0]),
                record: record.to_string(),
            },
            CoordinatorMessage::TransactionParsed { slot: 42, bank_id },
        ] {
            harness.parsed_tx.send(message).await.unwrap();
        }
    }
    let child = harness.slot(43).parent(42).record("child").parsed().await;
    child.confirm().await;
    for slot in 44..=50 {
        harness.slot(slot).parent(slot - 1).empty().await;
    }
    // Drain each ingress queue before the late metadata so the child is already confirmed
    // and all three banks' records have reached the coordinator.
    drop(
        harness
            .input_tx
            .reserve_many(harness.input_tx.max_capacity())
            .await
            .unwrap(),
    );
    drop(
        harness
            .parsed_tx
            .reserve_many(harness.parsed_tx.max_capacity())
            .await
            .unwrap(),
    );
    harness.expect_no_flush().await;

    send_to_bank(
        &harness,
        make_block_meta_update(42, 41, 1, &bank_block_hash(42, 4200)),
        4200,
    )
    .await;
    harness.expect_no_flush().await;
    for update in [
        make_block_meta_update(42, 41, 1, &block_hash(42)),
        make_slot_update(42, 41, SlotStatus::SlotConfirmed),
    ] {
        send_to_bank(&harness, update, 4201).await;
    }

    harness
        .expect_flush(42)
        .await
        .parent(41)
        .tx_count(1)
        .records(&["parent"]);
    harness
        .expect_flush(43)
        .await
        .parent(42)
        .tx_count(1)
        .records(&["child"]);
    harness.expect_no_flush().await;
}

#[tokio::test]
async fn malformed_block_meta_or_entry_is_skipped() {
    let mut harness = TestHarness::spawn();
    let mut bad_hash_meta = make_block_meta_update(50, 49, 0, &block_hash(50));
    let mut bad_hash_entry = make_entry_update(50, 0, 0);
    if let Some(UpdateOneof::BlockMeta(meta)) = bad_hash_meta.update_oneof.as_mut() {
        meta.blockhash = bs58::encode([1u8; 31]).into_string();
    }
    if let Some(UpdateOneof::Entry(entry)) = bad_hash_entry.update_oneof.as_mut() {
        entry.hash = vec![1; 31];
    }
    // A complete block whose parent is its own slot would block every later slot.
    let self_parent_meta = make_block_meta_update(50, 50, 0, &block_hash(50));
    for update in [
        bad_hash_entry,
        bad_hash_meta,
        make_entry_update(50, 0, 0),
        self_parent_meta,
    ] {
        harness
            .input_tx
            .send(CoordinatorInput::GeyserUpdate(Box::new(update)))
            .await
            .unwrap();
    }

    let recovered = harness.slot(50).parent(49).empty().await;
    recovered.confirm().await;
    harness.expect_flush(50).await.empty();

    let slot = harness.slot(51).parent(50).empty().await;
    slot.confirm().await;
    harness.expect_flush(51).await;
}

/// A server on Geyser proto 12.7 or older sends no bank ID, which decodes as 0.
#[tokio::test]
async fn missing_bank_id_stops_coordinator() {
    let (input_tx, input_rx) = mpsc::channel(8);
    let (_parsed_tx, parsed_rx) = mpsc::channel(8);
    let coordinator = tokio::spawn(BlockMachineCoordinator::<String>::run(
        input_rx,
        parsed_rx,
        None,
        None,
        AccountCommitAt::Confirmed,
        true,
    ));

    for slot in [100, 101] {
        let mut entry = make_entry_update(slot, 0, 0);
        if let Some(UpdateOneof::Entry(entry)) = entry.update_oneof.as_mut() {
            entry.bank_id = 0;
        }
        input_tx
            .send(CoordinatorInput::GeyserUpdate(Box::new(entry)))
            .await
            .unwrap();
    }

    let result = tokio::time::timeout(Duration::from_secs(2), coordinator)
        .await
        .expect("coordinator must stop")
        .expect("coordinator must not panic");
    assert!(matches!(
        result,
        Err(CoordinatorError::MissingBankId { slot: 101 })
    ));
}

#[tokio::test]
async fn bank_in_two_slots_stops_coordinator() {
    let (input_tx, input_rx) = mpsc::channel(8);
    let (_parsed_tx, parsed_rx) = mpsc::channel(8);
    let coordinator = tokio::spawn(BlockMachineCoordinator::<String>::run(
        input_rx,
        parsed_rx,
        None,
        None,
        AccountCommitAt::Confirmed,
        true,
    ));

    for update in [
        make_entry_update(100, 0, 0),
        make_block_meta_update(101, 100, 0, &block_hash(101)),
    ] {
        input_tx
            .send(CoordinatorInput::GeyserUpdate(Box::new(with_bank_id(
                update, 7,
            ))))
            .await
            .unwrap();
    }

    let result = tokio::time::timeout(Duration::from_secs(2), coordinator)
        .await
        .expect("coordinator must stop")
        .expect("coordinator must not panic");
    assert!(matches!(
        result,
        Err(CoordinatorError::BankInTwoSlots { bank_id: 7, .. })
    ));
}

/// A record arrives for slot 42, which the chain skips. Slot 43 must flush once the stream
/// is well past slot 42, not wait forever.
#[tokio::test]
async fn record_for_skipped_slot_does_not_block_forever() {
    let mut harness = TestHarness::spawn();
    let first = harness.slot(41).parent(40).empty().await;
    first.confirm().await;
    harness.expect_flush(41).await;

    send_record_to_slot(&harness.parsed_tx, 42, "skipped").await;
    let next = harness.slot(43).parent(41).empty().await;
    next.confirm().await;
    harness.expect_no_flush().await;

    for slot in 44..=43 + MISSING_PARENT_GRACE_SLOTS {
        harness.slot(slot).parent(slot - 1).empty().await;
    }
    harness.expect_flush(43).await;
}

/// Records come on their own channel and can lag. The record for skipped slot 42 arrives
/// after 43 froze, so no later parent edge skips 42.
#[tokio::test]
async fn late_record_for_skipped_slot_does_not_block_forever() {
    let mut harness = TestHarness::spawn();
    let first = harness.slot(41).parent(40).empty().await;
    first.confirm().await;
    harness.expect_flush(41).await;

    let next = harness.slot(43).parent(41).empty().await;
    tokio::time::sleep(Duration::from_millis(10)).await;
    send_record_to_slot(&harness.parsed_tx, 42, "skipped").await;
    next.confirm().await;
    harness.expect_no_flush().await;

    for slot in 44..=43 + MISSING_PARENT_GRACE_SLOTS {
        harness.slot(slot).parent(slot - 1).empty().await;
    }
    harness.expect_flush(43).await;
}

/// An early orphan record is not proven off-chain merely because no later parent edge reaches it.
#[tokio::test]
async fn record_below_first_parent_edge_requires_recovery() {
    let mut harness = TestHarness::spawn();
    send_record_to_slot(&harness.parsed_tx, 97, "early").await;
    let first = harness.slot(99).parent(98).empty().await;
    first.confirm().await;
    harness.expect_no_flush().await;

    let deadline = 97 + MISSING_PARENT_GRACE_SLOTS + 1;
    for slot in 100..=deadline {
        harness.slot(slot).parent(slot - 1).empty().await;
    }
    harness.expect_unresolved(97, deadline).await;
}

/// Replay all lifecycle updates on one channel so parent freeze versus child confirmation
/// has a fixed order. Neither candidate matches the child's expected parent hash, and no
/// direct commitment supplies the missing bank's identity.
async fn assert_unconfirmed_parent_stops_before_child_flush(
    late_parent_freeze: bool,
    repaired_root: bool,
    second_parent_freeze_slot: Option<u64>,
) {
    let (input_tx, input_rx) = mpsc::channel(2048);
    let (parsed_tx, parsed_rx) = mpsc::channel::<CoordinatorMessage<String>>(1);
    let (output_tx, mut output_rx) = mpsc::channel(2048);
    let (account_output_tx, mut account_output_rx) = mpsc::channel(2048);
    let coordinator = tokio::spawn(BlockMachineCoordinator::run(
        input_rx,
        parsed_rx,
        Some(output_tx),
        Some(account_output_tx),
        AccountCommitAt::Confirmed,
        true,
    ));
    let mut updates = Vec::new();
    if repaired_root {
        for update in [
            make_slot_update(42, 41, SlotStatus::SlotCreatedBank),
            make_slot_update(42, 41, SlotStatus::SlotDead),
        ] {
            updates.push(with_bank_id(update, 4200));
        }
    }
    for update in [
        make_slot_update(42, 41, SlotStatus::SlotCreatedBank),
        make_entry_update(42, 0, 0),
        make_block_meta_update(42, 41, 0, &block_hash(42)),
        make_slot_update(42, 41, SlotStatus::SlotConfirmed),
    ] {
        updates.push(with_bank_id(update, 4201));
    }
    for update in updates.drain(..) {
        input_tx
            .send(CoordinatorInput::GeyserUpdate(Box::new(update)))
            .await
            .unwrap();
    }
    let first = tokio::time::timeout(Duration::from_secs(2), output_rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first.slot, 42);

    for bank_id in [4300, 4301] {
        updates.push(with_bank_id(
            make_slot_update(43, 42, SlotStatus::SlotCreatedBank),
            bank_id,
        ));
    }
    updates.push(with_bank_id(make_entry_update(43, 0, 0), 4300));
    let parent_meta = with_bank_id(
        make_block_meta_update(43, 42, 0, &bank_block_hash(43, 4300)),
        4300,
    );
    if !late_parent_freeze {
        updates.push(parent_meta.clone());
    }
    for update in [
        make_entry_update(44, 0, 0),
        with_parent_blockhash(
            make_block_meta_update(44, 43, 0, &block_hash(44)),
            bank_block_hash(43, 4302),
        ),
        make_slot_update(44, 43, SlotStatus::SlotConfirmed),
    ] {
        updates.push(with_bank_id(update, 4400));
    }
    if late_parent_freeze {
        updates.push(parent_meta);
    }
    for update in updates {
        input_tx
            .send(CoordinatorInput::GeyserUpdate(Box::new(update)))
            .await
            .unwrap();
    }
    let deadline = 44 + MISSING_PARENT_GRACE_SLOTS + 1;
    for slot in 45..=deadline {
        if second_parent_freeze_slot == Some(slot) {
            for update in [
                make_entry_update(43, 0, 0),
                make_block_meta_update(43, 42, 0, &bank_block_hash(43, 4301)),
            ] {
                input_tx
                    .send(CoordinatorInput::GeyserUpdate(Box::new(with_bank_id(
                        update, 4301,
                    ))))
                    .await
                    .unwrap();
            }
        }
        for update in [
            make_entry_update(slot, 0, 0),
            make_block_meta_update(slot, slot - 1, 0, &block_hash(slot)),
        ] {
            input_tx
                .send(CoordinatorInput::GeyserUpdate(Box::new(with_bank_id(
                    update,
                    slot * 100,
                ))))
                .await
                .unwrap();
        }
        if slot < deadline {
            input_tx
                .send(CoordinatorInput::GeyserUpdate(Box::new(with_bank_id(
                    make_slot_update(slot, slot - 1, SlotStatus::SlotConfirmed),
                    slot * 100,
                ))))
                .await
                .unwrap();
        }
        if slot == deadline - 1 {
            drop(
                input_tx
                    .reserve_many(input_tx.max_capacity())
                    .await
                    .unwrap(),
            );
            assert!(
                output_rx.try_recv().is_err(),
                "Neither the child nor a later block may flush while its parent is unresolved"
            );
            while let Ok(output) = account_output_rx.try_recv() {
                assert!(
                    output.slot < 43,
                    "Account output must not cross the parent gap"
                );
            }
            assert!(
                !coordinator.is_finished(),
                "The original grace must remain intact"
            );
        }
    }
    let error = tokio::time::timeout(Duration::from_secs(2), coordinator)
        .await
        .expect("The original grace deadline must stop the coordinator")
        .expect("Coordinator must not panic")
        .expect_err("An unresolved canonical parent must require replay");
    assert!(
        matches!(
            error,
            CoordinatorError::UnresolvedBlock { slot: 43, observed_slot } if observed_slot == deadline
        ),
        "Unexpected gap error: {error}"
    );
    assert!(
        matches!(
            output_rx.try_recv(),
            Err(mpsc::error::TryRecvError::Disconnected)
        ),
        "The child and every later block must remain unpublished"
    );
    while let Ok(output) = account_output_rx.try_recv() {
        assert!(
            output.slot < 43,
            "Account output must not cross the parent gap"
        );
    }
    assert!(matches!(
        account_output_rx.try_recv(),
        Err(mpsc::error::TryRecvError::Disconnected)
    ));
    drop(input_tx);
    drop(parsed_tx);
}

/// A parent candidate freezing before child confirmation cannot be skipped at timeout.
#[tokio::test]
async fn frozen_parent_that_never_confirms_stops_before_child_flush() {
    assert_unconfirmed_parent_stops_before_child_flush(false, false, None).await;
}

/// Late metadata preserves the wait, but never permits dropping the unresolved parent.
#[tokio::test]
async fn late_frozen_parent_that_never_confirms_stops_before_child_flush() {
    assert_unconfirmed_parent_stops_before_child_flush(true, false, None).await;
}

/// A repaired-root fork report does not authorize skipping a confirmed child's parent.
#[tokio::test]
async fn late_forked_parent_that_never_confirms_stops_before_child_flush() {
    assert_unconfirmed_parent_stops_before_child_flush(true, true, None).await;
}

/// A second candidate cannot restart the deadline or authorize publishing across the gap.
#[tokio::test]
async fn second_parent_bank_freeze_keeps_original_grace_deadline() {
    assert_unconfirmed_parent_stops_before_child_flush(
        true,
        false,
        Some(44 + MISSING_PARENT_GRACE_SLOTS - 1),
    )
    .await;
}

/// The devnet Alpenglow switch: vote-only slots 100..=107 freeze on G = 99 and never confirm.
/// The first Alpenglow block 108 builds on G, and G gets Finalized with no Confirmed.
#[tokio::test]
async fn alpenglow_switch_skips_vote_only_slots() {
    let mut harness = TestHarness::spawn();
    let root = harness.slot(98).parent(97).empty().await;
    root.confirm().await;
    harness.expect_flush(98).await;

    let genesis = harness.slot(99).parent(98).record("tower").parsed().await;
    harness.slot(100).parent(99).record("vote").parsed().await;
    for slot in 101..=107 {
        harness.slot(slot).parent(slot - 1).empty().await;
    }
    let first = harness
        .slot(108)
        .parent(99)
        .record("alpenglow")
        .parsed()
        .await;
    first.confirm().await;
    genesis.finalize().await;
    first.finalize().await;

    harness.expect_flush(99).await.records(&["tower"]);
    harness.expect_flush(108).await.records(&["alpenglow"]);

    let next = harness.slot(109).parent(108).empty().await;
    next.confirm().await;
    harness.expect_flush(109).await;
}

/// Slot 101 has a record but has not frozen when bank 170 (parent 169) freezes. That edge does
/// not skip 101, so 101 must still freeze, confirm and flush.
#[tokio::test]
async fn far_bank_on_other_branch_keeps_slot_with_records() {
    let mut harness = TestHarness::spawn();
    let root = harness.slot(100).parent(99).empty().await;
    root.confirm().await;
    harness.expect_flush(100).await;

    send_record_to_slot(&harness.parsed_tx, 101, "late").await;
    harness.slot(170).parent(169).empty().await;

    let slot = harness.slot(101).parent(100).pending(1).await;
    slot.confirm().await;
    harness.expect_flush(101).await.records(&["late"]);
}

/// Bank 10 of slot 42 dies and repair bank 11 wins. The block machine then reports every
/// descendant of 42 as forked.
async fn flush_repaired_dead_slot_42(harness: &mut TestHarness) {
    let parent = harness.slot(41).parent(40).empty().await;
    parent.confirm().await;
    harness.expect_flush(41).await;

    send_to_bank(
        harness,
        make_slot_update(42, 41, SlotStatus::SlotCreatedBank),
        10,
    )
    .await;
    send_to_bank(harness, make_slot_update(42, 41, SlotStatus::SlotDead), 10).await;
    for update in [
        make_slot_update(42, 41, SlotStatus::SlotCreatedBank),
        make_entry_update(42, 0, 0),
        make_block_meta_update(42, 41, 0, &block_hash(42)),
        make_slot_update(42, 41, SlotStatus::SlotConfirmed),
    ] {
        send_to_bank(harness, update, 11).await;
    }
    harness.expect_flush(42).await;
}

/// A forked report before the confirm must not stop the confirmed child 43.
#[tokio::test]
async fn child_of_repaired_dead_slot_flushes() {
    let mut harness = TestHarness::spawn();
    flush_repaired_dead_slot_42(&mut harness).await;

    let child = harness.slot(43).parent(42).empty().await;
    child.confirm().await;
    harness.expect_flush(43).await;
}

/// With two frozen banks, slot 43 resolves only at its confirm, so the forked report for 43
/// comes after the confirm. The confirmed bank must still flush.
#[tokio::test]
async fn forked_report_after_confirm_keeps_confirmed_bank() {
    let mut harness = TestHarness::spawn();
    flush_repaired_dead_slot_42(&mut harness).await;

    for bank_id in [4300, 4301] {
        send_to_bank(
            &harness,
            make_slot_update(43, 42, SlotStatus::SlotCreatedBank),
            bank_id,
        )
        .await;
    }
    for bank_id in [4300, 4301] {
        send_to_bank(&harness, make_entry_update(43, 0, 0), bank_id).await;
        send_to_bank(
            &harness,
            make_block_meta_update(43, 42, 0, &block_hash(43)),
            bank_id,
        )
        .await;
    }
    send_to_bank(
        &harness,
        make_slot_update(43, 42, SlotStatus::SlotConfirmed),
        4301,
    )
    .await;

    let child = harness.slot(44).parent(43).empty().await;
    child.confirm().await;
    harness.expect_flush(43).await;
    harness.expect_flush(44).await;
}

/// Child 44 confirms before its parent 43 freezes. The block machine sends 43's fork report
/// with the late BlockMeta, so 43 must still flush when it confirms.
#[tokio::test]
async fn fork_report_after_child_confirm_keeps_parent() {
    let mut harness = TestHarness::spawn();
    flush_repaired_dead_slot_42(&mut harness).await;

    for update in [
        make_slot_update(43, 42, SlotStatus::SlotCreatedBank),
        make_entry_update(43, 0, 0),
    ] {
        send_to_bank(&harness, update, 4300).await;
    }
    let child = harness.slot(44).parent(43).empty().await;
    child.confirm().await;
    for update in [
        make_block_meta_update(43, 42, 0, &block_hash(43)),
        make_slot_update(43, 42, SlotStatus::SlotConfirmed),
    ] {
        send_to_bank(&harness, update, 4300).await;
    }

    harness.expect_flush(43).await;
    harness.expect_flush(44).await;
}

/// Slot 43 freezes with a fork report, and its child 44 confirms first. That is the order when
/// 43 gets no direct confirm and the server fills one in, so 43 must still flush.
#[tokio::test]
async fn child_confirm_before_forked_parent_keeps_parent() {
    let mut harness = TestHarness::spawn();
    flush_repaired_dead_slot_42(&mut harness).await;

    for update in [
        make_slot_update(43, 42, SlotStatus::SlotCreatedBank),
        make_entry_update(43, 0, 0),
        make_block_meta_update(43, 42, 0, &block_hash(43)),
    ] {
        send_to_bank(&harness, update, 4300).await;
    }
    let child = harness.slot(44).parent(43).empty().await;
    child.confirm().await;
    send_to_bank(
        &harness,
        make_slot_update(43, 42, SlotStatus::SlotConfirmed),
        4300,
    )
    .await;

    harness.expect_flush(43).await;
    harness.expect_flush(44).await;
}

#[tokio::test]
async fn cleared_banks_release_payloads_and_ignore_late_callbacks() {
    let mut harness = TestHarness::spawn();
    // Cleared candidate banks release records immediately. Their parser callbacks and
    // raw lifecycle messages can arrive after invalidation.
    for bank_id in 1000..1003 {
        send_to_bank(
            &harness,
            make_slot_update(42, 41, SlotStatus::SlotCreatedBank),
            bank_id,
        )
        .await;
        for record in ["before-clear", "late-callback"] {
            if record == "late-callback" {
                drop(
                    harness
                        .parsed_tx
                        .reserve_many(harness.parsed_tx.max_capacity())
                        .await
                        .unwrap(),
                );
                harness
                    .input_tx
                    .send(CoordinatorInput::GeyserUpdate(Box::new(SubscribeUpdate {
                        update_oneof: Some(UpdateOneof::EntryUpdateParent(
                            SubscribeUpdateEntryUpdateParent {
                                slot: 42,
                                cleared_bank_id: bank_id,
                                parent_slot: 40,
                                ..Default::default()
                            },
                        )),
                        ..Default::default()
                    })))
                    .await
                    .unwrap();
                drop(
                    harness
                        .input_tx
                        .reserve_many(harness.input_tx.max_capacity())
                        .await
                        .unwrap(),
                );
            }
            for message in [
                CoordinatorMessage::InstructionParsed {
                    slot: 42,
                    bank_id,
                    key: InstructionRecordSortKey::new(0, vec![0]),
                    record: record.to_string(),
                },
                CoordinatorMessage::AccountParsed {
                    slot: 42,
                    bank_id,
                    key: AccountRecordSortKey::new(1, [1; 32]),
                    record: record.to_string(),
                },
                CoordinatorMessage::TransactionParsed { slot: 42, bank_id },
            ] {
                harness.parsed_tx.send(message).await.unwrap();
            }
        }
        // Noncommitting lifecycle delivery can also lag the invalidation. It must
        // not revive A inside the block machine; contradictory commitment is tested separately.
        for update in [
            make_slot_update(42, 41, SlotStatus::SlotCreatedBank),
            make_entry_update(42, 0, 0),
            make_block_meta_update(42, 41, 0, &block_hash(42)),
            make_slot_update(42, 41, SlotStatus::SlotProcessed),
        ] {
            send_to_bank(&harness, update, bank_id).await;
        }
    }

    let replacement = harness.slot(42).parent(40).record("winner").parsed().await;
    replacement.confirm().await;
    harness.expect_flush(42).await.records(&["winner"]);
    harness.expect_account_flush(42).await.records(&[]);
    let child = harness.slot(43).parent(42).empty().await;
    child.confirm().await;
    harness.expect_flush(43).await.empty();
    harness.expect_account_flush(43).await.records(&[]);
    harness.expect_no_flush().await;
    harness.expect_no_account_flush().await;
}

/// An invalidated bank cannot later be declared canonical without an explicit error.
#[tokio::test]
async fn commitment_for_cleared_bank_requires_recovery() {
    for status in [SlotStatus::SlotConfirmed, SlotStatus::SlotFinalized] {
        let mut harness = TestHarness::spawn();
        send_to_bank(
            &harness,
            make_slot_update(42, 41, SlotStatus::SlotCreatedBank),
            1000,
        )
        .await;
        harness
            .input_tx
            .send(CoordinatorInput::GeyserUpdate(Box::new(SubscribeUpdate {
                update_oneof: Some(UpdateOneof::EntryUpdateParent(
                    SubscribeUpdateEntryUpdateParent {
                        slot: 42,
                        cleared_bank_id: 1000,
                        parent_slot: 40,
                        ..Default::default()
                    },
                )),
                ..Default::default()
            })))
            .await
            .unwrap();
        send_to_bank(&harness, make_slot_update(42, 41, status), 1000).await;
        harness.expect_unresolved(42, 42).await;
    }
}

/// A bank's hash is distinct from every other candidate, including another bank in its slot.
#[derive(Clone, Copy)]
struct HashLinkedBank {
    slot: u64,
    bank_id: u64,
    parent_slot: u64,
    blockhash: Hash,
    parent_blockhash: Hash,
}

impl HashLinkedBank {
    fn new(slot: u64, bank_id: u64, parent_slot: u64, parent_blockhash: Hash) -> Self {
        Self {
            slot,
            bank_id,
            parent_slot,
            blockhash: bank_block_hash(slot, bank_id),
            parent_blockhash,
        }
    }

    async fn create(self, harness: &TestHarness) {
        self.status(harness, SlotStatus::SlotCreatedBank).await;
    }

    async fn entry(self, harness: &TestHarness) {
        send_to_bank(harness, make_entry_update(self.slot, 0, 1), self.bank_id).await;
    }

    async fn metadata(self, harness: &TestHarness) {
        send_to_bank(
            harness,
            with_parent_blockhash(
                make_block_meta_update(self.slot, self.parent_slot, 1, &self.blockhash),
                self.parent_blockhash,
            ),
            self.bank_id,
        )
        .await;
    }

    async fn parsed(self, harness: &TestHarness, record: &str) {
        harness
            .input_tx
            .send(CoordinatorInput::AccountEventSeen {
                slot: self.slot,
                bank_id: self.bank_id,
            })
            .await
            .unwrap();
        for message in [
            CoordinatorMessage::InstructionParsed {
                slot: self.slot,
                bank_id: self.bank_id,
                key: InstructionRecordSortKey::new(0, vec![0]),
                record: record.to_string(),
            },
            CoordinatorMessage::AccountParsed {
                slot: self.slot,
                bank_id: self.bank_id,
                key: AccountRecordSortKey::new(1, [1; 32]),
                record: record.to_string(),
            },
            CoordinatorMessage::TransactionParsed {
                slot: self.slot,
                bank_id: self.bank_id,
            },
        ] {
            harness.parsed_tx.send(message).await.unwrap();
        }
    }

    async fn complete(self, harness: &TestHarness, record: &str) {
        self.create(harness).await;
        self.entry(harness).await;
        // Account ingress precedes BlockMeta in a complete bank. Its parser may still
        // finish later, but inherited commitment can freeze the count as soon as metadata
        // identifies this bank as the finalized child's parent.
        self.parsed(harness, record).await;
        self.metadata(harness).await;
    }

    async fn status(self, harness: &TestHarness, status: SlotStatus) {
        send_to_bank(
            harness,
            make_slot_update(self.slot, self.parent_slot, status),
            self.bank_id,
        )
        .await;
    }

    async fn expect_both_outputs(self, harness: &mut TestHarness, record: &str) {
        let output = harness.expect_flush(self.slot).await;
        assert_eq!(output.0.blockhash, self.blockhash);
        assert_eq!(output.0.parent_slot, self.parent_slot);
        output.tx_count(1).records(&[record]);
        let account_output = harness.expect_account_flush(self.slot).await;
        assert_eq!(account_output.0.bank_id, self.bank_id);
        assert_eq!(account_output.0.parent_slot, self.parent_slot);
        assert_eq!(account_output.0.blockhash, self.blockhash);
        account_output.records(&[record]);
    }
}

async fn finish_hash_linked_replay(harness: TestHarness) {
    let TestHarness {
        input_tx,
        parsed_tx,
        mut output_rx,
        mut account_output_rx,
        coordinator,
    } = harness;
    drop(input_tx);
    drop(parsed_tx);
    tokio::time::timeout(Duration::from_secs(2), coordinator)
        .await
        .expect("Coordinator did not finish after the complete chain")
        .expect("Coordinator panicked")
        .expect("A complete hash-linked chain must not require recovery");
    assert!(output_rx.recv().await.is_none());
    assert!(account_output_rx.recv().await.is_none());
}

#[derive(Clone, Copy)]
enum DelayedParentData {
    EntireBank,
    Metadata,
    Entries,
}

async fn assert_child_selects_hash_linked_parent(delayed: DelayedParentData) {
    let mut harness = TestHarness::spawn_with_options(true, AccountCommitAt::Finalized);
    let root = HashLinkedBank::new(41, 410, 40, block_hash(40));
    root.complete(&harness, "root").await;
    root.status(&harness, SlotStatus::SlotFinalized).await;
    root.expect_both_outputs(&mut harness, "root").await;

    let loser = HashLinkedBank::new(42, 420, 41, root.blockhash);
    let winner = HashLinkedBank::new(42, 421, 41, root.blockhash);
    let child = HashLinkedBank::new(43, 430, 42, winner.blockhash);
    assert_ne!(loser.blockhash, winner.blockhash);
    loser.complete(&harness, "losing-parent").await;
    match delayed {
        DelayedParentData::EntireBank => {},
        DelayedParentData::Metadata => {
            winner.create(&harness).await;
            winner.entry(&harness).await;
            winner.parsed(&harness, "winning-parent").await;
        },
        DelayedParentData::Entries => {
            winner.create(&harness).await;
            winner.metadata(&harness).await;
            winner.parsed(&harness, "winning-parent").await;
        },
    }
    child.complete(&harness, "child").await;
    child.status(&harness, SlotStatus::SlotFinalized).await;
    harness.expect_no_flush().await;
    harness.expect_no_account_flush().await;

    match delayed {
        DelayedParentData::EntireBank => winner.complete(&harness, "winning-parent").await,
        DelayedParentData::Metadata => winner.metadata(&harness).await,
        DelayedParentData::Entries => winner.entry(&harness).await,
    }
    // The child authenticates the parent by hash. No direct commitment for either parent
    // candidate has been sent, and the earlier losing candidate must never be published.
    winner
        .expect_both_outputs(&mut harness, "winning-parent")
        .await;
    child.expect_both_outputs(&mut harness, "child").await;

    // The wrapper must remain able to accept B's delayed own status instead of having
    // permanently finalized A when the child arrived.
    winner.status(&harness, SlotStatus::SlotConfirmed).await;
    child.status(&harness, SlotStatus::SlotFinalized).await;
    harness.expect_no_flush().await;
    harness.expect_no_account_flush().await;
    finish_hash_linked_replay(harness).await;
}

#[tokio::test]
async fn finalized_child_waits_for_parent_bank_named_by_hash() {
    assert_child_selects_hash_linked_parent(DelayedParentData::EntireBank).await;
}

#[tokio::test]
async fn finalized_child_selects_parent_after_delayed_winning_metadata() {
    assert_child_selects_hash_linked_parent(DelayedParentData::Metadata).await;
}

#[tokio::test]
async fn finalized_child_waits_for_hash_selected_parents_late_entries() {
    assert_child_selects_hash_linked_parent(DelayedParentData::Entries).await;
}

#[tokio::test]
async fn finalized_child_verifies_grandparent_before_promoting_sole_candidate() {
    let mut harness = TestHarness::spawn_with_options(true, AccountCommitAt::Finalized);
    let root = HashLinkedBank::new(40, 400, 39, block_hash(39));
    root.complete(&harness, "root").await;
    root.status(&harness, SlotStatus::SlotFinalized).await;
    root.expect_both_outputs(&mut harness, "root").await;

    let loser = HashLinkedBank::new(41, 410, 40, root.blockhash);
    let winner = HashLinkedBank::new(41, 411, 40, root.blockhash);
    let parent = HashLinkedBank::new(42, 420, 41, winner.blockhash);
    let child = HashLinkedBank::new(43, 430, 42, parent.blockhash);
    loser.complete(&harness, "losing-grandparent").await;
    parent.complete(&harness, "parent").await;
    child.complete(&harness, "child").await;
    child.status(&harness, SlotStatus::SlotFinalized).await;
    harness.expect_no_flush().await;
    harness.expect_no_account_flush().await;

    winner.complete(&harness, "winning-grandparent").await;
    winner
        .expect_both_outputs(&mut harness, "winning-grandparent")
        .await;
    parent.expect_both_outputs(&mut harness, "parent").await;
    child.expect_both_outputs(&mut harness, "child").await;
    finish_hash_linked_replay(harness).await;
}

#[tokio::test]
async fn finalized_child_selects_replacement_after_explicit_parent_clear() {
    let mut harness = TestHarness::spawn_with_options(true, AccountCommitAt::Finalized);
    let root = HashLinkedBank::new(40, 400, 39, block_hash(39));
    root.complete(&harness, "root").await;
    root.status(&harness, SlotStatus::SlotFinalized).await;
    root.expect_both_outputs(&mut harness, "root").await;

    let cleared = HashLinkedBank::new(42, 420, 41, block_hash(41));
    let winner = HashLinkedBank::new(42, 421, 40, root.blockhash);
    let child = HashLinkedBank::new(43, 430, 42, winner.blockhash);
    cleared.create(&harness).await;
    cleared.entry(&harness).await;
    cleared.parsed(&harness, "cleared-parent").await;
    harness
        .input_tx
        .send(CoordinatorInput::GeyserUpdate(Box::new(SubscribeUpdate {
            update_oneof: Some(UpdateOneof::EntryUpdateParent(
                SubscribeUpdateEntryUpdateParent {
                    slot: cleared.slot,
                    cleared_bank_id: cleared.bank_id,
                    parent_slot: winner.parent_slot,
                    parent_block_id: winner.parent_blockhash.to_bytes().to_vec(),
                },
            )),
            ..Default::default()
        })))
        .await
        .unwrap();
    winner.complete(&harness, "winning-parent").await;
    child.complete(&harness, "child").await;
    child.status(&harness, SlotStatus::SlotFinalized).await;
    // Late callbacks from the cleared bank cannot replace the verified winner's records.
    cleared.entry(&harness).await;
    cleared.parsed(&harness, "late-cleared-parent").await;

    winner
        .expect_both_outputs(&mut harness, "winning-parent")
        .await;
    child.expect_both_outputs(&mut harness, "child").await;
    finish_hash_linked_replay(harness).await;
}

#[tokio::test]
async fn child_with_conflicting_published_parent_hash_stops_before_output() {
    let mut harness = TestHarness::spawn_with_options(true, AccountCommitAt::Finalized);
    let parent = HashLinkedBank::new(42, 420, 41, block_hash(41));
    parent.complete(&harness, "published-parent").await;
    parent.status(&harness, SlotStatus::SlotFinalized).await;
    parent
        .expect_both_outputs(&mut harness, "published-parent")
        .await;

    let other_parent_hash = bank_block_hash(42, 421);
    assert_ne!(parent.blockhash, other_parent_hash);
    let child = HashLinkedBank::new(43, 430, 42, other_parent_hash);
    child.complete(&harness, "conflicting-child").await;
    child.status(&harness, SlotStatus::SlotFinalized).await;

    let TestHarness {
        input_tx,
        parsed_tx,
        mut output_rx,
        mut account_output_rx,
        coordinator,
    } = harness;
    drop(input_tx);
    drop(parsed_tx);
    let error = tokio::time::timeout(Duration::from_secs(2), coordinator)
        .await
        .expect("Contradictory parent identity must stop ingestion")
        .expect("Coordinator panicked")
        .expect_err("A child cannot extend a different block in the published parent slot");
    assert!(matches!(
        error,
        CoordinatorError::ConflictingBlockIdentity {
            slot: 43,
            bank_id: 430,
        }
    ));
    assert!(output_rx.recv().await.is_none());
    assert!(account_output_rx.recv().await.is_none());
}
