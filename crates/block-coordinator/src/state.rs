use std::collections::{BTreeMap, BTreeSet, HashMap};

use solana_clock::Slot;

use crate::{
    buffer::SlotRecordBuffer,
    types::{
        AccountCommitAt, AccountRecordSortKey, AccountSlot, BlockMetadata, CoordinatorError,
        DiscardReason, InstructionRecordSortKey, InstructionSlot, ParseStatsKind,
    },
};

/// All inputs to the coordinator state machine.
pub enum CoordinatorEvent<R> {
    /// Block metadata received (from FrozenBlock via BlockSM wrapper).
    BlockFrozen { slot: Slot, metadata: BlockMetadata },
    /// Slot confirmed by cluster consensus.
    SlotConfirmed { slot: Slot },
    /// Slot finalized by cluster consensus.
    SlotFinalized { slot: Slot },
    /// Slot discarded (dead, forked, or untracked).
    SlotDiscarded { slot: Slot, reason: DiscardReason },
    /// A raw Account event was seen on the geyser stream.
    /// Counted internally; frozen as expected_account_count when account gate is frozen.
    AccountEventSeen { slot: Slot },
    /// A parsed instruction record from a handler (sorted by key).
    InstructionRecordParsed {
        slot: Slot,
        key: InstructionRecordSortKey,
        record: R,
    },
    /// A parsed account record from a handler (sorted by write_version:pubkey).
    AccountRecordParsed {
        slot: Slot,
        key: AccountRecordSortKey,
        record: R,
    },
    /// A handler finished parsing a transaction.
    TransactionParsed { slot: Slot },
    /// A parse stat event (filtered or error).
    ParseStats { slot: Slot, kind: ParseStatsKind },
}

/// Pure-ish coordinator state (no channels, no wrapper).
pub struct CoordinatorState<R> {
    buffer: BTreeMap<Slot, SlotRecordBuffer<R>>,
    discarded_slots: BTreeSet<Slot>,
    last_instruction_flushed_slot: Option<Slot>,
    last_account_flushed_slot: Option<Slot>,
    /// Running count of AccountEventSeen signals per slot.
    /// Frozen as expected_account_count when the account gate is frozen.
    account_event_counts: HashMap<Slot, u64>,
    /// When accounts commit (Confirmed or Finalized).
    account_commit_at: AccountCommitAt,
    /// Post-flush account record/stat drops (observability).
    late_account_record_drops: u64,
    /// Post-freeze AccountEventSeen drops (observability).
    late_account_event_drops: u64,
}

impl<R> CoordinatorState<R> {
    const DISCARDED_SLOT_RETENTION_BEHIND_FLUSH: Slot = 512;
    /// arbitrary number
    const MAX_DISCARDED_SLOTS: usize = 100;

    pub fn new(account_commit_at: AccountCommitAt) -> Self {
        Self {
            buffer: BTreeMap::new(),
            discarded_slots: BTreeSet::new(),
            last_instruction_flushed_slot: None,
            last_account_flushed_slot: None,
            account_event_counts: HashMap::new(),
            account_commit_at,
            late_account_record_drops: 0,
            late_account_event_drops: 0,
        }
    }

    pub fn pending_slot_count(&self) -> usize { self.buffer.len() }

    pub fn discarded_slot_count(&self) -> usize { self.discarded_slots.len() }

    pub fn last_instruction_flushed_slot(&self) -> Option<Slot> {
        self.last_instruction_flushed_slot
    }

    pub fn last_account_flushed_slot(&self) -> Option<Slot> { self.last_account_flushed_slot }

