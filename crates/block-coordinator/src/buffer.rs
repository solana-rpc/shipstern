use std::collections::BTreeMap;

use solana_clock::Slot;

use crate::types::{
    AccountRecordSortKey, AccountSlot, BlockMetadata, InstructionRecordSortKey, InstructionSlot,
    ParseStatsKind,
};

/// Per-slot buffer of parsed records. Instructions flush once every transaction
/// is parsed and the slot is confirmed; accounts flush once every update is
/// parsed at the configured commitment. The slot is removed once both drain.
#[derive(Debug)]
pub struct SlotRecordBuffer<R> {
    /// Instruction records sorted by (tx_index, ix_path) for ordered flush.
    instruction_records: BTreeMap<InstructionRecordSortKey, R>,
    /// Account records sorted by (write_version, pubkey) for ordered flush.
    account_records: BTreeMap<AccountRecordSortKey, R>,
    /// Block metadata from FrozenBlock.
    metadata: Option<BlockMetadata>,
    /// Gate 1: fully parsed.
    parsed_tx_count: u64,
    /// Instruction commitment reached (confirmed by cluster consensus).
    instruction_commitment_reached: bool,
    /// Finalized by cluster consensus.
    finalized: bool,
    /// Account commitment reached (set at configured commitment level).
    account_commitment_reached: bool,
    /// Expected account count from source (None = not yet frozen, blocks account flush).
    expected_account_count: Option<u64>,
    /// Number of account events fully processed (records + filtered + failed).
    account_processed_count: u64,
    /// Whether instruction records have been drained from this buffer.
    instructions_drained: bool,
    /// Whether account records have been drained from this buffer.
    accounts_drained: bool,
    /// Parse stats counters.
    filtered_instruction_count: u64,
    failed_instruction_count: u64,
    filtered_account_count: u64,
    failed_account_count: u64,
    transaction_status_failed_count: u64,
    transaction_status_succeeded_count: u64,
}

impl<R> Default for SlotRecordBuffer<R> {
    fn default() -> Self {
        Self {
            instruction_records: BTreeMap::new(),
            account_records: BTreeMap::new(),
            metadata: None,
            parsed_tx_count: 0,
            instruction_commitment_reached: false,
            finalized: false,
            account_commitment_reached: false,
            expected_account_count: None,
            account_processed_count: 0,
            instructions_drained: false,
            accounts_drained: false,
            filtered_instruction_count: 0,
            failed_instruction_count: 0,
            filtered_account_count: 0,
            failed_account_count: 0,
            transaction_status_failed_count: 0,
            transaction_status_succeeded_count: 0,
        }
    }
}

impl<R> SlotRecordBuffer<R> {
    pub fn insert_instruction_record(&mut self, key: InstructionRecordSortKey, record: R) {
        if self.instruction_records.contains_key(&key) {
            tracing::warn!(
                ?key,
                "Duplicate instruction record sort key — previous record overwritten"
            );
        }
        self.instruction_records.insert(key, record);
    }

    pub fn insert_account_record(&mut self, key: AccountRecordSortKey, record: R) {
        if self.account_records.contains_key(&key) {
            tracing::warn!(
                ?key,
                "Duplicate AccountRecordSortKey — previous record overwritten"
            );
        }
        self.account_records.insert(key, record);
    }

    /// Set all block metadata from a FrozenBlock in one atomic operation.
    pub fn set_block_metadata(&mut self, metadata: BlockMetadata) {
        if self.parsed_tx_count > metadata.expected_tx_count {
            tracing::error!(
                parsed = self.parsed_tx_count,
                expected = metadata.expected_tx_count,
                "parsed_tx_count exceeds expected — possible handler bug"
            );
        }
        self.metadata = Some(metadata);
    }

