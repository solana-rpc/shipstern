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
//!   The implementation uses two independent drains with the same parent
//!   check: a slot flushes after its parent flushed or was discarded
//!   incomplete. A child of a dead slot waits for the repair. Instructions
//!   need the tx gate, accounts the account gate.
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
//! │  3. INCOMPLETE BLOCK                   [incomplete_block_discarded]         │
//! │     BlockMeta with surplus entries ──► BankDiscarded                        │
//! │     BlockMeta with missing entries ──► wait for them (pending bank)         │
//! │     Startup, missing CreatedBank and entry 0 ──► BankDiscarded              │
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
    DEFAULT_MAX_PENDING_BANKS,
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
        Self::spawn_with_limit(
            require_tx_gate,
            account_commit_at,
            DEFAULT_MAX_PENDING_BANKS,
        )
    }

    fn spawn_with_max_pending_banks(max_pending_banks: usize) -> Self {
        Self::spawn_with_limit(true, AccountCommitAt::Confirmed, max_pending_banks)
    }

    fn spawn_with_limit(
        require_tx_gate: bool,
        account_commit_at: AccountCommitAt,
        max_pending_banks: usize,
    ) -> Self {
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
            max_pending_banks,
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

    /// The error the coordinator stopped with.
    async fn expect_stopped(self) -> CoordinatorError {
        tokio::time::timeout(Duration::from_secs(2), self.coordinator)
            .await
            .expect("coordinator must stop")
            .expect("coordinator must not panic")
            .expect_err("coordinator must stop with an error")
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
async fn incomplete_block_discarded() {
    let mut harness = TestHarness::spawn();

    // Flush one slot first, so slot 101 cannot skip the parent check as the first flush.
    let first = harness.slot(99).parent(98).empty().await;
    first.confirm().await;
    harness.expect_flush(99).await.empty();

    // Freeze slot 100 with two entries where its BlockMeta counts one. A surplus is final.
    for update in [
        make_slot_update(100, 99, SlotStatus::SlotCreatedBank),
        make_entry_update(100, 0, 0),
        make_entry_update(100, 1, 0),
        make_block_meta_update(100, 99, 0, &Hash::new_unique()),
    ] {
        send_to_bank(&harness, update, 100).await;
    }
    send_record_to_slot(&harness.parsed_tx, 100, "ignored-incomplete").await;
    send_to_bank(
        &harness,
        make_slot_update(100, 99, SlotStatus::SlotConfirmed),
        100,
    )
    .await;
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
        DEFAULT_MAX_PENDING_BANKS,
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
        DEFAULT_MAX_PENDING_BANKS,
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
        make_block_meta_update(42, 41, 0, &Hash::new_unique()),
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
        make_block_meta_update(30, 29, 0, &Hash::new_unique()),
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
        make_block_meta_update(30, 27, 1, &Hash::new_unique()),
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
        (make_block_meta_update(30, 29, 0, &Hash::new_unique()), 3000),
        (make_slot_update(30, 29, SlotStatus::SlotDead), 3000),
    ] {
        send_to_bank(&harness, update, bank_id).await;
    }

    let next = harness.slot(31).parent(27).empty().await;
    next.confirm().await;
    harness.expect_flush(31).await;
}

/// The pending-bank limit for tests where a slot never resolves.
const TEST_PENDING_LIMIT: usize = 16;

///
/// Freeze empty slots on a chain from `from` until the coordinator stops behind a slot that never
/// resolves, and return its error. Nothing flushes meanwhile, and sends that race with the exit
/// are ignored.
///
async fn freeze_until_stopped(mut harness: TestHarness, from: u64) -> CoordinatorError {
    for slot in from..from + 4 * TEST_PENDING_LIMIT as u64 {
        if harness.coordinator.is_finished() {
            break;
        }
        for update in [
            make_slot_update(slot, slot - 1, SlotStatus::SlotCreatedBank),
            make_entry_update(slot, 0, 0),
            make_block_meta_update(slot, slot - 1, 0, &Hash::new_unique()),
        ] {
            let _ = harness
                .input_tx
                .send(CoordinatorInput::GeyserUpdate(Box::new(update)))
                .await;
        }
        tokio::task::yield_now().await;
    }
    assert!(harness.output_rx.try_recv().is_err(), "Unexpected flush");
    harness.expect_stopped().await
}

fn assert_stopped_behind(error: CoordinatorError, slot: u64) {
    assert!(
        matches!(
            error,
            CoordinatorError::TooManyPendingBanks {
                limit: TEST_PENDING_LIMIT,
                oldest_slot: Some(oldest),
                ..
            } if oldest == slot
        ),
        "{error}"
    );
}

/// Slot 42 never gets its BlockMeta, so it never freezes. Its confirmed child 43 waits, and
/// the coordinator stops with an error once too many banks pend behind it.
#[tokio::test]
async fn lost_parent_block_meta_stops_coordinator() {
    let mut harness = TestHarness::spawn_with_max_pending_banks(TEST_PENDING_LIMIT);
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

    assert_stopped_behind(freeze_until_stopped(harness, 44).await, 43);
}

/// Slot 5000 never gets its BlockMeta, and its confirmed child 5001 is the first bank after a
/// jump. The jump changes nothing: the coordinator stops once too many banks pend.
#[tokio::test]
async fn lost_parent_after_a_jump_stops_coordinator() {
    let mut harness = TestHarness::spawn_with_max_pending_banks(TEST_PENDING_LIMIT);
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

    assert_stopped_behind(freeze_until_stopped(harness, 5002).await, 5001);
}

/// Slot 42's bank 1 froze but lost. The confirmed bank 2 never gets its BlockMeta, so its child
/// 43 waits; the loser is dropped once bank 2 confirms, and the coordinator stops behind 43.
#[tokio::test]
async fn lost_block_meta_of_winning_bank_stops_coordinator() {
    let mut harness = TestHarness::spawn_with_max_pending_banks(TEST_PENDING_LIMIT);
    let parent = harness.slot(41).parent(40).empty().await;
    parent.confirm().await;
    harness.expect_flush(41).await;

    for update in [
        make_slot_update(42, 41, SlotStatus::SlotCreatedBank),
        make_entry_update(42, 0, 0),
        make_block_meta_update(42, 41, 0, &Hash::new_unique()),
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

    assert_stopped_behind(freeze_until_stopped(harness, 44).await, 43);
}

/// Slot 42's BlockMeta arrives after its child 43 and 7 more slots froze. That is under the
/// pending limit, so 42 must flush first, then 43.
#[tokio::test]
async fn late_parent_block_meta_flushes() {
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
    for slot in 44..=50 {
        harness.slot(slot).parent(slot - 1).empty().await;
    }
    for update in [
        make_entry_update(42, 0, 0),
        make_block_meta_update(42, 41, 0, &Hash::new_unique()),
        make_slot_update(42, 41, SlotStatus::SlotConfirmed),
    ] {
        send_to_bank(&harness, update, 42).await;
    }

    harness.expect_flush(42).await;
    harness.expect_flush(43).await;
}

#[tokio::test]
async fn malformed_block_meta_or_entry_is_skipped() {
    let mut harness = TestHarness::spawn();
    let mut bad_hash_meta = make_block_meta_update(50, 49, 0, &Hash::new_unique());
    let mut bad_hash_entry = make_entry_update(50, 0, 0);
    if let Some(UpdateOneof::BlockMeta(meta)) = bad_hash_meta.update_oneof.as_mut() {
        meta.blockhash = bs58::encode([1u8; 31]).into_string();
    }
    if let Some(UpdateOneof::Entry(entry)) = bad_hash_entry.update_oneof.as_mut() {
        entry.hash = vec![1; 31];
    }
    // A complete block whose parent is its own slot would block every later slot.
    let self_parent_meta = make_block_meta_update(50, 50, 0, &Hash::new_unique());
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
        DEFAULT_MAX_PENDING_BANKS,
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
        DEFAULT_MAX_PENDING_BANKS,
    ));

    for update in [
        make_entry_update(100, 0, 0),
        make_block_meta_update(101, 100, 0, &Hash::new_unique()),
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

/// The chain skips slot 42, whose bank the block machine saw. It discards 42 when 43 confirms,
/// so 42's record does not hold 43 back.
#[tokio::test]
async fn record_for_a_seen_skipped_slot_does_not_block() {
    let mut harness = TestHarness::spawn();
    let first = harness.slot(41).parent(40).empty().await;
    first.confirm().await;
    harness.expect_flush(41).await;

    send_to_bank(
        &harness,
        make_slot_update(42, 41, SlotStatus::SlotCreatedBank),
        42,
    )
    .await;
    send_record_to_slot(&harness.parsed_tx, 42, "skipped").await;
    let next = harness.slot(43).parent(41).empty().await;
    next.confirm().await;
    harness.expect_flush(43).await;
}

/// A record arrives for slot 42, whose bank the block machine never saw, so nothing discards
/// it. Slot 43 waits, and the coordinator stops with an error once too many banks pend.
#[tokio::test]
async fn record_for_an_unseen_skipped_slot_stops_coordinator() {
    let mut harness = TestHarness::spawn_with_max_pending_banks(TEST_PENDING_LIMIT);
    let first = harness.slot(41).parent(40).empty().await;
    first.confirm().await;
    harness.expect_flush(41).await;

    send_record_to_slot(&harness.parsed_tx, 42, "skipped").await;
    let next = harness.slot(43).parent(41).empty().await;
    next.confirm().await;
    harness.expect_no_flush().await;

    assert_stopped_behind(freeze_until_stopped(harness, 44).await, 42);
}

/// Records come on their own channel and can lag. A record for unseen slot 42 arrives after 43
/// froze; the coordinator still stops with an error rather than waiting forever.
#[tokio::test]
async fn late_record_for_an_unseen_skipped_slot_stops_coordinator() {
    let mut harness = TestHarness::spawn_with_max_pending_banks(TEST_PENDING_LIMIT);
    let first = harness.slot(41).parent(40).empty().await;
    first.confirm().await;
    harness.expect_flush(41).await;

    let next = harness.slot(43).parent(41).empty().await;
    tokio::time::sleep(Duration::from_millis(10)).await;
    send_record_to_slot(&harness.parsed_tx, 42, "skipped").await;
    next.confirm().await;
    harness.expect_no_flush().await;

    assert_stopped_behind(freeze_until_stopped(harness, 44).await, 42);
}

/// At stream start a record lands for slot 97, whose bank the block machine never saw. The
/// coordinator stops with an error instead of holding back 99 forever.
#[tokio::test]
async fn record_below_first_parent_edge_stops_coordinator() {
    let mut harness = TestHarness::spawn_with_max_pending_banks(TEST_PENDING_LIMIT);
    send_record_to_slot(&harness.parsed_tx, 97, "early").await;
    let first = harness.slot(99).parent(98).empty().await;
    first.confirm().await;
    harness.expect_no_flush().await;

    assert_stopped_behind(freeze_until_stopped(harness, 100).await, 97);
}

/// Slot 43 froze with a bank that loses, and the winning bank never seals, so 43 never gets a
/// confirm. Its confirmed child 44 waits, and the coordinator stops behind 43.
#[tokio::test]
async fn frozen_parent_that_never_confirms_stops_coordinator() {
    let mut harness = TestHarness::spawn_with_max_pending_banks(TEST_PENDING_LIMIT);
    let first = harness.slot(42).parent(41).empty().await;
    first.confirm().await;
    harness.expect_flush(42).await;

    for update in [
        make_slot_update(43, 42, SlotStatus::SlotCreatedBank),
        make_entry_update(43, 0, 0),
        make_block_meta_update(43, 42, 0, &Hash::new_unique()),
    ] {
        send_to_bank(&harness, update, 4300).await;
    }
    send_to_bank(
        &harness,
        make_slot_update(43, 42, SlotStatus::SlotCreatedBank),
        4301,
    )
    .await;
    let child = harness.slot(44).parent(43).empty().await;
    child.confirm().await;
    harness.expect_no_flush().await;

    assert_stopped_behind(freeze_until_stopped(harness, 45).await, 43);
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
        make_block_meta_update(42, 41, 0, &Hash::new_unique()),
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
            make_block_meta_update(43, 42, 0, &Hash::new_unique()),
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
        make_block_meta_update(43, 42, 0, &Hash::new_unique()),
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
        make_block_meta_update(43, 42, 0, &Hash::new_unique()),
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
        // Raw lifecycle delivery can also lag the invalidation. It must not revive A
        // inside the block machine or let A's late commitment select the slot's winner.
        for update in [
            make_slot_update(42, 41, SlotStatus::SlotCreatedBank),
            make_entry_update(42, 0, 0),
            make_block_meta_update(42, 41, 0, &Hash::new_unique()),
            make_slot_update(42, 41, SlotStatus::SlotConfirmed),
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

///
/// Freeze slot `slot` (parent `slot - 1`) on BlockMeta with one of its two entries. Entry 0 holds
/// the slot's one transaction and entry 1 is a tick. LaserStream can send a bank's last entry
/// after its BlockMeta, and a stream that starts mid-slot misses the first ones.
///
async fn freeze_with_entry(harness: &TestHarness, slot: u64, index: u64) {
    let mut block_meta = make_block_meta_update(slot, slot - 1, 1, &Hash::new_unique());
    if let Some(UpdateOneof::BlockMeta(meta)) = block_meta.update_oneof.as_mut() {
        meta.entries_count = 2;
    }
    for update in [
        make_slot_update(slot, slot - 1, SlotStatus::SlotCreatedBank),
        make_entry_update(slot, index, u64::from(index == 0)),
        block_meta,
    ] {
        send_to_bank(harness, update, slot).await;
    }
}

async fn freeze_without_last_tick(harness: &TestHarness, slot: u64, record: &str) {
    freeze_with_entry(harness, slot, 0).await;
    send_record_to_slot(&harness.parsed_tx, slot, record).await;
}

async fn send_late_tick(harness: &TestHarness, slot: u64) {
    send_to_bank(harness, make_entry_update(slot, 1, 0), slot).await;
}

async fn confirm_slot(harness: &TestHarness, slot: u64) {
    send_to_bank(
        harness,
        make_slot_update(slot, slot - 1, SlotStatus::SlotConfirmed),
        slot,
    )
    .await;
}

/// Flush slot 99, so later slots cannot pass as the stream's first flush.
async fn flush_slot_99(harness: &mut TestHarness) {
    harness.slot(99).parent(98).empty().await.confirm().await;
    harness.expect_flush(99).await.empty();
}

/// The late tick completes slot 100 before its confirm, so the slot flushes in order.
#[tokio::test]
async fn entry_after_block_meta_flushes() {
    let mut harness = TestHarness::spawn();
    flush_slot_99(&mut harness).await;

    freeze_without_last_tick(&harness, 100, "a").await;
    send_late_tick(&harness, 100).await;
    confirm_slot(&harness, 100).await;
    harness.expect_flush(100).await.parent(99).records(&["a"]);

    harness.slot(101).parent(100).empty().await.confirm().await;
    harness.expect_flush(101).await.empty();
}

/// Slot 100 confirms before its late tick. The confirm waits for the tick.
#[tokio::test]
async fn confirm_before_late_entry_waits_for_it() {
    let mut harness = TestHarness::spawn();
    flush_slot_99(&mut harness).await;

    freeze_without_last_tick(&harness, 100, "a").await;
    confirm_slot(&harness, 100).await;
    harness.expect_no_flush().await;

    send_late_tick(&harness, 100).await;
    harness.expect_flush(100).await.records(&["a"]);
}

/// A later slot's confirm does not prove the tick is lost, so the child waits for it.
#[tokio::test]
async fn late_entry_after_the_child_confirms_still_flushes() {
    let mut harness = TestHarness::spawn();
    flush_slot_99(&mut harness).await;

    freeze_without_last_tick(&harness, 100, "a").await;
    confirm_slot(&harness, 100).await;
    harness.slot(101).parent(100).empty().await.confirm().await;
    harness.expect_no_flush().await;

    send_late_tick(&harness, 100).await;
    harness.expect_flush(100).await.records(&["a"]);
    harness.expect_flush(101).await.empty();
}

/// The tick of slot 100 never comes. The bank counts as pending even with no records, so the
/// coordinator stops behind it instead of skipping it.
#[tokio::test]
async fn entry_that_never_comes_stops_coordinator() {
    let mut harness = TestHarness::spawn_with_max_pending_banks(TEST_PENDING_LIMIT);
    flush_slot_99(&mut harness).await;

    freeze_with_entry(&harness, 100, 0).await;
    confirm_slot(&harness, 100).await;
    assert_stopped_behind(freeze_until_stopped(harness, 101).await, 100);
}

/// The stream started inside slot 100, so it missed its CreatedBank and entry 0. Nothing flushed
/// yet, so the slot is discarded and its child flushes.
#[tokio::test]
async fn first_block_of_a_stream_started_mid_slot_is_discarded() {
    let mut harness = TestHarness::spawn();

    let mut block_meta = make_block_meta_update(100, 99, 0, &Hash::new_unique());
    if let Some(UpdateOneof::BlockMeta(meta)) = block_meta.update_oneof.as_mut() {
        meta.entries_count = 2;
    }
    send_to_bank(&harness, make_entry_update(100, 1, 0), 100).await;
    send_to_bank(&harness, block_meta, 100).await;
    send_record_to_slot(&harness.parsed_tx, 100, "lost").await;
    confirm_slot(&harness, 100).await;

    harness.slot(101).parent(100).empty().await.confirm().await;
    harness.expect_flush(101).await.parent(100).empty();
    harness.expect_no_flush().await;
}

/// At startup the stream saw slot 100's CreatedBank, so its entries are coming even though
/// BlockMeta arrives before all of them.
#[tokio::test]
async fn block_meta_before_every_entry_at_startup_waits_for_them() {
    let mut harness = TestHarness::spawn();

    let mut block_meta = make_block_meta_update(100, 99, 1, &Hash::new_unique());
    if let Some(UpdateOneof::BlockMeta(meta)) = block_meta.update_oneof.as_mut() {
        meta.entries_count = 2;
    }
    for update in [
        make_slot_update(100, 99, SlotStatus::SlotCreatedBank),
        block_meta,
        make_slot_update(100, 99, SlotStatus::SlotConfirmed),
    ] {
        send_to_bank(&harness, update, 100).await;
    }
    send_record_to_slot(&harness.parsed_tx, 100, "a").await;
    harness.expect_no_flush().await;

    send_to_bank(&harness, make_entry_update(100, 0, 1), 100).await;
    send_late_tick(&harness, 100).await;
    harness.expect_flush(100).await.records(&["a"]);
}

/// Bank 10 of slot 100 dies, and repair bank 11 freezes short of its tick. The repair puts the
/// slot back on the chain, so child 101 waits for the tick instead of flushing past a dead slot.
#[tokio::test]
async fn late_entry_of_a_repair_bank_flushes_before_its_child() {
    let mut harness = TestHarness::spawn();
    flush_slot_99(&mut harness).await;

    send_to_bank(
        &harness,
        make_slot_update(100, 99, SlotStatus::SlotCreatedBank),
        10,
    )
    .await;
    send_to_bank(
        &harness,
        make_slot_update(100, 99, SlotStatus::SlotDead),
        10,
    )
    .await;

    let mut block_meta = make_block_meta_update(100, 99, 1, &Hash::new_unique());
    if let Some(UpdateOneof::BlockMeta(meta)) = block_meta.update_oneof.as_mut() {
        meta.entries_count = 2;
    }
    for update in [
        make_slot_update(100, 99, SlotStatus::SlotCreatedBank),
        make_entry_update(100, 0, 1),
        block_meta,
        make_slot_update(100, 99, SlotStatus::SlotConfirmed),
    ] {
        send_to_bank(&harness, update, 11).await;
    }
    for message in [
        CoordinatorMessage::InstructionParsed {
            slot: 100,
            bank_id: 11,
            key: InstructionRecordSortKey::new(0, vec![0]),
            record: "repair".to_string(),
        },
        CoordinatorMessage::TransactionParsed {
            slot: 100,
            bank_id: 11,
        },
    ] {
        harness.parsed_tx.send(message).await.unwrap();
    }
    harness.slot(101).parent(100).empty().await.confirm().await;
    harness.expect_no_flush().await;

    send_to_bank(&harness, make_entry_update(100, 1, 0), 11).await;
    harness.expect_flush(100).await.records(&["repair"]);
    harness.expect_flush(101).await.empty();
}

///
/// Slot 100 dies, repair bank 11 freezes short of its tick, then the slot dies again during
/// replay bank 12. Block machine 0.11 reports only the first death.
///
async fn kill_slot_100_twice(harness: &TestHarness) {
    let dead = || {
        let mut update = make_slot_update(100, 99, SlotStatus::SlotDead);
        if let Some(UpdateOneof::Slot(status)) = update.update_oneof.as_mut() {
            status.bank_id = None;
        }
        update
    };
    let mut block_meta = make_block_meta_update(100, 99, 0, &Hash::new_unique());
    if let Some(UpdateOneof::BlockMeta(meta)) = block_meta.update_oneof.as_mut() {
        meta.entries_count = 2;
    }
    send_to_bank(
        harness,
        make_slot_update(100, 99, SlotStatus::SlotCreatedBank),
        10,
    )
    .await;
    send_to_bank(harness, dead(), 10).await;
    for update in [
        make_slot_update(100, 99, SlotStatus::SlotCreatedBank),
        make_entry_update(100, 0, 0),
        block_meta,
    ] {
        send_to_bank(harness, update, 11).await;
    }
    send_to_bank(
        harness,
        make_slot_update(100, 99, SlotStatus::SlotCreatedBank),
        12,
    )
    .await;
    harness
        .input_tx
        .send(CoordinatorInput::GeyserUpdate(Box::new(dead())))
        .await
        .unwrap();
}

/// The coordinator drops the held repair bank 11 itself, so a successor that skips slot 100
/// flushes.
#[tokio::test]
async fn second_death_drops_the_held_repair_bank() {
    let mut harness = TestHarness::spawn();
    flush_slot_99(&mut harness).await;
    kill_slot_100_twice(&harness).await;

    harness.slot(101).parent(99).empty().await.confirm().await;
    harness.expect_flush(101).await.parent(99).empty();
}

/// After the second death, repair bank 13 confirms before its BlockMeta and its child confirms
/// first. Dropping bank 11 must not mark slot 100 dead again, or the child flushes past bank 13.
#[tokio::test]
async fn repair_after_a_second_death_flushes_before_its_child() {
    let mut harness = TestHarness::spawn();
    flush_slot_99(&mut harness).await;
    kill_slot_100_twice(&harness).await;

    for update in [
        make_slot_update(100, 99, SlotStatus::SlotCreatedBank),
        make_entry_update(100, 0, 0),
        make_slot_update(100, 99, SlotStatus::SlotConfirmed),
    ] {
        send_to_bank(&harness, update, 13).await;
    }
    harness.slot(101).parent(100).empty().await.confirm().await;
    harness.expect_no_flush().await;

    send_to_bank(
        &harness,
        make_block_meta_update(100, 99, 0, &Hash::new_unique()),
        13,
    )
    .await;
    harness.expect_flush(100).await.empty();
    harness.expect_flush(101).await.empty();
}

/// Slot 100 dies once, and repair bank 11 confirms before its BlockMeta, which comes after
/// child 101's. A node replays a child only on a frozen parent, so the child waits for the repair.
#[tokio::test]
async fn repair_block_meta_after_its_child_flushes_first() {
    let mut harness = TestHarness::spawn();
    flush_slot_99(&mut harness).await;

    send_to_bank(
        &harness,
        make_slot_update(100, 99, SlotStatus::SlotCreatedBank),
        10,
    )
    .await;
    send_to_bank(
        &harness,
        make_slot_update(100, 99, SlotStatus::SlotDead),
        10,
    )
    .await;
    for update in [
        make_slot_update(100, 99, SlotStatus::SlotCreatedBank),
        make_entry_update(100, 0, 0),
        make_slot_update(100, 99, SlotStatus::SlotConfirmed),
    ] {
        send_to_bank(&harness, update, 11).await;
    }
    harness.slot(101).parent(100).empty().await.confirm().await;
    harness.expect_no_flush().await;

    send_to_bank(
        &harness,
        make_block_meta_update(100, 99, 0, &Hash::new_unique()),
        11,
    )
    .await;
    harness.expect_flush(100).await.empty();
    harness.expect_flush(101).await.empty();
}

/// At startup, bank 1000 of slot 100 freezes on parent 98, which then dies, so slot 100 is
/// reported forked. Replacement bank 1001 on parent 97 freezes short of its tick and confirms,
/// and child 101 confirms first. The child waits for the tick, even as the stream's first flush.
#[tokio::test]
async fn short_replacement_of_a_forked_slot_flushes_before_its_child_at_startup() {
    let mut harness = TestHarness::spawn();

    send_to_bank(
        &harness,
        make_slot_update(98, 97, SlotStatus::SlotCreatedBank),
        98,
    )
    .await;
    for update in [
        make_slot_update(100, 98, SlotStatus::SlotCreatedBank),
        make_entry_update(100, 0, 0),
        make_block_meta_update(100, 98, 0, &Hash::new_unique()),
    ] {
        send_to_bank(&harness, update, 1000).await;
    }
    send_to_bank(&harness, make_slot_update(98, 97, SlotStatus::SlotDead), 98).await;

    let mut block_meta = make_block_meta_update(100, 97, 1, &Hash::new_unique());
    if let Some(UpdateOneof::BlockMeta(meta)) = block_meta.update_oneof.as_mut() {
        meta.entries_count = 2;
    }
    for update in [
        make_slot_update(100, 97, SlotStatus::SlotCreatedBank),
        make_entry_update(100, 0, 1),
        block_meta,
        make_slot_update(100, 97, SlotStatus::SlotConfirmed),
    ] {
        send_to_bank(&harness, update, 1001).await;
    }
    for message in [
        CoordinatorMessage::InstructionParsed {
            slot: 100,
            bank_id: 1001,
            key: InstructionRecordSortKey::new(0, vec![0]),
            record: "replacement".to_string(),
        },
        CoordinatorMessage::TransactionParsed {
            slot: 100,
            bank_id: 1001,
        },
    ] {
        harness.parsed_tx.send(message).await.unwrap();
    }
    harness.slot(101).parent(100).empty().await.confirm().await;
    harness.expect_no_flush().await;

    send_to_bank(&harness, make_entry_update(100, 1, 0), 1001).await;
    harness.expect_flush(100).await.records(&["replacement"]);
    harness.expect_flush(101).await.empty();
}

/// A late entry with more transactions than the BlockMeta counts can never match it.
#[tokio::test]
async fn late_entry_with_extra_transactions_discards_the_block() {
    let mut harness = TestHarness::spawn();
    flush_slot_99(&mut harness).await;

    freeze_without_last_tick(&harness, 100, "lost").await;
    send_to_bank(&harness, make_entry_update(100, 1, 1), 100).await;
    confirm_slot(&harness, 100).await;
    harness.expect_no_flush().await;

    harness.slot(101).parent(100).empty().await.confirm().await;
    harness.expect_flush(101).await.empty();
}

/// A bank that froze short of entries has frozen, so clearing it stops the coordinator.
#[tokio::test]
async fn update_parent_for_a_bank_waiting_for_entries_stops_coordinator() {
    let harness = TestHarness::spawn();
    freeze_without_last_tick(&harness, 100, "a").await;
    harness
        .input_tx
        .send(CoordinatorInput::GeyserUpdate(Box::new(SubscribeUpdate {
            update_oneof: Some(UpdateOneof::EntryUpdateParent(
                SubscribeUpdateEntryUpdateParent {
                    slot: 100,
                    cleared_bank_id: 100,
                    parent_slot: 98,
                    ..Default::default()
                },
            )),
            ..Default::default()
        })))
        .await
        .unwrap();
    assert!(matches!(
        harness.expect_stopped().await,
        CoordinatorError::ClearedCommittedBank {
            slot: 100,
            bank_id: 100
        }
    ));
}