    /// For backwards compatibility / observability: the minimum of the two flush slots.
    pub fn last_flushed_slot(&self) -> Option<Slot> {
        match (
            self.last_instruction_flushed_slot,
            self.last_account_flushed_slot,
        ) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    pub fn oldest_pending_slot(&self) -> Option<Slot> {
        self.buffer.first_key_value().map(|(&s, _)| s)
    }

    pub fn late_account_record_drops(&self) -> u64 { self.late_account_record_drops }

    pub fn late_account_event_drops(&self) -> u64 { self.late_account_event_drops }

    pub fn apply(&mut self, event: CoordinatorEvent<R>) -> Result<(), CoordinatorError> {
        match event {
            CoordinatorEvent::BlockFrozen { slot, metadata } => {
                if self.is_already_flushed(slot, "BlockFrozen") {
                    return Ok(());
                }
                let buf = self.buffer.entry(slot).or_default();
                buf.set_block_metadata(metadata);
            },
            CoordinatorEvent::SlotConfirmed { slot } => {
                if self.is_already_flushed(slot, "SlotConfirmed") {
                    return Ok(());
                }
                let buf = self.buffer.entry(slot).or_default();
                buf.mark_instruction_commitment_reached();
                if self.account_commit_at == AccountCommitAt::Confirmed {
                    self.freeze_account_gate(slot);
                }
            },
            CoordinatorEvent::SlotFinalized { slot } => {
                // Finalized is only relevant when account commit is configured at finalized.
                if self.account_commit_at != AccountCommitAt::Finalized {
                    return Ok(());
                }
                if self.is_already_flushed(slot, "SlotFinalized") {
                    return Ok(());
                }
                let buf = self.buffer.entry(slot).or_default();
                buf.mark_as_finalized();
                if self.account_commit_at == AccountCommitAt::Finalized {
                    self.freeze_account_gate(slot);
                }
            },
            CoordinatorEvent::SlotDiscarded { slot, reason } => {
                self.discard_slot(slot, reason);
            },
            CoordinatorEvent::AccountEventSeen { slot } => {
                if self.is_already_flushed(slot, "AccountEventSeen") {
                    return Ok(());
                }
                if self.discarded_slots.contains(&slot) {
                    self.log_discarded_slot_drop(slot, "AccountEventSeen");
                    return Ok(());
                }
                // If account gate is already frozen for this slot, warn + skip.
                if let Some(buf) = self.buffer.get(&slot)
                    && (buf.account_gate_reached() || self.is_account_gate_frozen(slot))
                {
                    tracing::warn!(
                        slot,
                        "AccountEventSeen after account gate frozen — dropping"
                    );
                    self.late_account_event_drops += 1;
                    return Ok(());
                }
                *self.account_event_counts.entry(slot).or_default() += 1;
            },
            CoordinatorEvent::InstructionRecordParsed { slot, key, record } => {
                if !self.validate_instruction_slot(slot, "InstructionRecordParsed") {
                    return Ok(());
                }
                self.buffer
                    .entry(slot)
                    .or_default()
                    .insert_instruction_record(key, record);
            },
            CoordinatorEvent::AccountRecordParsed { slot, key, record } => {
                if !self.validate_account_slot(slot, "AccountRecordParsed") {
                    return Ok(());
                }
                let buf = self.buffer.entry(slot).or_default();
                buf.insert_account_record(key, record);
                buf.increment_account_processed_count();
            },
            CoordinatorEvent::TransactionParsed { slot } => {
                if !self.validate_instruction_slot(slot, "TransactionParsed") {
                    return Ok(());
                }
                self.buffer
                    .entry(slot)
                    .or_default()
                    .increment_parsed_tx_count();
            },
            CoordinatorEvent::ParseStats { slot, kind } => {
                let is_account_stat = matches!(
                    kind,
                    ParseStatsKind::AccountFiltered | ParseStatsKind::AccountError
                );
                if is_account_stat {
                    if !self.validate_account_slot(slot, "ParseStats") {
                        return Ok(());
                    }
                    let buf = self.buffer.entry(slot).or_default();
                    buf.increment_parse_stat(kind);
                    buf.increment_account_processed_count();
                } else {
                    if !self.validate_instruction_slot(slot, "ParseStats") {
                        return Ok(());
                    }
                    self.buffer
                        .entry(slot)
                        .or_default()
                        .increment_parse_stat(kind);
                }
            },
        }
        Ok(())
    }

    /// Drain instruction-ready slots. Returns strictly ascending InstructionSlots.
    pub fn drain_instruction_flushable(
        &mut self,
    ) -> Result<Vec<InstructionSlot<R>>, CoordinatorError> {
        let mut flushed = Vec::new();

        let slots_to_check: Vec<Slot> = self.buffer.keys().copied().collect();
        for slot in slots_to_check {
            let buf = self.buffer.get(&slot).unwrap();
            if buf.instructions_drained() || !buf.instruction_gate_reached() {
                // Stop at the first non-ready slot to maintain ordering.
                if !buf.instructions_drained() {
                    break;
                }
                continue;
            }

            let parent_slot = buf
                .parent_slot()
                .ok_or(CoordinatorError::ReadySlotMissingMetadata { slot })?;

            let is_first = self.last_instruction_flushed_slot.is_none();
            let parent_ok = self
                .last_instruction_flushed_slot
                .is_some_and(|last| parent_slot <= last)
                || self.discarded_slots.contains(&parent_slot);

            if !is_first && !parent_ok {
                break;
            }

            let buf = self.buffer.get_mut(&slot).unwrap();
            let ix_slot = buf
                .drain_instruction_records(slot)
                .ok_or(CoordinatorError::ReadySlotMissingMetadata { slot })?;
            self.last_instruction_flushed_slot = Some(slot);
            flushed.push(ix_slot);
        }
        if !flushed.is_empty() {
            self.cleanup_fully_drained();
        }
        Ok(flushed)
    }

    /// Drain account-ready slots. Returns strictly ascending AccountSlots.
    pub fn drain_account_flushable(&mut self) -> Vec<AccountSlot<R>> {
        let mut flushed = Vec::new();

        let slots_to_check: Vec<Slot> = self.buffer.keys().copied().collect();
        for slot in slots_to_check {
            let buf = self.buffer.get(&slot).unwrap();
            if buf.accounts_drained() || !buf.account_gate_reached() {
                if !buf.accounts_drained() {
                    break;
                }
                continue;
            }

            let buf = self.buffer.get_mut(&slot).unwrap();
            let acct_slot = buf.drain_account_records(slot);
            self.last_account_flushed_slot = Some(slot);
            flushed.push(acct_slot);
        }
        if !flushed.is_empty() {
            self.cleanup_fully_drained();
        }
        flushed
    }

    /// Remove buffer entries where both instructions and accounts are drained.
    fn cleanup_fully_drained(&mut self) {
        let drained: Vec<Slot> = self
            .buffer
            .iter()
            .filter(|(_, buf)| buf.is_fully_drained())
            .map(|(&s, _)| s)
            .collect();
        for slot in drained {
            self.buffer.remove(&slot);
        }
        self.prune_discarded_slots();
        // Prune by the account frontier, not the instruction one: in finalized mode
        // instruction slots flush far earlier, and pruning by them drops valid counts.
        if let Some(last) = self.last_account_flushed_slot {
            self.account_event_counts.retain(|&s, _| s > last);
        }
    }

    /// Guard for BlockFrozen/SlotConfirmed/SlotFinalized — drop stale lifecycle events
    /// for slots that have already been fully flushed.
    fn is_already_flushed(&self, slot: Slot, event: &'static str) -> bool {
        let both_flushed = self
            .last_instruction_flushed_slot
            .is_some_and(|last| slot <= last)
            && self
                .last_account_flushed_slot
                .is_some_and(|last| slot <= last);
        if both_flushed {
            tracing::error!(
                slot,
                event,
                last_instruction_flushed = ?self.last_instruction_flushed_slot,
                last_account_flushed = ?self.last_account_flushed_slot,
                "Lifecycle event for already-flushed slot — investigate immediately"
            );
            return true;
        }
        false
    }

    /// Instruction events for slots at or behind the flush frontier are stale.
    fn validate_instruction_slot(&self, slot: Slot, event: &'static str) -> bool {
        if self.discarded_slots.contains(&slot) {
            self.log_discarded_slot_drop(slot, event);
            return false;
        }
        if self
            .last_instruction_flushed_slot
            .is_some_and(|last| slot <= last)
        {
            tracing::warn!(
                slot,
                event,
                last_instruction_flushed = ?self.last_instruction_flushed_slot,
                last_account_flushed = ?self.last_account_flushed_slot,
                oldest_pending_slot = ?self.oldest_pending_slot(),
                discarded_slot_count = self.discarded_slots.len(),
                known_discarded = self.discarded_slots.contains(&slot),
                "Instruction event for slot at or behind flush frontier; dropping"
            );
            return false;
        }
        true
    }

    /// Relaxed validation for account events — warn + drop if post-flush (not fatal).
    fn validate_account_slot(&mut self, slot: Slot, event: &'static str) -> bool {
        if self.discarded_slots.contains(&slot) {
            self.log_discarded_slot_drop(slot, event);
            return false;
        }
        if self
            .last_account_flushed_slot
            .is_some_and(|last| slot <= last)
        {
            tracing::warn!(
                slot,
                last_account_flushed = ?self.last_account_flushed_slot,
                "Late account event after flush — dropping"
            );
            self.late_account_record_drops += 1;
            return false;
        }
        // Also drop if accounts already drained for this specific slot.
        if let Some(buf) = self.buffer.get(&slot)
            && buf.accounts_drained()
        {
            tracing::warn!(slot, "Account event after accounts drained — dropping");
            self.late_account_record_drops += 1;
            return false;
        }
        true
    }

    fn log_discarded_slot_drop(&self, slot: Slot, event: &'static str) {
        tracing::warn!(
            slot,
            event,
            had_buffer = self.buffer.contains_key(&slot),
            buffered_record_count = self
                .buffer
                .get(&slot)
                .map_or(0, SlotRecordBuffer::record_count),
            last_instruction_flushed = ?self.last_instruction_flushed_slot,
            last_account_flushed = ?self.last_account_flushed_slot,
            oldest_pending_slot = ?self.oldest_pending_slot(),
            discarded_slot_count = self.discarded_slots.len(),
            "Dropping event for known discarded slot"
        );
    }

    /// Freeze the account gate for a slot: move account_event_counts into expected_account_count
    /// and mark account_commitment_reached.
    fn freeze_account_gate(&mut self, slot: Slot) {
        let count = self.account_event_counts.remove(&slot).unwrap_or(0);
        let buf = self.buffer.entry(slot).or_default();
        buf.set_expected_account_count(count);
        buf.mark_account_commitment_reached();
    }

    /// Check if the account gate has been frozen for a slot (expected_account_count is set).
    fn is_account_gate_frozen(&self, slot: Slot) -> bool {
        self.buffer
            .get(&slot)
            .is_some_and(|buf| buf.is_account_gate_frozen())
    }

    fn discard_slot(&mut self, slot: Slot, reason: DiscardReason) {
        let was_known_discarded = !self.discarded_slots.insert(slot);
        self.account_event_counts.remove(&slot);
        self.prune_discarded_slots();
        let buffer = self.buffer.remove(&slot);
        let record_count = buffer.as_ref().map_or(0, SlotRecordBuffer::record_count);
        tracing::warn!(
            slot,
            %reason,
            had_buffer = buffer.is_some(),
            record_count,
            was_known_discarded,
            last_instruction_flushed = ?self.last_instruction_flushed_slot,
            last_account_flushed = ?self.last_account_flushed_slot,
            "Discarding slot"
        );
    }

    /// Remove discarded-slot markers far behind `last_flushed_slot`, keeping a short
    /// window so late parse results for non-canonical slots are still recognized.
    /// Past `MAX_DISCARDED_SLOTS` it only warns, since evicting could stall the
    /// pipeline.
    fn prune_discarded_slots(&mut self) {
        if let Some(last) = self.last_flushed_slot() {
            let min_retained = last.saturating_sub(Self::DISCARDED_SLOT_RETENTION_BEHIND_FLUSH);
            self.discarded_slots.retain(|&s| s > min_retained);
        }
        if self.discarded_slots.len() > Self::MAX_DISCARDED_SLOTS {
            tracing::warn!(
                count = self.discarded_slots.len(),
                max = Self::MAX_DISCARDED_SLOTS,
                "Discarded slots exceed cap — not evicting to preserve flush correctness"
            );
        }
    }
}

impl<R> Default for CoordinatorState<R> {
    fn default() -> Self { Self::new(AccountCommitAt::Confirmed) }
}

#[cfg(test)]
mod tests {
    use solana_hash::Hash;