    pub fn increment_parsed_tx_count(&mut self) {
        self.parsed_tx_count += 1;
        // INVARIANT: parsed_tx_count never exceeds expected_tx_count. If it does, a
        // handler sent duplicate signals or the expected count is wrong; the slot still
        // flushes (>=) so the pipeline doesn't stall, but records may be incomplete.
        if let Some(meta) = &self.metadata
            && self.parsed_tx_count > meta.expected_tx_count
        {
            tracing::error!(
                parsed = self.parsed_tx_count,
                expected = meta.expected_tx_count,
                "parsed_tx_count exceeds expected — investigate immediately"
            );
        }
    }

    pub fn increment_parse_stat(&mut self, kind: ParseStatsKind) {
        match kind {
            ParseStatsKind::InstructionFiltered => self.filtered_instruction_count += 1,
            ParseStatsKind::InstructionError => self.failed_instruction_count += 1,
            ParseStatsKind::AccountFiltered => self.filtered_account_count += 1,
            ParseStatsKind::AccountError => self.failed_account_count += 1,
            ParseStatsKind::TransactionStatusFailed => self.transaction_status_failed_count += 1,
            ParseStatsKind::TransactionStatusSucceeded => {
                self.transaction_status_succeeded_count += 1
            },
        }
    }

    pub fn increment_account_processed_count(&mut self) {
        self.account_processed_count += 1;
        if let Some(expected) = self.expected_account_count
            && self.account_processed_count > expected
        {
            tracing::error!(
                processed = self.account_processed_count,
                expected,
                "account_processed_count exceeds expected — investigate immediately"
            );
        }
    }

    pub fn mark_instruction_commitment_reached(&mut self) {
        self.instruction_commitment_reached = true;
    }

    pub fn mark_as_finalized(&mut self) { self.finalized = true; }

    pub fn mark_account_commitment_reached(&mut self) { self.account_commitment_reached = true; }

    pub fn instruction_commitment_reached(&self) -> bool { self.instruction_commitment_reached }

    pub fn account_commitment_reached(&self) -> bool { self.account_commitment_reached }

    pub fn is_finalized(&self) -> bool { self.finalized }

    /// Set the expected account count for the account-count gate. First-write-wins: if already
    /// set, subsequent calls are ignored (prevents duplicate Slot(Confirmed)
    /// from overwriting a non-zero count with 0).
    pub fn set_expected_account_count(&mut self, count: u64) {
        if self.expected_account_count.is_some() {
            tracing::warn!(
                existing = ?self.expected_account_count,
                new = count,
                "expected_account_count already set — ignoring duplicate"
            );
            return;
        }
        if self.account_processed_count() > count {
            tracing::error!(
                processed = self.account_processed_count(),
                expected = count,
                "account_processed_count exceeds expected — possible source/handler mismatch"
            );
        }
        self.expected_account_count = Some(count);
    }

    pub fn account_processed_count(&self) -> u64 { self.account_processed_count }

    pub fn tx_parse_complete(&self) -> bool {
        self.metadata
            .as_ref()
            .is_some_and(|meta| self.parsed_tx_count >= meta.expected_tx_count)
    }

    /// Account-count gate: all account updates processed.
    /// None = not yet received, blocks account readiness.
    pub fn account_parse_complete(&self) -> bool {
        self.expected_account_count
            .is_some_and(|n| self.account_processed_count() >= n)
    }

    /// Instruction gate: tx parse complete + instruction commitment reached.
    pub fn instruction_gate_reached(&self) -> bool {
        self.tx_parse_complete() && self.instruction_commitment_reached()
    }

    /// Account gate: account parse complete + account commitment reached.
    pub fn account_gate_reached(&self) -> bool {
        self.account_parse_complete() && self.account_commitment_reached()
    }

    /// True once the account gate has been frozen for this slot.
    pub fn is_account_gate_frozen(&self) -> bool {
        self.account_commitment_reached() && self.expected_account_count.is_some()
    }

    /// Legacy: all gates satisfied (both instruction and account ready).
    pub fn slot_ready(&self) -> bool {
        self.instruction_gate_reached() && self.account_gate_reached()
    }

