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
//! │    buffer: BTreeMap<(Slot, Bank), SlotBuffer> // pending banks              │
//! │    discarded_slots: BTreeSet<Slot>        // dead/forked/incomplete         │
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
//! │  │ slot in discarded? ├───────►│ DROP + WARN (non-canonical)     │          │
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
//! │  │ Buffer the message │        [parsed_messages_before_lifecycle]           │
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
//! │                                  │  flush_without  │  │ parent flushed │    │
//! │                                  │  _account_cnt]  │  │ or discarded?  │    │
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
//!   The implementation uses two independent drains. Instruction path applies
//!   gap/parent checks. Account path does not use parent-gap checks; it only
//!   requires account_gate_reached and ascending slot order.
//!   When `require_tx_gate=false`, `expected_tx_count` is forced to 0, so
//!   `tx_parse_complete` is immediately true once BlockFrozen is seen.
//!
//! ┌─────────────────────────────────────────────────────────────────────────────┐
//! │                           DISCARD SCENARIOS                                 │
//! ├─────────────────────────────────────────────────────────────────────────────┤
//! │                                                                             │
//! │  1. DEAD SLOT                          [dead_slot_discarded]                │
//! │     SlotLifecycle::Dead ──► ForksDetected ──► discard_slot()                │
//! │                                                                             │
//! │  2. FORKED SLOT                        [sibling_fork_via_finalized]         │
//! │     Sibling finalized ──► ForksDetected ──► discard_slot()                  │
//! │                                                                             │
//! │  3. INCOMPLETE BLOCK                   [incomplete_block_discarded]         │
//! │     BlockMeta with missing entries ──► BankDiscarded                        │
//! │                                                                             │
//! │  discard_slot() actions:                                                    │
//! │    • Add to discarded_slots set                                             │
//! │    • Remove from buffer                                                     │
//! │    • both drain_* functions called in run() after event                     │
//! │                                                                             │
//! └─────────────────────────────────────────────────────────────────────────────┘
//!
//! ┌─────────────────────────────────────────────────────────────────────────────┐
//! │                           TEST COVERAGE MATRIX                              │
//! ├─────────────────────────────────────────────────────────────────────────────┤
//! │                                                                             │
//! │  READINESS SYSTEM:                                                          │
//! │    ✓ two_gate_flush_end_to_end      instruction gate + account gate required │
//! │    ✓ empty_slot_flushes             tx_parse_complete satisfied with 0 tx   │
//! │    ✓ incomplete_slot_blocks_subsequent  tx_parse_complete not satisfied      │
//! │    ✓ account_gate_blocks_flush_until_account_count_received  Account gate blocks flush │
//! │                                                                             │
//! │  SEQUENTIAL ORDERING:                                                       │
//! │    ✓ sequential_flush_order         Earlier slot blocks later ones          │
//! │    ✓ gap_in_sequence_blocks_flush   Missing parent blocks child             │
//! │                                                                             │
//! │  DISCARD HANDLING:                                                          │
//! │    ✓ dead_slot_discarded            Dead slot removed, no output            │
//! │    ✓ dead_slot_discards_descendants Killing ancestor discards whole chain  │
//! │    ✓ dead_slot_unblocks_next        Dead sibling unblocks subsequent slot   │
//! │    ✓ incomplete_block_discarded     BlockMeta without entries is discarded  │
//! │    ✓ discarded_slot_ignores_parsed  Messages for discarded slot dropped     │
//! │                                                                             │
//! │  FORK HANDLING:                                                             │
//! │    ✓ sibling_fork_via_finalized     Finalizing one sibling forks other      │
//! │                                                                             │
//! │  EDGE CASES:                                                                │
//! │    ✓ parsed_messages_before_lifecycle  Early messages preserved             │
//! │    ✓ duplicate_confirm_does_not_change Confirming twice is safe             │
//! │                                                                             │
//! │  LATE POST-FLUSH MESSAGES:                                                  │
//! │    ✓ late_message_for_flushed_slot_is_dropped  Drops stale parsed events    │
//! │                                                                             │
//! └─────────────────────────────────────────────────────────────────────────────┘
//! ```
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
//! │  ForksDetected(A)                          ForksDetected(B)                 │
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
};
use solana_hash::Hash;
use tokio::sync::mpsc;
use yellowstone_grpc_proto::{
    geyser::{
        subscribe_update::UpdateOneof, SlotStatus, SubscribeUpdate, SubscribeUpdateBlockMeta,
        SubscribeUpdateEntry, SubscribeUpdateSlot,
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

        tokio::spawn(BlockMachineCoordinator::run(
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
        }
    }

    fn slot(&self, slot: u64) -> SlotBuilder {
        SlotBuilder::new(self.input_tx.clone(), self.parsed_tx.clone(), slot)
    }

    /// Create a bank and freeze it with no entries, so the coordinator discards it.
    async fn send_incomplete_bank(&self, slot: u64, parent: u64) {
        for update in [
            make_slot_update(slot, parent, SlotStatus::SlotCreatedBank),
            make_block_meta_update(slot, parent, 0, &Hash::new_unique()),
        ] {
            self.input_tx
                .send(CoordinatorInput::GeyserUpdate(Box::new(update)))
                .await
                .unwrap();
        }
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
            parent_blockhash: bs58::encode(Hash::default().as_ref()).into_string(),
            executed_transaction_count: tx_count,
            entries_count: 1,
            bank_id: slot,
        })),
    }
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

    for (bank_id, value, blockhash) in [
        (7, "loser", loser_blockhash),
        (8, "winner", Hash::new_unique()),
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
    assert_eq!(harness.expect_account_flush(slot).await.0.records, [
        "winner"
    ]);
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
        let blockhash = Hash::new_unique();

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

    let slot_101 = harness
        .slot(101)
        .parent(100)
        .record("rec-101")
        .parsed()
        .await;
    let slot_102 = harness
        .slot(102)
        .parent(101)
        .record("rec-102")
        .parsed()
        .await;

    slot_102.confirm().await;
    harness.expect_no_flush().await;

    slot_101.confirm().await;

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
async fn dead_slot_unblocks_next() {
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

    harness.expect_no_flush().await;

    // Confirming 102 first would make the block machine skip sibling 101 by itself.
    blocker.kill().await;
    waiting.confirm().await;

    harness.expect_flush(102).await.records(&["survives"]);
}

#[tokio::test]
async fn dead_slot_discards_descendants() {
    let mut harness = TestHarness::spawn();

    // Chain: 100 → 101 → 102. Killing 100 should discard 101 and 102 too.
    let ancestor = harness.slot(100).parent(99).record("root").parsed().await;
    let child = harness.slot(101).parent(100).record("child").parsed().await;
    let grandchild = harness
        .slot(102)
        .parent(101)
        .record("grandchild")
        .parsed()
        .await;

    child.confirm().await;
    grandchild.confirm().await;

    // Both descendants are ready, but killing the ancestor should discard the whole chain.
    ancestor.kill().await;

    harness.expect_no_flush().await;
}

#[tokio::test]
async fn empty_slot_flushes() {
    let mut harness = TestHarness::spawn();

    let slot = harness.slot(100).parent(99).empty().await;
    slot.confirm().await;

    harness.expect_flush(100).await.empty();
}

#[tokio::test]
async fn incomplete_block_discarded() {
    let mut harness = TestHarness::spawn();

    // Flush one slot first, so slot 101 cannot skip the parent check as the first flush.
    let first = harness.slot(99).parent(98).empty().await;
    first.confirm().await;
    harness.expect_flush(99).await.empty();

    harness.send_incomplete_bank(100, 99).await;
    send_record_to_slot(&harness.parsed_tx, 100, "ignored-incomplete").await;
    harness
        .input_tx
        .send(CoordinatorInput::GeyserUpdate(Box::new(make_slot_update(
            100,
            99,
            SlotStatus::SlotConfirmed,
        ))))
        .await
        .unwrap();
    harness.expect_no_flush().await;

    let next = harness.slot(101).parent(100).empty().await;
    next.confirm().await;

    harness.expect_flush(101).await.empty();
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
    let loser = harness.slot(102).parent(100).record("loser").parsed().await;

    winner.confirm().await;
    harness.expect_flush(100).await;
    harness.expect_flush(101).await.records(&["winner"]);

    winner.finalize().await;
    loser.confirm().await;

    harness.expect_no_flush().await;
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

/// Slot 42's bank 10 dies, then a repaired bank 11 of slot 42 freezes and is confirmed.
/// The dead bank must not block the repaired one.
#[tokio::test]
async fn repaired_bank_of_dead_slot_flushes() {
    let mut harness = TestHarness::spawn();
    let parent = harness.slot(41).parent(40).empty().await;
    parent.confirm().await;
    harness.expect_flush(41).await;

    send_to_bank(
        &harness,
        make_slot_update(42, 41, SlotStatus::SlotCreatedBank),
        10,
    )
    .await;
    send_to_bank(&harness, make_slot_update(42, 41, SlotStatus::SlotDead), 10).await;
    for update in [
        make_slot_update(42, 41, SlotStatus::SlotCreatedBank),
        make_entry_update(42, 0, 0),
        make_block_meta_update(42, 41, 0, &Hash::new_unique()),
        make_slot_update(42, 41, SlotStatus::SlotConfirmed),
    ] {
        send_to_bank(&harness, update, 11).await;
    }

    harness.expect_flush(42).await;
}

/// Slot 42 never gets its BlockMeta, so it never freezes. Its confirmed child 43 must
/// flush once the stream is well past slot 42, not wait forever.
#[tokio::test]
async fn lost_parent_block_meta_does_not_block_child_forever() {
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

    for slot in 44..=43 + 64 {
        harness.slot(slot).parent(slot - 1).empty().await;
    }
    harness.expect_flush(43).await;
}

/// A server can send a hash of the wrong length. The block machine's adapter panics on
/// one, so the coordinator must skip the update and keep running.
#[tokio::test]
async fn malformed_hash_is_skipped() {
    let mut harness = TestHarness::spawn();
    let mut meta = make_block_meta_update(50, 49, 0, &Hash::new_unique());
    let mut entry = make_entry_update(50, 0, 0);
    if let Some(UpdateOneof::BlockMeta(meta)) = meta.update_oneof.as_mut() {
        meta.blockhash = bs58::encode([1u8; 31]).into_string();
    }
    if let Some(UpdateOneof::Entry(entry)) = entry.update_oneof.as_mut() {
        entry.hash = vec![1; 31];
    }
    for update in [entry, meta] {
        harness
            .input_tx
            .send(CoordinatorInput::GeyserUpdate(Box::new(update)))
            .await
            .unwrap();
    }

    let slot = harness.slot(51).parent(50).empty().await;
    slot.confirm().await;
    harness.expect_flush(51).await;
}