    use super::*;

    fn metadata(parent_slot: Slot, expected_tx_count: u64) -> BlockMetadata {
        BlockMetadata {
            parent_slot,
            blockhash: Hash::default(),
            expected_tx_count,
        }
    }

    /// Helper: make a slot fully ready for both instruction and account drains.
    fn apply_ready_slot(
        state: &mut CoordinatorState<String>,
        slot: Slot,
        parent: Slot,
        expected_tx_count: u64,
    ) {
        state
            .apply(CoordinatorEvent::BlockFrozen {
                slot,
                metadata: metadata(parent, expected_tx_count),
            })
            .unwrap();

        for _ in 0..expected_tx_count {
            state
                .apply(CoordinatorEvent::TransactionParsed { slot })
                .unwrap();
        }

        state
            .apply(CoordinatorEvent::SlotConfirmed { slot })
            .unwrap();
    }

    /// Drain both instruction and account for convenience in tests that don't
    /// care about the split. Returns instruction slots only.
    fn drain_both(
        state: &mut CoordinatorState<String>,
    ) -> Result<Vec<InstructionSlot<String>>, CoordinatorError> {
        let ix = state.drain_instruction_flushable()?;
        let _ = state.drain_account_flushable();
        Ok(ix)
    }

    #[test]
    fn monotonic_flush_order() {
        let mut state = CoordinatorState::<String>::default();

        apply_ready_slot(&mut state, 102, 101, 1);
        apply_ready_slot(&mut state, 100, 99, 1);
        apply_ready_slot(&mut state, 103, 102, 1);
        apply_ready_slot(&mut state, 101, 100, 1);

        let flushed = drain_both(&mut state).unwrap();
        let slots: Vec<_> = flushed.iter().map(|slot| slot.slot).collect();
        assert_eq!(slots, vec![100, 101, 102, 103]);
    }