    pub fn instructions_drained(&self) -> bool { self.instructions_drained }

    pub fn accounts_drained(&self) -> bool { self.accounts_drained }

    /// True when both instruction and account drains are complete.
    pub fn is_fully_drained(&self) -> bool { self.instructions_drained && self.accounts_drained }

    pub fn parent_slot(&self) -> Option<Slot> {
        self.metadata.as_ref().map(|meta| meta.parent_slot)
    }

    pub fn parsed_tx_count(&self) -> u64 { self.parsed_tx_count }

    pub fn record_count(&self) -> usize {
        self.instruction_records.len() + self.account_records.len()
    }

    /// Drain instruction records and produce an InstructionSlot.
    /// Returns None if metadata is missing.
    pub fn drain_instruction_records(&mut self, slot: Slot) -> Option<InstructionSlot<R>> {
        let metadata = self.metadata.as_ref()?;
        let result = InstructionSlot {
            slot,
            parent_slot: metadata.parent_slot,
            blockhash: metadata.blockhash,
            executed_transaction_count: metadata.expected_tx_count,
            records: std::mem::take(&mut self.instruction_records)
                .into_values()
                .collect(),
            filtered_instruction_count: self.filtered_instruction_count,
            failed_instruction_count: self.failed_instruction_count,
            transaction_status_failed_count: self.transaction_status_failed_count,
            transaction_status_succeeded_count: self.transaction_status_succeeded_count,
        };
        self.instructions_drained = true;
        Some(result)
    }

    /// Drain account records and produce an AccountSlot.
    pub fn drain_account_records(&mut self, slot: Slot) -> AccountSlot<R> {
        let decoded_account_count = self.account_records.len() as u64;
        let result = AccountSlot {
            slot,
            records: std::mem::take(&mut self.account_records)
                .into_values()
                .collect(),
            decoded_account_count,
            filtered_account_count: self.filtered_account_count,
            failed_account_count: self.failed_account_count,
        };
        self.accounts_drained = true;
        result
    }
}

#[cfg(test)]
mod tests {
    use solana_hash::Hash;

    use super::*;

    #[test]
    fn instruction_records_drain_in_sorted_order() {
        let mut buf = SlotRecordBuffer::<String>::default();
        buf.set_block_metadata(BlockMetadata {
            parent_slot: 0,
            blockhash: Hash::default(),
            expected_tx_count: 0,
        });

        // Insert out of order
        buf.insert_instruction_record(InstructionRecordSortKey::new(1, vec![0]), "tx1-ix0".into());
        buf.insert_instruction_record(
            InstructionRecordSortKey::new(0, vec![0, 1]),
            "tx0-ix0.1".into(),
        );
        buf.insert_instruction_record(InstructionRecordSortKey::new(0, vec![0]), "tx0-ix0".into());

        let ix_slot = buf.drain_instruction_records(42).expect("instruction slot");
        assert_eq!(ix_slot.records, vec![
            "tx0-ix0".to_string(),
            "tx0-ix0.1".to_string(),
            "tx1-ix0".to_string(),
        ]);
    }

    #[test]
    fn account_records_drain_in_sorted_order() {
        let mut buf = SlotRecordBuffer::<String>::default();

        // Insert out of order
        buf.insert_account_record(AccountRecordSortKey::new(300, [3; 32]), "wv300".into());
        buf.insert_account_record(AccountRecordSortKey::new(100, [1; 32]), "wv100".into());
        buf.insert_account_record(AccountRecordSortKey::new(200, [2; 32]), "wv200".into());

        let acct_slot = buf.drain_account_records(42);
        assert_eq!(acct_slot.records, vec![
            "wv100".to_string(),
            "wv200".to_string(),
            "wv300".to_string(),
        ]);
    }

    #[test]
    fn account_gate_not_ready_by_default() {
        let buf = SlotRecordBuffer::<String>::default();
        assert!(!buf.slot_ready());
    }

