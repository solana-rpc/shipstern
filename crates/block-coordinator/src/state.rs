use std::collections::{BTreeMap, BTreeSet};

use solana_clock::Slot;

use crate::{
    buffer::SlotRecordBuffer,
    types::{
        AccountCommitAt, AccountRecordSortKey, AccountSlot, BankKey, BlockMetadata,
        CoordinatorError, DiscardReason, InstructionRecordSortKey, InstructionSlot, ParseStatsKind,
    },
};

///
/// All inputs to the coordinator state machine.
///
/// A slot can have several banks (candidate blocks). Bank-scoped events carry the
/// `bank_id`, and only the bank that reaches confirmed is flushed.
///
pub enum CoordinatorEvent<R> {
    /// Block metadata received (from FrozenBlock via BlockSM wrapper).
    BlockFrozen {
        slot: Slot,
        bank_id: u64,
        metadata: BlockMetadata,
    },
    /// Bank confirmed by cluster consensus.
    SlotConfirmed { slot: Slot, bank_id: u64 },
    /// Bank finalized by cluster consensus.
    SlotFinalized { slot: Slot, bank_id: u64 },
    /// The slot left the chain. Dead drops its frozen banks and `bank_ids`, Incomplete drops every
    /// bank, and a new bank can still win after either. Forked drops no bank, a confirm wins.
    SlotDiscarded {
        slot: Slot,
        reason: DiscardReason,
        bank_ids: Vec<u64>,
    },
    /// One bank lost its slot to another bank, froze incomplete, or is in a slot that a
    /// confirmed slot skips.
    BankDiscarded {
        slot: Slot,
        bank_id: u64,
        reason: DiscardReason,
    },
    /// A raw Account event was seen on the geyser stream.
    /// Counted internally; frozen as expected_account_count when account gate is frozen.
    AccountEventSeen { slot: Slot, bank_id: u64 },
    /// A parsed instruction record from a handler (sorted by key).
    InstructionRecordParsed {
        slot: Slot,
        bank_id: u64,
        key: InstructionRecordSortKey,
        record: R,
    },
    /// A parsed account record from a handler (sorted by write_version:pubkey).
    AccountRecordParsed {
        slot: Slot,
        bank_id: u64,
        key: AccountRecordSortKey,
        record: R,
    },
    /// A handler finished parsing a transaction.
    TransactionParsed { slot: Slot, bank_id: u64 },
    /// A parse stat event (filtered or error).
    ParseStats {
        slot: Slot,
        bank_id: u64,
        kind: ParseStatsKind,
    },
}