    #[test]
    fn gap_invariant_holds() {
        let mut state = CoordinatorState::<String>::default();

        apply_ready_slot(&mut state, 100, 99, 1);
        apply_ready_slot(&mut state, 102, 101, 1);

        let flushed = drain_both(&mut state).unwrap();
        assert_eq!(flushed.len(), 1);
        assert_eq!(flushed[0].slot, 100);

        state
            .apply(CoordinatorEvent::SlotDiscarded {
                slot: 101,
                reason: DiscardReason::Dead,
            })
            .unwrap();

        let flushed = drain_both(&mut state).unwrap();
        assert_eq!(flushed.len(), 1);
        assert_eq!(flushed[0].slot, 102);
    }

    #[test]
    fn discard_unblocks_chain() {
        let mut state = CoordinatorState::<String>::default();

        apply_ready_slot(&mut state, 100, 99, 1);
        apply_ready_slot(&mut state, 101, 100, 1);
        apply_ready_slot(&mut state, 102, 101, 1);

        state
            .apply(CoordinatorEvent::SlotDiscarded {
                slot: 100,
                reason: DiscardReason::Dead,
            })
            .unwrap();

        let flushed = drain_both(&mut state).unwrap();
        let slots: Vec<_> = flushed.iter().map(|slot| slot.slot).collect();
        assert_eq!(slots, vec![101, 102]);
    }