    #[test]
    fn account_gate_each_requirement_enforced() {
        // instruction gate check 1 + account parse check
        let mut buf = SlotRecordBuffer::<String>::default();
        buf.set_block_metadata(BlockMetadata {
            parent_slot: 0,
            blockhash: Hash::default(),
            expected_tx_count: 1,
        });
        buf.increment_parsed_tx_count();
        buf.set_expected_account_count(0);
        assert!(!buf.slot_ready()); // missing instruction_commitment_reached

        // instruction commitment + account parse, but tx parse is incomplete
        let mut buf2 = SlotRecordBuffer::<String>::default();
        buf2.mark_instruction_commitment_reached();
        buf2.set_expected_account_count(0);
        assert!(!buf2.slot_ready()); // missing tx_parse_complete

        // instruction gate complete, but account gate incomplete
        let mut buf3 = SlotRecordBuffer::<String>::default();
        buf3.set_block_metadata(BlockMetadata {
            parent_slot: 0,
            blockhash: Hash::default(),
            expected_tx_count: 1,
        });
        buf3.increment_parsed_tx_count();
        buf3.mark_instruction_commitment_reached();
        assert!(!buf3.slot_ready()); // missing account_parse_complete (expected_account_count is None)
    }

    #[test]
    fn account_gate_ready_when_all_requirements_met() {
        let mut buf = SlotRecordBuffer::<String>::default();
        buf.set_block_metadata(BlockMetadata {
            parent_slot: 0,
            blockhash: Hash::default(),
            expected_tx_count: 2,
        });
        buf.increment_parsed_tx_count();
        buf.increment_parsed_tx_count();
        buf.set_expected_account_count(0);
        buf.mark_instruction_commitment_reached();
        buf.mark_account_commitment_reached();
        assert!(buf.slot_ready());
    }

    #[test]
    fn ix_path_depth_first_ordering() {
        let mut buf = SlotRecordBuffer::<String>::default();
        buf.set_block_metadata(BlockMetadata {
            parent_slot: 0,
            blockhash: Hash::default(),
            expected_tx_count: 0,
        });
        // Simulate: tx0 has main ix [0] with two CPIs [0,0] and [0,1]
        // And [0,0] has a nested CPI [0,0,0]
        buf.insert_instruction_record(InstructionRecordSortKey::new(0, vec![0, 1]), "cpi-1".into());
        buf.insert_instruction_record(
            InstructionRecordSortKey::new(0, vec![0, 0, 0]),
            "nested-cpi".into(),
        );
        buf.insert_instruction_record(InstructionRecordSortKey::new(0, vec![0]), "main".into());
        buf.insert_instruction_record(InstructionRecordSortKey::new(0, vec![0, 0]), "cpi-0".into());

        let ix_slot = buf.drain_instruction_records(42).expect("instruction slot");
        assert_eq!(ix_slot.records, vec![
            "main".to_string(),
            "cpi-0".to_string(),
            "nested-cpi".to_string(),
            "cpi-1".to_string(),
        ]);
    }

    #[test]
    fn drain_empties_buffer() {
        let mut buf = SlotRecordBuffer::<String>::default();
        buf.insert_instruction_record(InstructionRecordSortKey::new(0, vec![0]), "record".into());
        assert_eq!(buf.record_count(), 1);

        buf.set_block_metadata(BlockMetadata {
            parent_slot: 0,
            blockhash: Hash::default(),
            expected_tx_count: 0,
        });
        let ix_slot = buf.drain_instruction_records(42).expect("instruction slot");
        assert_eq!(ix_slot.records.len(), 1);
        assert_eq!(buf.record_count(), 0);
    }