/// Pure-ish coordinator state (no channels, no wrapper).
pub struct CoordinatorState<R> {
    buffer: BTreeMap<BankKey, SlotRecordBuffer<R>>,
    /// Slots off the chain, with why. A new frozen bank clears a dead or incomplete slot. A forked
    /// slot waits for a confirm of itself or of a child, or for its confirmed bank to prove
    /// incomplete, because no dead report follows a fork report.
    discarded_slots: BTreeMap<Slot, DiscardReason>,
    discarded_banks: BTreeMap<BankKey, DiscardReason>,
    confirmed_banks: BTreeMap<Slot, u64>,
    last_instruction_flushed_slot: Option<Slot>,
    last_account_flushed_slot: Option<Slot>,
    /// Running count of AccountEventSeen signals per bank.
    /// Frozen as expected_account_count when the account gate is frozen.
    account_event_counts: BTreeMap<BankKey, u64>,
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
            discarded_slots: BTreeMap::new(),
            discarded_banks: BTreeMap::new(),
            confirmed_banks: BTreeMap::new(),
            last_instruction_flushed_slot: None,
            last_account_flushed_slot: None,
            account_event_counts: BTreeMap::new(),
            account_commit_at,
            late_account_record_drops: 0,
            late_account_event_drops: 0,
        }
    }

    pub fn pending_bank_count(&self) -> usize { self.buffer.len() }

    pub fn discarded_slot_count(&self) -> usize { self.discarded_slots.len() }

    /// Reject delayed events for cleared, dead or losing banks. Incomplete banks still need
    /// their commitment event to discard the incomplete slot.
    pub(crate) fn is_invalidated_bank(&self, key: BankKey) -> bool {
        matches!(
            self.discarded_banks.get(&key),
            Some(DiscardReason::Dead | DiscardReason::Forked | DiscardReason::Cleared)
        )
    }

    /// `key` froze or confirmed, so nothing may clear it any more.
    pub(crate) fn is_bank_frozen_or_confirmed(&self, key: BankKey) -> bool {
        self.confirmed_banks.get(&key.slot) == Some(&key.bank_id)
            || self
                .buffer
                .get(&key)
                .is_some_and(|buf| buf.parent_slot().is_some())
    }

    pub(crate) fn has_bank(&self, key: BankKey) -> bool { self.buffer.contains_key(&key) }

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
        self.buffer.first_key_value().map(|(key, _)| key.slot)
    }

    pub fn late_account_record_drops(&self) -> u64 { self.late_account_record_drops }

    pub fn late_account_event_drops(&self) -> u64 { self.late_account_event_drops }

    pub fn apply(&mut self, event: CoordinatorEvent<R>) -> Result<(), CoordinatorError> {
        match event {
            CoordinatorEvent::BlockFrozen {
                slot,
                bank_id,
                metadata,
            } => {
                let key = BankKey::new(slot, bank_id);
                if self.is_discarded(key) || self.is_behind_instruction_frontier(key, "BlockFrozen")
                {
                    return Ok(());
                }
                // A new bank puts a dead or incomplete slot back on the chain.
                if !self.is_forked(slot) && self.discarded_slots.remove(&slot).is_some() {
                    tracing::info!(slot, bank_id, "New bank froze for a discarded slot");
                }
                let buf = self.buffer.entry(key).or_default();
                buf.set_block_metadata(metadata);
            },
            CoordinatorEvent::SlotConfirmed { slot, bank_id } => {
                let key = BankKey::new(slot, bank_id);
                if self.is_behind_instruction_frontier(key, "SlotConfirmed")
                    || !self.commit_bank(key)
                {
                    return Ok(());
                }
                if self.account_commit_at == AccountCommitAt::Confirmed {
                    self.freeze_account_gate(key);
                }
            },
            CoordinatorEvent::SlotFinalized { slot, bank_id } => {
                // Finalized is only relevant when account commit is configured at finalized.
                if self.account_commit_at != AccountCommitAt::Finalized {
                    return Ok(());
                }
                let key = BankKey::new(slot, bank_id);
                if self.is_already_flushed(slot, "SlotFinalized") || !self.commit_bank(key) {
                    return Ok(());
                }
                self.freeze_account_gate(key);
            },
            CoordinatorEvent::SlotDiscarded {
                slot,
                reason,
                bank_ids,
            } => {
                self.discard_slot(slot, reason, &bank_ids);
            },
            CoordinatorEvent::BankDiscarded {
                slot,
                bank_id,
                reason,
            } => {
                self.discard_bank(BankKey::new(slot, bank_id), reason);
            },
            CoordinatorEvent::AccountEventSeen { slot, bank_id } => {
                let key = BankKey::new(slot, bank_id);
                if self.is_discarded(key) {
                    self.log_discarded_bank_drop(key, "AccountEventSeen");
                    return Ok(());
                }
                if self.is_already_flushed(slot, "AccountEventSeen") {
                    return Ok(());
                }
                if self.is_account_gate_frozen(key) {
                    tracing::warn!(
                        slot,
                        bank_id,
                        "AccountEventSeen after account gate frozen — dropping"
                    );
                    self.late_account_event_drops += 1;
                    return Ok(());
                }
                *self.account_event_counts.entry(key).or_default() += 1;
            },
            CoordinatorEvent::InstructionRecordParsed {
                slot,
                bank_id,
                key,
                record,
            } => {
                let bank_key = BankKey::new(slot, bank_id);
                if !self.validate_instruction_bank(bank_key, "InstructionRecordParsed") {
                    return Ok(());
                }
                self.buffer
                    .entry(bank_key)
                    .or_default()
                    .insert_instruction_record(key, record);
            },
            CoordinatorEvent::AccountRecordParsed {
                slot,
                bank_id,
                key,
                record,
            } => {
                let bank_key = BankKey::new(slot, bank_id);
                if !self.validate_account_bank(bank_key, "AccountRecordParsed") {
                    return Ok(());
                }
                let buf = self.buffer.entry(bank_key).or_default();
                buf.insert_account_record(key, record);
                buf.increment_account_processed_count();
            },
            CoordinatorEvent::TransactionParsed { slot, bank_id } => {
                let key = BankKey::new(slot, bank_id);
                if !self.validate_instruction_bank(key, "TransactionParsed") {
                    return Ok(());
                }
                self.buffer
                    .entry(key)
                    .or_default()
                    .increment_parsed_tx_count();
            },
            CoordinatorEvent::ParseStats {
                slot,
                bank_id,
                kind,
            } => {
                let key = BankKey::new(slot, bank_id);
                let is_account_stat = matches!(
                    kind,
                    ParseStatsKind::AccountFiltered | ParseStatsKind::AccountError
                );
                if is_account_stat {
                    if !self.validate_account_bank(key, "ParseStats") {
                        return Ok(());
                    }
                    let buf = self.buffer.entry(key).or_default();
                    buf.increment_parse_stat(kind);
                    buf.increment_account_processed_count();
                } else {
                    if !self.validate_instruction_bank(key, "ParseStats") {
                        return Ok(());
                    }
                    self.buffer
                        .entry(key)
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

        let mut next_slot = self.first_buffered_slot_from(0);
        while let Some(slot) = next_slot {
            next_slot = slot
                .checked_add(1)
                .and_then(|from| self.first_buffered_slot_from(from));
            if self
                .last_instruction_flushed_slot
                .is_some_and(|last| slot <= last)
            {
                continue;
            }
            let Some(&bank_id) = self.confirmed_banks.get(&slot) else {
                // Banks of a discarded slot that never win wait here until pruned.
                if self.discarded_slots.contains_key(&slot) {
                    continue;
                }
                break;
            };
            let key = BankKey::new(slot, bank_id);
            // A winner that a BankDiscarded dropped after its confirm has no buffer.
            let Some(buf) = self.buffer.get(&key) else {
                continue;
            };
            // Stop at the first non-ready slot to maintain ordering.
            if !buf.instruction_gate_reached() {
                break;
            }

            let parent_slot = buf
                .parent_slot()
                .ok_or(CoordinatorError::ReadySlotMissingMetadata { slot })?;
            if !self.parent_flushed(parent_slot, self.last_instruction_flushed_slot) {
                break;
            }

            let ix_slot = self
                .buffer
                .get_mut(&key)
                .and_then(|buf| buf.drain_instruction_records(slot))
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

        let mut next_slot = self.first_buffered_slot_from(0);
        while let Some(slot) = next_slot {
            next_slot = slot
                .checked_add(1)
                .and_then(|from| self.first_buffered_slot_from(from));
            let Some(&bank_id) = self.confirmed_banks.get(&slot) else {
                if self.discarded_slots.contains_key(&slot) {
                    continue;
                }
                break;
            };
            let key = BankKey::new(slot, bank_id);
            let Some(buf) = self.buffer.get(&key) else {
                continue;
            };
            if buf.accounts_drained() {
                continue;
            }
            if !buf.account_gate_reached() {
                break;
            }

            // Accounts follow the chain like instructions. Otherwise a child that confirms before
            // its fork-reported parent flushes first, and cleanup then drops the parent's accounts.
            // A bank whose BlockFrozen came behind the instruction frontier has no parent to check.
            let parent_flushed = buf
                .parent_slot()
                .is_none_or(|parent| self.parent_flushed(parent, self.last_account_flushed_slot));
            if !parent_flushed {
                break;
            }

            let Some(buf) = self.buffer.get_mut(&key) else {
                continue;
            };
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
        let drained: Vec<BankKey> = self
            .buffer
            .iter()
            .filter(|(_, buf)| buf.is_fully_drained())
            .map(|(&s, _)| s)
            .collect();
        for key in drained {
            self.buffer.remove(&key);
        }
        // A bank at or below both flush points can never flush.
        if let (Some(ix), Some(acct)) = (
            self.last_instruction_flushed_slot,
            self.last_account_flushed_slot,
        ) {
            let last = ix.min(acct);
            self.buffer.retain(|key, _| key.slot > last);
        }
        self.prune_discarded_slots();
        // Prune by the account frontier, not the instruction one: in finalized mode
        // instruction slots flush far earlier, and pruning by them drops valid counts.
        if let Some(last) = self.last_account_flushed_slot {
            self.account_event_counts = self
                .account_event_counts
                .split_off(&BankKey::first(last.saturating_add(1)));
        }
    }

    /// A slot flushes after its parent, so output follows the chain: the parent is behind the
    /// drain's frontier, or was discarded incomplete. The first flush has no frontier. A node
    /// replays a child only on a frozen parent, so a child of a dead slot waits for the repair's
    /// BlockMeta, which can come after the child's. A fork-reported parent's own confirm follows
    /// its child's, so the child waits for it too.
    fn parent_flushed(&self, parent_slot: Slot, frontier: Option<Slot>) -> bool {
        let Some(last) = frontier else {
            return true;
        };
        parent_slot <= last
            || matches!(
                self.discarded_slots.get(&parent_slot),
                Some(DiscardReason::Incomplete)
            )
    }

    /// Guard for SlotFinalized and AccountEventSeen: drop an event for a slot that both
    /// drains already flushed.
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

    /// A bank that freezes or confirms at or behind the instruction frontier can never
    /// flush in order, so drop the event. The account frontier lags in finalized mode.
    fn is_behind_instruction_frontier(&self, key: BankKey, event: &'static str) -> bool {
        let BankKey { slot, bank_id } = key;
        let behind = self
            .last_instruction_flushed_slot
            .is_some_and(|last| slot <= last);
        if behind {
            tracing::warn!(
                slot,
                bank_id,
                event,
                last_instruction_flushed = ?self.last_instruction_flushed_slot,
                "Lifecycle event behind the instruction frontier; dropping it"
            );
        }
        behind
    }

    /// Instruction events for slots at or behind the flush frontier are stale.
    fn validate_instruction_bank(&self, key: BankKey, event: &'static str) -> bool {
        let BankKey { slot, bank_id } = key;
        if self.is_discarded(key) {
            self.log_discarded_bank_drop(key, event);
            return false;
        }
        if self
            .last_instruction_flushed_slot
            .is_some_and(|last| slot <= last)
        {
            tracing::warn!(
                slot,
                bank_id,
                event,
                last_instruction_flushed = ?self.last_instruction_flushed_slot,
                last_account_flushed = ?self.last_account_flushed_slot,
                oldest_pending_slot = ?self.oldest_pending_slot(),
                discarded_slot_count = self.discarded_slots.len(),
                "Instruction event for slot at or behind flush frontier; dropping"
            );
            return false;
        }
        true
    }

    /// Relaxed validation for account events — warn + drop if post-flush (not fatal).
    fn validate_account_bank(&mut self, key: BankKey, event: &'static str) -> bool {
        let BankKey { slot, bank_id } = key;
        if self.is_discarded(key) {
            self.log_discarded_bank_drop(key, event);
            return false;
        }
        if self
            .last_account_flushed_slot
            .is_some_and(|last| slot <= last)
        {
            tracing::warn!(
                slot,
                bank_id,
                last_account_flushed = ?self.last_account_flushed_slot,
                "Late account event after flush — dropping"
            );
            self.late_account_record_drops += 1;
            return false;
        }
        // Also drop if accounts already drained for this bank.
        if let Some(buf) = self.buffer.get(&key)
            && buf.accounts_drained()
        {
            tracing::warn!(
                slot,
                bank_id,
                "Account event after accounts drained; dropping"
            );
            self.late_account_record_drops += 1;
            return false;
        }
        true
    }

    fn log_discarded_bank_drop(&self, key: BankKey, event: &'static str) {
        let BankKey { slot, bank_id } = key;
        // A live slot can have losing banks, so only a discarded slot warns.
        if !self.discarded_slots.contains_key(&slot) {
            tracing::debug!(slot, bank_id, event, "Dropping event for a losing bank");
            return;
        }
        tracing::warn!(
            slot,
            bank_id,
            event,
            had_buffer = self.buffer.contains_key(&key),
            buffered_record_count = self
                .buffer
                .get(&key)
                .map_or(0, SlotRecordBuffer::record_count),
            last_instruction_flushed = ?self.last_instruction_flushed_slot,
            last_account_flushed = ?self.last_account_flushed_slot,
            oldest_pending_slot = ?self.oldest_pending_slot(),
            discarded_slot_count = self.discarded_slots.len(),
            "Dropping event for known discarded slot"
        );
    }

    /// Freeze the account gate for a bank: move account_event_counts into expected_account_count
    /// and mark account_commitment_reached.
    fn freeze_account_gate(&mut self, key: BankKey) {
        let count = self.account_event_counts.remove(&key).unwrap_or(0);
        let buf = self.buffer.entry(key).or_default();
        buf.set_expected_account_count(count);
        buf.mark_account_commitment_reached();
    }

    /// Check if the account gate has been frozen for a bank (expected_account_count is set).
    fn is_account_gate_frozen(&self, key: BankKey) -> bool {
        self.buffer
            .get(&key)
            .is_some_and(|buf| buf.is_account_gate_frozen())
    }

    fn is_discarded(&self, key: BankKey) -> bool {
        let BankKey { slot, bank_id } = key;
        self.discarded_banks.contains_key(&key)
            || self
                .confirmed_banks
                .get(&slot)
                .is_some_and(|&winner| winner != bank_id)
    }

    fn is_forked(&self, slot: Slot) -> bool {
        matches!(self.discarded_slots.get(&slot), Some(DiscardReason::Forked))
    }

    /// Mark `key` as its slot's confirmed bank, so its other banks count as discarded even with no
    /// BankDiscarded. Returns false when this bank froze incomplete, so it never flushes.
    fn commit_bank(&mut self, key: BankKey) -> bool {
        let BankKey { slot, bank_id } = key;
        if matches!(
            self.discarded_banks.get(&key),
            Some(DiscardReason::Incomplete)
        ) {
            if self.is_forked(slot) || !self.discarded_slots.contains_key(&slot) {
                self.discard_slot(slot, DiscardReason::Incomplete, &[bank_id]);
            }
            return false;
        }
        self.confirmed_banks.insert(slot, bank_id);
        // A confirm wins over a fork report. The descendants of a repaired slot get one too, so
        // keep confirmed slots out of the set, or it fills and warns.
        self.discarded_slots.remove(&slot);

        // A confirmed child puts its parent on the chain. A parent confirm that the server fills
        // in comes after the child's, so a fork-reported parent with a frozen bank waits for it.
        if let Some(parent) = self.buffer.get(&key).and_then(|buf| buf.parent_slot())
            && self.is_forked(parent)
            && self.has_frozen_bank(parent)
        {
            self.discarded_slots.remove(&parent);
        }

        self.buffer
            .entry(key)
            .or_default()
            .mark_instruction_commitment_reached();
        true
    }

    fn discard_bank(&mut self, key: BankKey, reason: DiscardReason) {
        let BankKey { slot, bank_id } = key;
        let newly_discarded = !self.discarded_banks.contains_key(&key);
        let record_count = self.drop_bank(key, reason);
        if !newly_discarded {
            return;
        }
        // A fast leader handover clears a bank routinely, so that is not a warning.
        if matches!(reason, DiscardReason::Cleared) {
            tracing::debug!(slot, bank_id, record_count, "Bank cleared by UpdateParent");
            return;
        }
        tracing::warn!(slot, bank_id, %reason, record_count, "Discarding bank");
    }

    /// Tombstone `key` and drop its records. Returns how many records it held.
    fn drop_bank(&mut self, key: BankKey, reason: DiscardReason) -> usize {
        // Keep the first reason, so a later fork cannot hide an incomplete bank.
        self.discarded_banks.entry(key).or_insert(reason);
        self.account_event_counts.remove(&key);
        self.buffer
            .remove(&key)
            .map_or(0, |buffer| buffer.record_count())
    }

    /// A bank of `slot` has frozen, so its buffer holds block metadata.
    fn has_frozen_bank(&self, slot: Slot) -> bool {
        self.buffer
            .range(BankKey::slot_range(slot))
            .any(|(_, buf)| buf.parent_slot().is_some())
    }

    /// `slot` has a frozen bank, and a confirmed bank names it as parent.
    fn named_by_confirmed_child(&self, slot: Slot) -> bool {
        self.has_frozen_bank(slot)
            && self
                .confirmed_banks
                .range(slot.saturating_add(1)..)
                .any(|(&child, &bank_id)| {
                    self.buffer
                        .get(&BankKey::new(child, bank_id))
                        .and_then(|buf| buf.parent_slot())
                        == Some(slot)
                })
    }

    /// Every bank of `slot` that has a buffer or an account count.
    pub(crate) fn banks_of(&self, slot: Slot) -> BTreeSet<BankKey> {
        let range = BankKey::slot_range(slot);
        self.buffer
            .range(range.clone())
            .map(|(&key, _)| key)
            .chain(self.account_event_counts.range(range).map(|(&key, _)| key))
            .collect()
    }

    fn first_buffered_slot_from(&self, from: Slot) -> Option<Slot> {
        self.buffer
            .range(BankKey::first(from)..)
            .next()
            .map(|(key, _)| key.slot)
    }

    fn discard_slot(&mut self, slot: Slot, reason: DiscardReason, bank_ids: &[u64]) {
        // A confirm wins over a fork report in either order. With two banks the slot resolves
        // only at its confirm, so the report can come after it.
        if matches!(reason, DiscardReason::Forked)
            && (self.confirmed_banks.contains_key(&slot) || self.named_by_confirmed_child(slot))
        {
            return;
        }
        let was_known_discarded = self.discarded_slots.contains_key(&slot);
        // Forked sticks over a dead report, since none follows a fork report. Incomplete means
        // the slot's confirmed bank misses data, so the slot is final and ends the fork wait.
        let stored = self.discarded_slots.entry(slot).or_insert(reason);
        if !matches!(stored, DiscardReason::Forked) || matches!(reason, DiscardReason::Incomplete) {
            *stored = reason;
        }

        // Dead is final for each reported bank, and a repair gets a new bank ID. A fork report
        // also reaches the descendants of a repaired slot, so it drops no bank.
        let reported: BTreeSet<BankKey> = bank_ids
            .iter()
            .map(|&bank_id| BankKey::new(slot, bank_id))
            .collect();
        let dropped: BTreeSet<BankKey> = self
            .banks_of(slot)
            .into_iter()
            .chain(reported.iter().copied())
            .filter(|key| {
                let frozen = self
                    .buffer
                    .get(key)
                    .is_some_and(|buf| buf.parent_slot().is_some());
                match reason {
                    DiscardReason::Incomplete => true,
                    DiscardReason::Dead => frozen || reported.contains(key),
                    DiscardReason::Forked | DiscardReason::Cleared => false,
                }
            })
            .collect();
        let record_count: usize = dropped
            .into_iter()
            .map(|key| self.drop_bank(key, reason))
            .sum();

        self.confirmed_banks.remove(&slot);
        self.prune_discarded_slots();
        // A fork report drops nothing, and block-machine 0.11 can send one for every slot after a
        // repaired dead slot, so it is not a warning.
        if matches!(reason, DiscardReason::Forked) {
            tracing::debug!(
                slot,
                was_known_discarded,
                "Fork report; the slot waits for a confirm"
            );
            return;
        }
        tracing::warn!(
            slot,
            %reason,
            record_count,
            was_known_discarded,
            last_instruction_flushed = ?self.last_instruction_flushed_slot,
            last_account_flushed = ?self.last_account_flushed_slot,
            "Discarding slot"
        );
    }

    ///
    /// Remove markers far behind both flush frontiers; recent ones still drop late records.
    /// While one frontier is unset, at startup, its drain can still need a confirm, so nothing is
    /// pruned. Past `MAX_DISCARDED_SLOTS` it only warns, since evicting could stall the pipeline.
    ///
    fn prune_discarded_slots(&mut self) {
        if let (Some(ix), Some(acct)) = (
            self.last_instruction_flushed_slot,
            self.last_account_flushed_slot,
        ) {
            let min_retained = ix
                .min(acct)
                .saturating_sub(Self::DISCARDED_SLOT_RETENTION_BEHIND_FLUSH);
            let first_kept = min_retained.saturating_add(1);
            self.discarded_slots = self.discarded_slots.split_off(&first_kept);
            self.discarded_banks = self.discarded_banks.split_off(&BankKey::first(first_kept));
            self.confirmed_banks = self.confirmed_banks.split_off(&first_kept);
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
                bank_id: 0,
                metadata: metadata(parent, expected_tx_count),
            })
            .unwrap();

        for _ in 0..expected_tx_count {
            state
                .apply(CoordinatorEvent::TransactionParsed { slot, bank_id: 0 })
                .unwrap();
        }

        state
            .apply(CoordinatorEvent::SlotConfirmed { slot, bank_id: 0 })
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

    /// In finalized mode the account frontier lags. A bank that freezes for a discarded
    /// slot behind the instruction frontier must stay discarded, or later slots stall.
    #[test]
    fn late_bank_behind_instruction_frontier_stays_discarded() {
        let mut state = CoordinatorState::<String>::new(AccountCommitAt::Finalized);
        apply_ready_slot(&mut state, 41, 40, 0);
        state
            .apply(CoordinatorEvent::SlotDiscarded {
                slot: 42,
                reason: DiscardReason::Dead,
                bank_ids: vec![0],
            })
            .unwrap();
        // Slot 43 skips the dead slot, so the instruction frontier passes it.
        apply_ready_slot(&mut state, 43, 41, 0);
        assert_eq!(drain_both(&mut state).unwrap().len(), 2);

        state
            .apply(CoordinatorEvent::BlockFrozen {
                slot: 42,
                bank_id: 1,
                metadata: metadata(41, 0),
            })
            .unwrap();
        state
            .apply(CoordinatorEvent::SlotConfirmed {
                slot: 42,
                bank_id: 1,
            })
            .unwrap();
        apply_ready_slot(&mut state, 44, 43, 0);

        let flushed = drain_both(&mut state).unwrap();
        assert_eq!(flushed.len(), 1);
        assert_eq!(flushed[0].slot, 44);

        for slot in [41, 43, 44] {
            state
                .apply(CoordinatorEvent::SlotFinalized { slot, bank_id: 0 })
                .unwrap();
        }
        let accounts: Vec<_> = state
            .drain_account_flushable()
            .iter()
            .map(|s| s.slot)
            .collect();
        assert_eq!(accounts, [41, 43, 44]);
    }

    /// In finalized account mode a slot behind the instruction frontier can still get its
    /// commit. Its instructions must not flush after a later slot.
    #[test]
    fn instruction_drain_never_goes_back() {
        let mut state = CoordinatorState::<String>::new(AccountCommitAt::Finalized);
        apply_ready_slot(&mut state, 41, 40, 0);
        state
            .apply(CoordinatorEvent::BlockFrozen {
                slot: 42,
                bank_id: 0,
                metadata: metadata(41, 0),
            })
            .unwrap();
        state
            .apply(CoordinatorEvent::SlotDiscarded {
                slot: 42,
                reason: DiscardReason::Forked,
                bank_ids: vec![],
            })
            .unwrap();
        apply_ready_slot(&mut state, 43, 41, 0);
        let slots: Vec<_> = drain_both(&mut state)
            .unwrap()
            .iter()
            .map(|s| s.slot)
            .collect();
        assert_eq!(slots, [41, 43]);

        state
            .apply(CoordinatorEvent::SlotFinalized {
                slot: 42,
                bank_id: 0,
            })
            .unwrap();
        assert!(state.drain_instruction_flushable().unwrap().is_empty());
    }

    /// Slot 42 is fork-reported, and its child 43 confirms before 42's BlockMeta arrives. The
    /// confirm puts 42 on the chain, so 43 waits for 42's own confirm, which comes later.
    #[test]
    fn child_waits_for_a_fork_reported_parent() {
        let mut state = CoordinatorState::<String>::default();
        apply_ready_slot(&mut state, 41, 40, 0);
        state
            .apply(CoordinatorEvent::SlotDiscarded {
                slot: 42,
                reason: DiscardReason::Forked,
                bank_ids: vec![],
            })
            .unwrap();
        apply_ready_slot(&mut state, 43, 42, 0);
        let slots: Vec<_> = drain_both(&mut state)
            .unwrap()
            .iter()
            .map(|s| s.slot)
            .collect();
        assert_eq!(slots, [41]);

        state
            .apply(CoordinatorEvent::BlockFrozen {
                slot: 42,
                bank_id: 0,
                metadata: metadata(41, 0),
            })
            .unwrap();
        assert!(drain_both(&mut state).unwrap().is_empty());

        state
            .apply(CoordinatorEvent::SlotConfirmed {
                slot: 42,
                bank_id: 0,
            })
            .unwrap();
        let slots: Vec<_> = drain_both(&mut state)
            .unwrap()
            .iter()
            .map(|s| s.slot)
            .collect();
        assert_eq!(slots, [42, 43]);
    }

    /// A fork-reported parent whose confirmed bank misses data is discarded as incomplete.
    /// That ends the child's wait, or the pipeline would stall on it forever.
    #[test]
    fn incomplete_report_ends_the_wait_for_a_fork_reported_parent() {
        let mut state = CoordinatorState::<String>::default();
        apply_ready_slot(&mut state, 41, 40, 0);
        state
            .apply(CoordinatorEvent::SlotDiscarded {
                slot: 42,
                reason: DiscardReason::Forked,
                bank_ids: vec![],
            })
            .unwrap();
        apply_ready_slot(&mut state, 43, 42, 0);
        assert_eq!(drain_both(&mut state).unwrap().len(), 1);

        state
            .apply(CoordinatorEvent::SlotDiscarded {
                slot: 42,
                reason: DiscardReason::Incomplete,
                bank_ids: vec![],
            })
            .unwrap();
        let slots: Vec<_> = drain_both(&mut state)
            .unwrap()
            .iter()
            .map(|s| s.slot)
            .collect();
        assert_eq!(slots, [43]);
    }

    /// In finalized account mode the confirmed bank's accounts wait for finalized, and
    /// the other bank's accounts never flush.
    #[test]
    fn finalized_mode_flushes_only_confirmed_bank_accounts() {
        let mut state = CoordinatorState::<String>::new(AccountCommitAt::Finalized);
        // The loser goes last with the same key, so a leak into bank 8 replaces its record.
        for bank_id in [8, 7] {
            state
                .apply(CoordinatorEvent::AccountEventSeen { slot: 42, bank_id })
                .unwrap();
            state
                .apply(CoordinatorEvent::AccountRecordParsed {
                    slot: 42,
                    bank_id,
                    key: AccountRecordSortKey::new(1, [1; 32]),
                    record: bank_id.to_string(),
                })
                .unwrap();
        }

        state
            .apply(CoordinatorEvent::SlotConfirmed {
                slot: 42,
                bank_id: 8,
            })
            .unwrap();
        assert!(state.drain_account_flushable().is_empty());

        state
            .apply(CoordinatorEvent::SlotFinalized {
                slot: 42,
                bank_id: 8,
            })
            .unwrap();
        let flushed = state.drain_account_flushable();
        assert_eq!(flushed.len(), 1);
        assert_eq!(flushed[0].records, ["8"]);
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

    /// A node replays a child only on a frozen parent, so a child of a dead slot waits for the
    /// repair, whose BlockMeta can come after the child's, and flushes after it.
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
                bank_ids: vec![],
            })
            .unwrap();
        assert!(drain_both(&mut state).unwrap().is_empty());

        apply_ready_slot(&mut state, 101, 100, 0);
        let slots: Vec<_> = drain_both(&mut state)
            .unwrap()
            .iter()
            .map(|s| s.slot)
            .collect();
        assert_eq!(slots, [101, 102]);
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
                bank_ids: vec![],
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
                bank_id: 0,
                metadata: metadata(99, 2),
            })
            .unwrap();

        state
            .apply(CoordinatorEvent::InstructionRecordParsed {
                slot,
                bank_id: 0,
                key: InstructionRecordSortKey::new(1, vec![0]),
                record: "b".to_string(),
            })
            .unwrap();
        state
            .apply(CoordinatorEvent::InstructionRecordParsed {
                slot,
                bank_id: 0,
                key: InstructionRecordSortKey::new(0, vec![0]),
                record: "a".to_string(),
            })
            .unwrap();

        state
            .apply(CoordinatorEvent::TransactionParsed { slot, bank_id: 0 })
            .unwrap();
        state
            .apply(CoordinatorEvent::TransactionParsed { slot, bank_id: 0 })
            .unwrap();
        state
            .apply(CoordinatorEvent::SlotConfirmed { slot, bank_id: 0 })
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
            .apply(CoordinatorEvent::SlotConfirmed {
                slot: 100,
                bank_id: 0,
            })
            .unwrap();

        let flushed = drain_both(&mut state).unwrap();
        assert!(flushed.is_empty());
        assert_eq!(state.pending_bank_count(), 0);
    }

    #[test]
    fn post_flush_instruction_events_without_discard_tombstone_are_dropped() {
        let mut state = CoordinatorState::<String>::default();
        apply_ready_slot(&mut state, 100, 99, 0);

        let _ = drain_both(&mut state).unwrap();

        state
            .apply(CoordinatorEvent::InstructionRecordParsed {
                slot: 100,
                bank_id: 0,
                key: InstructionRecordSortKey::new(0, vec![0]),
                record: "late".to_string(),
            })
            .unwrap();
        state
            .apply(CoordinatorEvent::TransactionParsed {
                slot: 100,
                bank_id: 0,
            })
            .unwrap();

        let flushed = drain_both(&mut state).unwrap();
        assert!(flushed.is_empty());
        assert_eq!(state.pending_bank_count(), 0);
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
                    bank_ids: vec![],
                })
                .unwrap();
        }

        // Slot 1200 skips the dead slots.
        apply_ready_slot(&mut state, 1200, 900, 0);

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
                    bank_ids: vec![],
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
                bank_ids: vec![],
            })
            .unwrap();

        apply_ready_slot(&mut state, 100, 99, 0);
        let flushed = drain_both(&mut state).unwrap();
        assert_eq!(flushed.len(), 1);

        assert!(state.discarded_slots.contains_key(&96));
    }

    #[test]
    fn late_parsed_message_for_recent_discarded_slot_is_dropped_after_flush_frontier_advances() {
        let mut state = CoordinatorState::<String>::default();

        state
            .apply(CoordinatorEvent::SlotDiscarded {
                slot: 96,
                reason: DiscardReason::Forked,
                bank_ids: vec![],
            })
            .unwrap();

        apply_ready_slot(&mut state, 100, 99, 0);
        let flushed = drain_both(&mut state).unwrap();
        assert_eq!(flushed.len(), 1);

        state
            .apply(CoordinatorEvent::InstructionRecordParsed {
                slot: 96,
                bank_id: 0,
                key: InstructionRecordSortKey::new(0, vec![0]),
                record: "late fork".to_string(),
            })
            .unwrap();
        state
            .apply(CoordinatorEvent::TransactionParsed {
                slot: 96,
                bank_id: 0,
            })
            .unwrap();

        assert_eq!(state.pending_bank_count(), 0);
    }

    #[test]
    fn observability_accessors() {
        let mut state = CoordinatorState::<String>::default();

        assert_eq!(state.pending_bank_count(), 0);
        assert_eq!(state.discarded_slot_count(), 0);
        assert_eq!(state.last_flushed_slot(), None);
        assert_eq!(state.oldest_pending_slot(), None);

        apply_ready_slot(&mut state, 100, 99, 0);
        apply_ready_slot(&mut state, 102, 101, 0);
        assert_eq!(state.pending_bank_count(), 2);
        assert_eq!(state.oldest_pending_slot(), Some(100));

        state
            .apply(CoordinatorEvent::SlotDiscarded {
                slot: 50,
                reason: DiscardReason::Dead,
                bank_ids: vec![],
            })
            .unwrap();
        assert_eq!(state.discarded_slot_count(), 1);

        let flushed = drain_both(&mut state).unwrap();
        assert_eq!(flushed.len(), 1);
        assert_eq!(state.last_flushed_slot(), Some(100));
        assert_eq!(state.pending_bank_count(), 1);
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
                bank_id: 0,
                metadata: metadata(99, 0),
            })
            .unwrap();
        state
            .apply(CoordinatorEvent::AccountEventSeen { slot, bank_id: 0 })
            .unwrap();
        state
            .apply(CoordinatorEvent::AccountEventSeen { slot, bank_id: 0 })
            .unwrap();
        state
            .apply(CoordinatorEvent::SlotConfirmed { slot, bank_id: 0 })
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
                bank_id: 0,
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
                bank_id: 0,
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
                bank_id: 0,
                metadata: metadata(99, 1),
            })
            .unwrap();
        state
            .apply(CoordinatorEvent::TransactionParsed { slot, bank_id: 0 })
            .unwrap();
        state
            .apply(CoordinatorEvent::AccountEventSeen { slot, bank_id: 0 })
            .unwrap();
        state
            .apply(CoordinatorEvent::AccountRecordParsed {
                slot,
                bank_id: 0,
                key: AccountRecordSortKey::new(1, [1; 32]),
                record: "acct".to_string(),
            })
            .unwrap();
        state
            .apply(CoordinatorEvent::SlotConfirmed { slot, bank_id: 0 })
            .unwrap();

        // Instructions flush at confirmed.
        let ix_flushed = state.drain_instruction_flushable().unwrap();
        assert_eq!(ix_flushed.len(), 1);

        // Accounts NOT ready yet (waiting for finalized).
        let acct_flushed = state.drain_account_flushable();
        assert!(acct_flushed.is_empty());

        // Finalize → accounts flush.
        state
            .apply(CoordinatorEvent::SlotFinalized { slot, bank_id: 0 })
            .unwrap();
        let acct_flushed = state.drain_account_flushable();
        assert_eq!(acct_flushed.len(), 1);
        assert_eq!(acct_flushed[0].records, vec!["acct".to_string()]);
    }

    #[test]
    fn finalized_event_is_ignored_when_account_commitment_is_confirmed() {
        let mut state = CoordinatorState::<String>::default();

        state
            .apply(CoordinatorEvent::SlotFinalized {
                slot: 123,
                bank_id: 0,
            })
            .unwrap();

        assert_eq!(state.pending_bank_count(), 0);
        assert_eq!(state.oldest_pending_slot(), None);
    }

    #[test]
    fn account_event_after_gate_frozen_is_warn_not_error() {
        let mut state = CoordinatorState::<String>::default();
        let slot = 100;

        state
            .apply(CoordinatorEvent::BlockFrozen {
                slot,
                bank_id: 0,
                metadata: metadata(99, 0),
            })
            .unwrap();
        state
            .apply(CoordinatorEvent::SlotConfirmed { slot, bank_id: 0 })
            .unwrap();

        // Drain accounts (0 expected, 0 processed → immediately ready).
        let acct_flushed = state.drain_account_flushable();
        assert_eq!(acct_flushed.len(), 1);

        // Late AccountEventSeen → warn + drop (not error).
        state
            .apply(CoordinatorEvent::AccountEventSeen { slot, bank_id: 0 })
            .unwrap();
        assert_eq!(state.late_account_event_drops(), 1);
    }

    #[test]
    fn account_event_after_gate_frozen_before_ready_is_dropped() {
        let mut state = CoordinatorState::<String>::default();
        let slot = 101;

        // Count one account event before freeze.
        state
            .apply(CoordinatorEvent::AccountEventSeen { slot, bank_id: 0 })
            .unwrap();
        state
            .apply(CoordinatorEvent::BlockFrozen {
                slot,
                bank_id: 0,
                metadata: metadata(100, 0),
            })
            .unwrap();
        state
            .apply(CoordinatorEvent::SlotConfirmed { slot, bank_id: 0 })
            .unwrap();

        // Gate is frozen with expected_account_count=1, but not yet ready.
        // Another AccountEventSeen must be treated as late and dropped.
        state
            .apply(CoordinatorEvent::AccountEventSeen { slot, bank_id: 0 })
            .unwrap();
        assert_eq!(state.late_account_event_drops(), 1);

        // One processed account should still be enough to flush.
        state
            .apply(CoordinatorEvent::AccountRecordParsed {
                slot,
                bank_id: 0,
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
            .apply(CoordinatorEvent::AccountEventSeen {
                slot: 100,
                bank_id: 0,
            })
            .unwrap();

        // A later slot advances only the instruction frontier.
        state
            .apply(CoordinatorEvent::BlockFrozen {
                slot: 200,
                bank_id: 0,
                metadata: metadata(199, 0),
            })
            .unwrap();
        state
            .apply(CoordinatorEvent::SlotConfirmed {
                slot: 200,
                bank_id: 0,
            })
            .unwrap();
        let ix_flushed = state.drain_instruction_flushable().unwrap();
        assert_eq!(ix_flushed.len(), 1);
        assert_eq!(ix_flushed[0].slot, 200);

        // Now freeze account gate for slot 100 at finalized.
        state
            .apply(CoordinatorEvent::BlockFrozen {
                slot: 100,
                bank_id: 0,
                metadata: metadata(99, 0),
            })
            .unwrap();
        state
            .apply(CoordinatorEvent::SlotConfirmed {
                slot: 100,
                bank_id: 0,
            })
            .unwrap();
        state
            .apply(CoordinatorEvent::SlotFinalized {
                slot: 100,
                bank_id: 0,
            })
            .unwrap();

        // Should not flush yet: expected_account_count must still be 1.
        let acct_flushed = state.drain_account_flushable();
        assert!(acct_flushed.is_empty());

        state
            .apply(CoordinatorEvent::AccountRecordParsed {
                slot: 100,
                bank_id: 0,
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
                bank_id: 0,
                key: AccountRecordSortKey::new(1, [1; 32]),
                record: "late".to_string(),
            })
            .unwrap();
        assert_eq!(state.late_account_record_drops(), 1);

        // Late AccountFiltered stat → warn + drop.
        state
            .apply(CoordinatorEvent::ParseStats {
                slot,
                bank_id: 0,
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
                bank_id: 0,
                metadata: metadata(99, 1),
            })
            .unwrap();
        state
            .apply(CoordinatorEvent::AccountRecordParsed {
                slot,
                bank_id: 0,
                key: AccountRecordSortKey::new(42, [1; 32]),
                record: "acct".to_string(),
            })
            .unwrap();
        state
            .apply(CoordinatorEvent::TransactionParsed { slot, bank_id: 0 })
            .unwrap();
        state
            .apply(CoordinatorEvent::AccountEventSeen { slot, bank_id: 0 })
            .unwrap();
        state
            .apply(CoordinatorEvent::SlotConfirmed { slot, bank_id: 0 })
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
            .apply(CoordinatorEvent::AccountEventSeen { slot, bank_id: 0 })
            .unwrap();
        assert!(state
            .account_event_counts
            .contains_key(&BankKey::new(slot, 0)));

        state
            .apply(CoordinatorEvent::BlockFrozen {
                slot,
                bank_id: 0,
                metadata: metadata(99, 0),
            })
            .unwrap();
        assert!(state
            .account_event_counts
            .contains_key(&BankKey::new(slot, 0)));

        state
            .apply(CoordinatorEvent::AccountRecordParsed {
                slot,
                bank_id: 0,
                key: AccountRecordSortKey::new(1, [1; 32]),
                record: "acct".to_string(),
            })
            .unwrap();

        state
            .apply(CoordinatorEvent::SlotConfirmed { slot, bank_id: 0 })
            .unwrap();
        assert!(!state
            .account_event_counts
            .contains_key(&BankKey::new(slot, 0)));

        let acct_flushed = state.drain_account_flushable();
        assert_eq!(acct_flushed.len(), 1);
    }

    #[test]
    fn account_event_for_never_frozen_slot_does_not_stall_flush() {
        let mut state = CoordinatorState::<String>::default();

        state
            .apply(CoordinatorEvent::AccountEventSeen {
                slot: 100,
                bank_id: 0,
            })
            .unwrap();

        apply_ready_slot(&mut state, 200, 199, 0);
        let flushed = drain_both(&mut state).unwrap();
        assert_eq!(flushed.len(), 1);
        assert_eq!(flushed[0].slot, 200);

        assert!(!state
            .account_event_counts
            .contains_key(&BankKey::new(100, 0)));
    }

    #[test]
    fn malformed_pubkey_account_error_still_flushes() {
        let mut state = CoordinatorState::<String>::default();
        let slot = 100;

        state
            .apply(CoordinatorEvent::BlockFrozen {
                slot,
                bank_id: 0,
                metadata: metadata(99, 0),
            })
            .unwrap();
        state
            .apply(CoordinatorEvent::AccountEventSeen { slot, bank_id: 0 })
            .unwrap();
        state
            .apply(CoordinatorEvent::ParseStats {
                slot,
                bank_id: 0,
                kind: ParseStatsKind::AccountError,
            })
            .unwrap();
        state
            .apply(CoordinatorEvent::SlotConfirmed { slot, bank_id: 0 })
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
                bank_id: 0,
                metadata: metadata(99, 0),
            })
            .unwrap();
        state
            .apply(CoordinatorEvent::AccountEventSeen { slot, bank_id: 0 })
            .unwrap();
        state
            .apply(CoordinatorEvent::AccountEventSeen { slot, bank_id: 0 })
            .unwrap();
        state
            .apply(CoordinatorEvent::AccountRecordParsed {
                slot,
                bank_id: 0,
                key: AccountRecordSortKey::new(5, [1; 32]),
                record: "first".to_string(),
            })
            .unwrap();
        state
            .apply(CoordinatorEvent::AccountRecordParsed {
                slot,
                bank_id: 0,
                key: AccountRecordSortKey::new(10, [1; 32]),
                record: "second".to_string(),
            })
            .unwrap();
        state
            .apply(CoordinatorEvent::SlotConfirmed { slot, bank_id: 0 })
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
        assert_eq!(state.pending_bank_count(), 1);

        // Drain accounts.
        let acct = state.drain_account_flushable();
        assert_eq!(acct.len(), 1);
        // Now buffer entry removed.
        assert_eq!(state.pending_bank_count(), 0);
    }

    fn account_record(state: &mut CoordinatorState<String>, slot: Slot, record: &str) {
        state
            .apply(CoordinatorEvent::AccountEventSeen { slot, bank_id: 0 })
            .unwrap();
        state
            .apply(CoordinatorEvent::AccountRecordParsed {
                slot,
                bank_id: 0,
                key: AccountRecordSortKey::new(1, [slot as u8; 32]),
                record: record.to_string(),
            })
            .unwrap();
    }

    /// A fork-reported parent's own confirm follows its child's. The child's accounts wait for
    /// the parent's, or the cleanup after both drains pass the parent drops its accounts.
    #[test]
    fn account_drain_waits_for_a_fork_reported_parent() {
        let mut state = CoordinatorState::<String>::default();
        apply_ready_slot(&mut state, 41, 40, 0);
        let _ = drain_both(&mut state).unwrap();

        account_record(&mut state, 42, "parent");
        state
            .apply(CoordinatorEvent::SlotDiscarded {
                slot: 42,
                reason: DiscardReason::Forked,
                bank_ids: vec![],
            })
            .unwrap();
        account_record(&mut state, 43, "child");
        apply_ready_slot(&mut state, 43, 42, 0);
        assert!(state.drain_instruction_flushable().unwrap().is_empty());
        assert!(
            state.drain_account_flushable().is_empty(),
            "the child waits for its parent"
        );

        state
            .apply(CoordinatorEvent::BlockFrozen {
                slot: 42,
                bank_id: 0,
                metadata: metadata(41, 0),
            })
            .unwrap();
        state
            .apply(CoordinatorEvent::SlotConfirmed {
                slot: 42,
                bank_id: 0,
            })
            .unwrap();

        let instructions: Vec<Slot> = state
            .drain_instruction_flushable()
            .unwrap()
            .iter()
            .map(|ix_slot| ix_slot.slot)
            .collect();
        let accounts: Vec<(Slot, Vec<String>)> = state
            .drain_account_flushable()
            .into_iter()
            .map(|acct_slot| (acct_slot.slot, acct_slot.records))
            .collect();
        assert_eq!(instructions, [42, 43]);
        assert_eq!(accounts, [
            (42, vec!["parent".to_string()]),
            (43, vec!["child".to_string()]),
        ]);
    }

    /// Finalized mode before the first account flush: instructions run past the marker
    /// retention while slot 100 waits for its last account record. Slot 100 must keep its
    /// confirm, or the account drain stops at it for good.
    #[test]
    fn first_account_slot_keeps_its_confirm_while_instructions_run_ahead() {
        let mut state = CoordinatorState::<String>::new(AccountCommitAt::Finalized);
        state
            .apply(CoordinatorEvent::AccountEventSeen {
                slot: 100,
                bank_id: 0,
            })
            .unwrap();
        apply_ready_slot(&mut state, 100, 99, 0);
        state
            .apply(CoordinatorEvent::SlotFinalized {
                slot: 100,
                bank_id: 0,
            })
            .unwrap();

        let last = 200 + CoordinatorState::<String>::DISCARDED_SLOT_RETENTION_BEHIND_FLUSH;
        for slot in 101..=last {
            account_record(&mut state, slot, "later");
            apply_ready_slot(&mut state, slot, slot - 1, 0);
            state
                .apply(CoordinatorEvent::SlotFinalized { slot, bank_id: 0 })
                .unwrap();
            let _ = drain_both(&mut state).unwrap();
        }
        assert_eq!(state.last_instruction_flushed_slot(), Some(last));
        assert_eq!(state.last_account_flushed_slot(), None);

        state
            .apply(CoordinatorEvent::AccountRecordParsed {
                slot: 100,
                bank_id: 0,
                key: AccountRecordSortKey::new(1, [100; 32]),
                record: "late".to_string(),
            })
            .unwrap();
        let flushed = state.drain_account_flushable();
        assert_eq!(flushed.first().map(|acct_slot| acct_slot.slot), Some(100));
        assert_eq!(flushed.len() as u64, last - 99);
    }
}