    #[test]
    fn no_data_loss() {
        let mut state = CoordinatorState::<String>::default();
        let slot = 100;

        state
            .apply(CoordinatorEvent::BlockFrozen {
                slot,
                metadata: metadata(99, 2),
            })
            .unwrap();

        state
            .apply(CoordinatorEvent::InstructionRecordParsed {
                slot,
                key: InstructionRecordSortKey::new(1, vec![0]),
                record: "b".to_string(),
            })
            .unwrap();
        state
            .apply(CoordinatorEvent::InstructionRecordParsed {
                slot,
                key: InstructionRecordSortKey::new(0, vec![0]),
                record: "a".to_string(),
            })
            .unwrap();

        state
            .apply(CoordinatorEvent::TransactionParsed { slot })
            .unwrap();
        state
            .apply(CoordinatorEvent::TransactionParsed { slot })
            .unwrap();
        state
            .apply(CoordinatorEvent::SlotConfirmed { slot })
            .unwrap();

        let flushed = state.drain_instruction_flushable().unwrap();
        assert_eq!(flushed.len(), 1);
        assert_eq!(flushed[0].records, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn idempotent_confirm() {
        let mut state = CoordinatorState::<String>::default();
        apply_ready_slot(&mut state, 100, 99, 0);

        let flushed = drain_both(&mut state).unwrap();
        assert_eq!(flushed.len(), 1);
        assert_eq!(flushed[0].slot, 100);

        // Second confirm for an already-flushed slot is silently dropped.
        state
            .apply(CoordinatorEvent::SlotConfirmed { slot: 100 })
            .unwrap();

        let flushed = drain_both(&mut state).unwrap();
        assert!(flushed.is_empty());
        assert_eq!(state.pending_slot_count(), 0);
    }

    #[test]
    fn post_flush_instruction_events_without_discard_tombstone_are_dropped() {
        let mut state = CoordinatorState::<String>::default();
        apply_ready_slot(&mut state, 100, 99, 0);

        let _ = drain_both(&mut state).unwrap();

        state
            .apply(CoordinatorEvent::InstructionRecordParsed {
                slot: 100,
                key: InstructionRecordSortKey::new(0, vec![0]),
                record: "late".to_string(),
            })
            .unwrap();
        state
            .apply(CoordinatorEvent::TransactionParsed { slot: 100 })
            .unwrap();

        let flushed = drain_both(&mut state).unwrap();
        assert!(flushed.is_empty());
        assert_eq!(state.pending_slot_count(), 0);
    }

    #[test]
    fn eviction_does_not_block_children() {
        let mut state = CoordinatorState::<String>::default();

        apply_ready_slot(&mut state, 900, 899, 0);
        let flushed = drain_both(&mut state).unwrap();
        assert_eq!(flushed.len(), 1);
        assert_eq!(state.last_flushed_slot(), Some(900));

        for s in 1000..1200 {
            state
                .apply(CoordinatorEvent::SlotDiscarded {
                    slot: s,
                    reason: DiscardReason::Dead,
                })
                .unwrap();
        }

        apply_ready_slot(&mut state, 1200, 1000, 0);

        let flushed = drain_both(&mut state).unwrap();
        assert_eq!(flushed.len(), 1);
        assert_eq!(flushed[0].slot, 1200);
    }

    #[test]
    fn prune_removes_entries_far_below_last_flushed() {
        let mut state = CoordinatorState::<String>::default();

        for s in 50..60 {
            state
                .apply(CoordinatorEvent::SlotDiscarded {
                    slot: s,
                    reason: DiscardReason::Dead,
                })
                .unwrap();
        }

        apply_ready_slot(&mut state, 1000, 999, 0);
        let flushed = drain_both(&mut state).unwrap();
        assert_eq!(flushed.len(), 1);

        assert_eq!(state.discarded_slots.len(), 0);
    }

    #[test]
    fn prune_retains_recent_discarded_slots_below_last_flushed() {
        let mut state = CoordinatorState::<String>::default();

        state
            .apply(CoordinatorEvent::SlotDiscarded {
                slot: 96,
                reason: DiscardReason::Forked,
            })
            .unwrap();

        apply_ready_slot(&mut state, 100, 99, 0);
        let flushed = drain_both(&mut state).unwrap();
        assert_eq!(flushed.len(), 1);

        assert!(state.discarded_slots.contains(&96));
    }

    #[test]
    fn late_parsed_message_for_recent_discarded_slot_is_dropped_after_flush_frontier_advances() {
        let mut state = CoordinatorState::<String>::default();

        state
            .apply(CoordinatorEvent::SlotDiscarded {
                slot: 96,
                reason: DiscardReason::Forked,
            })
            .unwrap();

        apply_ready_slot(&mut state, 100, 99, 0);
        let flushed = drain_both(&mut state).unwrap();
        assert_eq!(flushed.len(), 1);

        state
            .apply(CoordinatorEvent::InstructionRecordParsed {
                slot: 96,
                key: InstructionRecordSortKey::new(0, vec![0]),
                record: "late fork".to_string(),
            })
            .unwrap();
        state
            .apply(CoordinatorEvent::TransactionParsed { slot: 96 })
            .unwrap();

        assert_eq!(state.pending_slot_count(), 0);
    }

    #[test]
    fn observability_accessors() {
        let mut state = CoordinatorState::<String>::default();

        assert_eq!(state.pending_slot_count(), 0);
        assert_eq!(state.discarded_slot_count(), 0);
        assert_eq!(state.last_flushed_slot(), None);
        assert_eq!(state.oldest_pending_slot(), None);

        apply_ready_slot(&mut state, 100, 99, 0);
        apply_ready_slot(&mut state, 102, 101, 0);
        assert_eq!(state.pending_slot_count(), 2);
        assert_eq!(state.oldest_pending_slot(), Some(100));

        state
            .apply(CoordinatorEvent::SlotDiscarded {
                slot: 50,
                reason: DiscardReason::Dead,
            })
            .unwrap();
        assert_eq!(state.discarded_slot_count(), 1);

        let flushed = drain_both(&mut state).unwrap();
        assert_eq!(flushed.len(), 1);
        assert_eq!(state.last_flushed_slot(), Some(100));
        assert_eq!(state.pending_slot_count(), 1);
        assert_eq!(state.oldest_pending_slot(), Some(102));
        assert_eq!(state.discarded_slot_count(), 1);
    }

    #[test]
    fn account_blocks_flush_until_all_processed() {
        let mut state = CoordinatorState::<String>::default();
        let slot = 100;

        state
            .apply(CoordinatorEvent::BlockFrozen {
                slot,
                metadata: metadata(99, 0),
            })
            .unwrap();
        state
            .apply(CoordinatorEvent::AccountEventSeen { slot })
            .unwrap();
        state
            .apply(CoordinatorEvent::AccountEventSeen { slot })
            .unwrap();
        state
            .apply(CoordinatorEvent::SlotConfirmed { slot })
            .unwrap();

        // Instruction flush ok, but accounts wait.
        let ix_flushed = state.drain_instruction_flushable().unwrap();
        assert_eq!(ix_flushed.len(), 1);
        let acct_flushed = state.drain_account_flushable();
        assert!(acct_flushed.is_empty());

        // Process 1 account → still blocked.
        state
            .apply(CoordinatorEvent::AccountRecordParsed {
                slot,
                key: AccountRecordSortKey::new(1, [1; 32]),
                record: "a".to_string(),
            })
            .unwrap();
        let acct_flushed = state.drain_account_flushable();
        assert!(acct_flushed.is_empty());

        // Process 2nd account → accounts flush.
        state
            .apply(CoordinatorEvent::AccountRecordParsed {
                slot,
                key: AccountRecordSortKey::new(2, [2; 32]),
                record: "b".to_string(),
            })
            .unwrap();
        let acct_flushed = state.drain_account_flushable();
        assert_eq!(acct_flushed.len(), 1);
        assert_eq!(acct_flushed[0].slot, slot);
    }

    #[test]
    fn independent_flush_instructions_at_confirmed_accounts_at_finalized() {
        let mut state = CoordinatorState::new(AccountCommitAt::Finalized);
        let slot = 100;

        state
            .apply(CoordinatorEvent::BlockFrozen {
                slot,
                metadata: metadata(99, 1),
            })
            .unwrap();
        state
            .apply(CoordinatorEvent::TransactionParsed { slot })
            .unwrap();
        state
            .apply(CoordinatorEvent::AccountEventSeen { slot })
            .unwrap();
        state
            .apply(CoordinatorEvent::AccountRecordParsed {
                slot,
                key: AccountRecordSortKey::new(1, [1; 32]),
                record: "acct".to_string(),
            })
            .unwrap();
        state
            .apply(CoordinatorEvent::SlotConfirmed { slot })
            .unwrap();

        // Instructions flush at confirmed.
        let ix_flushed = state.drain_instruction_flushable().unwrap();
        assert_eq!(ix_flushed.len(), 1);

        // Accounts NOT ready yet (waiting for finalized).
        let acct_flushed = state.drain_account_flushable();
        assert!(acct_flushed.is_empty());

        // Finalize → accounts flush.
        state
            .apply(CoordinatorEvent::SlotFinalized { slot })
            .unwrap();
        let acct_flushed = state.drain_account_flushable();
        assert_eq!(acct_flushed.len(), 1);
        assert_eq!(acct_flushed[0].records, vec!["acct".to_string()]);
    }

    #[test]
    fn finalized_event_is_ignored_when_account_commitment_is_confirmed() {
        let mut state = CoordinatorState::<String>::default();

        state
            .apply(CoordinatorEvent::SlotFinalized { slot: 123 })
            .unwrap();

        assert_eq!(state.pending_slot_count(), 0);
        assert_eq!(state.oldest_pending_slot(), None);
    }

    #[test]
    fn account_event_after_gate_frozen_is_warn_not_error() {
        let mut state = CoordinatorState::<String>::default();
        let slot = 100;

        state
            .apply(CoordinatorEvent::BlockFrozen {
                slot,
                metadata: metadata(99, 0),
            })
            .unwrap();
        state
            .apply(CoordinatorEvent::SlotConfirmed { slot })
            .unwrap();

        // Drain accounts (0 expected, 0 processed → immediately ready).
        let acct_flushed = state.drain_account_flushable();
        assert_eq!(acct_flushed.len(), 1);

        // Late AccountEventSeen → warn + drop (not error).
        state
            .apply(CoordinatorEvent::AccountEventSeen { slot })
            .unwrap();
        assert_eq!(state.late_account_event_drops(), 1);
    }

    #[test]
    fn account_event_after_gate_frozen_before_ready_is_dropped() {
        let mut state = CoordinatorState::<String>::default();
        let slot = 101;

        // Count one account event before freeze.
        state
            .apply(CoordinatorEvent::AccountEventSeen { slot })
            .unwrap();
        state
            .apply(CoordinatorEvent::BlockFrozen {
                slot,
                metadata: metadata(100, 0),
            })
            .unwrap();
        state
            .apply(CoordinatorEvent::SlotConfirmed { slot })
            .unwrap();

        // Gate is frozen with expected_account_count=1, but not yet ready.
        // Another AccountEventSeen must be treated as late and dropped.
        state
            .apply(CoordinatorEvent::AccountEventSeen { slot })
            .unwrap();
        assert_eq!(state.late_account_event_drops(), 1);

        // One processed account should still be enough to flush.
        state
            .apply(CoordinatorEvent::AccountRecordParsed {
                slot,
                key: AccountRecordSortKey::new(1, [1; 32]),
                record: "acct".to_string(),
            })
            .unwrap();
        let acct_flushed = state.drain_account_flushable();
        assert_eq!(acct_flushed.len(), 1);
        assert_eq!(acct_flushed[0].slot, slot);
    }

    #[test]
    fn account_event_counts_are_not_pruned_by_instruction_frontier() {
        let mut state = CoordinatorState::<String>::new(AccountCommitAt::Finalized);

        // Account event arrives for slot 100, but account gate won't freeze
        // until finalized.
        state
            .apply(CoordinatorEvent::AccountEventSeen { slot: 100 })
            .unwrap();

        // A later slot advances only the instruction frontier.
        state
            .apply(CoordinatorEvent::BlockFrozen {
                slot: 200,
                metadata: metadata(199, 0),
            })
            .unwrap();
        state
            .apply(CoordinatorEvent::SlotConfirmed { slot: 200 })
            .unwrap();
        let ix_flushed = state.drain_instruction_flushable().unwrap();
        assert_eq!(ix_flushed.len(), 1);
        assert_eq!(ix_flushed[0].slot, 200);

        // Now freeze account gate for slot 100 at finalized.
        state
            .apply(CoordinatorEvent::BlockFrozen {
                slot: 100,
                metadata: metadata(99, 0),
            })
            .unwrap();
        state
            .apply(CoordinatorEvent::SlotConfirmed { slot: 100 })
            .unwrap();
        state
            .apply(CoordinatorEvent::SlotFinalized { slot: 100 })
            .unwrap();

        // Should not flush yet: expected_account_count must still be 1.
        let acct_flushed = state.drain_account_flushable();
        assert!(acct_flushed.is_empty());

        state
            .apply(CoordinatorEvent::AccountRecordParsed {
                slot: 100,
                key: AccountRecordSortKey::new(1, [1; 32]),
                record: "acct".to_string(),
            })
            .unwrap();

        let acct_flushed = state.drain_account_flushable();
        assert_eq!(acct_flushed.len(), 1);
        assert_eq!(acct_flushed[0].slot, 100);
        assert_eq!(acct_flushed[0].records, vec!["acct".to_string()]);
    }

    #[test]
    fn late_account_record_after_flush_increments_counter() {
        let mut state = CoordinatorState::<String>::default();
        let slot = 100;

        apply_ready_slot(&mut state, slot, 99, 0);
        let _ = state.drain_instruction_flushable().unwrap();
        let _ = state.drain_account_flushable();

        // Late account record → warn + drop.
        state
            .apply(CoordinatorEvent::AccountRecordParsed {
                slot,
                key: AccountRecordSortKey::new(1, [1; 32]),
                record: "late".to_string(),
            })
            .unwrap();
        assert_eq!(state.late_account_record_drops(), 1);

        // Late AccountFiltered stat → warn + drop.
        state
            .apply(CoordinatorEvent::ParseStats {
                slot,
                kind: ParseStatsKind::AccountFiltered,
            })
            .unwrap();
        assert_eq!(state.late_account_record_drops(), 2);
    }

    #[test]
    fn drain_account_flushable_strictly_ascending() {
        let mut state = CoordinatorState::<String>::default();

        apply_ready_slot(&mut state, 102, 101, 0);
        apply_ready_slot(&mut state, 100, 99, 0);
        apply_ready_slot(&mut state, 101, 100, 0);

        // Drain instructions first to not block account ordering.
        let _ = state.drain_instruction_flushable().unwrap();

        let acct_flushed = state.drain_account_flushable();
        let slots: Vec<_> = acct_flushed.iter().map(|s| s.slot).collect();
        assert_eq!(slots, vec![100, 101, 102]);
    }

    #[test]
    fn account_gate_ready_with_all_readiness_checks() {
        let mut state = CoordinatorState::<String>::default();
        let slot = 100;

        state
            .apply(CoordinatorEvent::BlockFrozen {
                slot,
                metadata: metadata(99, 1),
            })
            .unwrap();
        state
            .apply(CoordinatorEvent::AccountRecordParsed {
                slot,
                key: AccountRecordSortKey::new(42, [1; 32]),
                record: "acct".to_string(),
            })
            .unwrap();
        state
            .apply(CoordinatorEvent::TransactionParsed { slot })
            .unwrap();
        state
            .apply(CoordinatorEvent::AccountEventSeen { slot })
            .unwrap();
        state
            .apply(CoordinatorEvent::SlotConfirmed { slot })
            .unwrap();

        let acct_flushed = state.drain_account_flushable();
        assert_eq!(acct_flushed.len(), 1);
        assert_eq!(acct_flushed[0].records, vec!["acct".to_string()]);
    }

    #[test]
    fn account_events_before_block_frozen_are_counted() {
        let mut state = CoordinatorState::<String>::default();
        let slot = 100;

        state
            .apply(CoordinatorEvent::AccountEventSeen { slot })
            .unwrap();
        assert!(state.account_event_counts.contains_key(&slot));

        state
            .apply(CoordinatorEvent::BlockFrozen {
                slot,
                metadata: metadata(99, 0),
            })
            .unwrap();
        assert!(state.account_event_counts.contains_key(&slot));

        state
            .apply(CoordinatorEvent::AccountRecordParsed {
                slot,
                key: AccountRecordSortKey::new(1, [1; 32]),
                record: "acct".to_string(),
            })
            .unwrap();

        state
            .apply(CoordinatorEvent::SlotConfirmed { slot })
            .unwrap();
        assert!(!state.account_event_counts.contains_key(&slot));

        let acct_flushed = state.drain_account_flushable();
        assert_eq!(acct_flushed.len(), 1);
    }

    #[test]
    fn account_event_for_never_frozen_slot_does_not_stall_flush() {
        let mut state = CoordinatorState::<String>::default();

        state
            .apply(CoordinatorEvent::AccountEventSeen { slot: 100 })
            .unwrap();

        apply_ready_slot(&mut state, 200, 199, 0);
        let flushed = drain_both(&mut state).unwrap();
        assert_eq!(flushed.len(), 1);
        assert_eq!(flushed[0].slot, 200);

        assert!(!state.account_event_counts.contains_key(&100));
    }

    #[test]
    fn malformed_pubkey_account_error_still_flushes() {
        let mut state = CoordinatorState::<String>::default();
        let slot = 100;

        state
            .apply(CoordinatorEvent::BlockFrozen {
                slot,
                metadata: metadata(99, 0),
            })
            .unwrap();
        state
            .apply(CoordinatorEvent::AccountEventSeen { slot })
            .unwrap();
        state
            .apply(CoordinatorEvent::ParseStats {
                slot,
                kind: ParseStatsKind::AccountError,
            })
            .unwrap();
        state
            .apply(CoordinatorEvent::SlotConfirmed { slot })
            .unwrap();

        let acct_flushed = state.drain_account_flushable();
        assert_eq!(acct_flushed.len(), 1);
        assert_eq!(acct_flushed[0].slot, slot);
        assert!(acct_flushed[0].records.is_empty());
    }

    #[test]
    fn same_pubkey_multiple_updates_preserves_write_version_order() {
        let mut state = CoordinatorState::<String>::default();
        let slot = 100;

        state
            .apply(CoordinatorEvent::BlockFrozen {
                slot,
                metadata: metadata(99, 0),
            })
            .unwrap();
        state
            .apply(CoordinatorEvent::AccountEventSeen { slot })
            .unwrap();
        state
            .apply(CoordinatorEvent::AccountEventSeen { slot })
            .unwrap();
        state
            .apply(CoordinatorEvent::AccountRecordParsed {
                slot,
                key: AccountRecordSortKey::new(5, [1; 32]),
                record: "first".to_string(),
            })
            .unwrap();
        state
            .apply(CoordinatorEvent::AccountRecordParsed {
                slot,
                key: AccountRecordSortKey::new(10, [1; 32]),
                record: "second".to_string(),
            })
            .unwrap();
        state
            .apply(CoordinatorEvent::SlotConfirmed { slot })
            .unwrap();

        let acct_flushed = state.drain_account_flushable();
        assert_eq!(acct_flushed.len(), 1);
        assert_eq!(acct_flushed[0].records, vec![
            "first".to_string(),
            "second".to_string()
        ]);
    }

    #[test]
    fn slot_removed_only_when_both_drained() {
        let mut state = CoordinatorState::<String>::default();
        apply_ready_slot(&mut state, 100, 99, 0);

        // Drain instructions only.
        let ix = state.drain_instruction_flushable().unwrap();
        assert_eq!(ix.len(), 1);
        // Buffer entry still present (accounts not drained).
        assert_eq!(state.pending_slot_count(), 1);

        // Drain accounts.
        let acct = state.drain_account_flushable();
        assert_eq!(acct.len(), 1);
        // Now buffer entry removed.
        assert_eq!(state.pending_slot_count(), 0);
    }
}