    #[test]
    fn parsed_tx_overshoot_still_ready() {
        let mut buf = SlotRecordBuffer::<String>::default();
        buf.set_block_metadata(BlockMetadata {
            parent_slot: 0,
            blockhash: Hash::default(),
            expected_tx_count: 1,
        });
        buf.increment_parsed_tx_count();
        buf.increment_parsed_tx_count(); // overshoot
        buf.set_expected_account_count(0);
        buf.mark_instruction_commitment_reached();
        buf.mark_account_commitment_reached();

        // Overshoot doesn't prevent readiness — the slot still flushes.
        // (The tracing::error fires at runtime to flag the handler bug.)
        assert!(buf.slot_ready());
        assert_eq!(buf.parsed_tx_count(), 2);
    }

    #[test]
    fn set_block_metadata_overwrites() {
        let mut buf = SlotRecordBuffer::<String>::default();
        buf.set_block_metadata(BlockMetadata {
            parent_slot: 10,
            blockhash: Hash::default(),
            expected_tx_count: 5,
        });
        assert_eq!(buf.parent_slot(), Some(10));

        // Second call overwrites — last metadata wins.
        let new_hash = Hash::new_unique();
        buf.set_block_metadata(BlockMetadata {
            parent_slot: 20,
            blockhash: new_hash,
            expected_tx_count: 3,
        });
        assert_eq!(buf.parent_slot(), Some(20));

        buf.increment_parsed_tx_count();
        buf.increment_parsed_tx_count();
        buf.increment_parsed_tx_count();
        buf.set_expected_account_count(0);
        buf.mark_instruction_commitment_reached();
        buf.mark_account_commitment_reached();
        assert!(buf.slot_ready());

        let ix_slot = buf.drain_instruction_records(42).expect("instruction slot");
        assert_eq!(ix_slot.parent_slot, 20);
        assert_eq!(ix_slot.blockhash, new_hash);
        assert_eq!(ix_slot.executed_transaction_count, 3);
    }

    #[test]
    fn account_count_gate_none_blocks_readiness() {
        let mut buf = SlotRecordBuffer::<String>::default();
        buf.set_block_metadata(BlockMetadata {
            parent_slot: 0,
            blockhash: Hash::default(),
            expected_tx_count: 0,
        });
        buf.mark_instruction_commitment_reached();
        // Instruction gate satisfied, but account_parse_complete is false (None) -> not ready.
        assert!(!buf.account_parse_complete());
        assert!(!buf.slot_ready());
    }

    #[test]
    fn account_count_gate_zero_expected_passes() {
        let mut buf = SlotRecordBuffer::<String>::default();
        buf.set_expected_account_count(0);
        // No accounts expected, none processed → account-count gate satisfied.
        assert!(buf.account_parse_complete());
    }

    #[test]
    fn account_count_gate_mixed_account_processing() {
        let mut buf = SlotRecordBuffer::<String>::default();
        buf.set_expected_account_count(3);
        assert!(!buf.account_parse_complete());

        // 1 successful record
        buf.insert_account_record(AccountRecordSortKey::new(100, [1; 32]), "acct1".into());
        buf.increment_account_processed_count();
        assert_eq!(buf.account_processed_count(), 1);
        assert!(!buf.account_parse_complete());

        // 1 filtered
        buf.increment_parse_stat(ParseStatsKind::AccountFiltered);
        buf.increment_account_processed_count();
        assert_eq!(buf.account_processed_count(), 2);
        assert!(!buf.account_parse_complete());

        // 1 error
        buf.increment_parse_stat(ParseStatsKind::AccountError);
        buf.increment_account_processed_count();
        assert_eq!(buf.account_processed_count(), 3);
        assert!(buf.account_parse_complete());
    }

    #[test]
    fn account_count_gate_first_write_wins() {
        let mut buf = SlotRecordBuffer::<String>::default();
        buf.set_expected_account_count(5);
        // Second call is ignored (first-write-wins).
        buf.set_expected_account_count(0);
        // Still expects 5, not 0.
        assert!(!buf.account_parse_complete());
        for i in 0..5 {
            buf.insert_account_record(
                AccountRecordSortKey::new(i, [i as u8; 32]),
                format!("acct{i}"),
            );
            buf.increment_account_processed_count();
        }
        assert!(buf.account_parse_complete());
    }
}
