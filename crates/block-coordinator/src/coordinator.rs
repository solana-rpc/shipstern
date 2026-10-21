use std::collections::{BTreeMap, BTreeSet, HashMap};

use solana_clock::Slot;
use solana_commitment_config::CommitmentLevel;
use solana_hash::Hash;
use tokio::sync::mpsc;
use yellowstone_block_machine::{
    event::{BlockMetaEvInfo, GeyserEventAdapter, GeyserEventInfo},
    state_machine::BlockStateMachineOutput,
    wrapper::{BlockMachineConfig, BlocksStateMachineWrapper},
};
use yellowstone_grpc_proto::geyser::{subscribe_update::UpdateOneof, SubscribeUpdate};

use crate::{
    state::{CoordinatorEvent, CoordinatorState},
    types::{
        AccountCommitAt, AccountSlot, BankKey, BlockMetadata, ColorSlot, CoordinatorError,
        CoordinatorInput, CoordinatorMessage, DiscardReason, InstructionSlot,
    },
};

/// Core orchestrator that owns a `BlocksStateMachineWrapper` and delegates
/// ordering/flush decisions to the pure-ish `CoordinatorState`. Unresolved required blocks
/// stop ingestion rather than allowing either output to advance past a gap.
pub struct BlockMachineCoordinator<R> {
    wrapper: BlocksStateMachineWrapper,
    state: CoordinatorState<R>,
    input_rx: mpsc::Receiver<CoordinatorInput>,
    parsed_rx: mpsc::Receiver<CoordinatorMessage<R>>,
    instruction_output_tx: Option<mpsc::Sender<InstructionSlot<R>>>,
    account_output_tx: Option<mpsc::Sender<AccountSlot<R>>>,
    /// When false, Gate 1 is disabled by forcing expected_tx_count to 0.
    require_tx_gate: bool,
    /// Recent frozen banks and their parents, to find a slot whose BlockMeta never came.
    frozen_banks: BTreeMap<BankKey, Slot>,
    /// Slots that must still freeze or confirm, with the slot whose age bounds the wait: the
    /// slot itself for a freeze, the confirmed child for a confirm.
    pending_slots: BTreeMap<Slot, Slot>,
    /// Recent slots with records and no frozen bank, that no frozen bank names or skips yet.
    record_slots: BTreeSet<Slot>,
    /// The last slot that `expect_slot` checked, to skip repeated records.
    last_expected_slot: Option<Slot>,
    /// The slot of each recent bank, in two generations so that memory stays bounded.
    bank_slots: [HashMap<u64, Slot>; 2],
    /// Entries can arrive after BlockMeta on processed streams. Do not let the wrapper freeze
    /// a bank until its unique entries account for the metadata's expected contents.
    pending_blocks: BTreeMap<BankKey, PendingBlock>,
    /// Consecutive metadata can advance grace even after a stream jump with no complete banks.
    last_metadata_slot: Option<Slot>,
    /// Raw commitments survive the wrapper's pending-metadata queue and garbage collection.
    committed_banks: BTreeMap<Slot, u64>,
    /// Exact parent links of committed banks distinguish skipped slots from missing blocks.
    committed_parents: BTreeMap<Slot, Slot>,
}

#[cfg(test)]
mod tests {
    use yellowstone_grpc_proto::geyser::SubscribeUpdateBlockMeta;

    use super::*;

    #[test]
    fn matching_counts_cannot_hide_a_missing_entry_index() {
        let mut pending = PendingBlock {
            entry_transactions: BTreeMap::from([(1, 1), (2, 1)]),
            metadata: Some(BlockMetaEvInfo {
                slot: 42,
                bank_id: 42,
                parent_slot: 41,
                entries_count: 2,
                executed_transaction_count: 2,
                blockhash: [0; 32],
                parent_blockhash: [0; 32],
                block_time: 0,
            }),
        };
        assert!(!pending.is_complete());
        pending.entry_transactions.remove(&2);
        pending.entry_transactions.insert(0, 1);
        assert!(pending.is_complete());
    }

    #[test]
    fn missing_entries_after_a_stream_jump_fail_without_skipping() {
        let (_, input_rx) = mpsc::channel(1);
        let (_, parsed_rx) = mpsc::channel(1);
        let mut coordinator = BlockMachineCoordinator::<()>::new(
            input_rx,
            parsed_rx,
            None,
            None,
            AccountCommitAt::Confirmed,
            true,
        );
        let metadata = |slot, entries_count| SubscribeUpdate {
            update_oneof: Some(UpdateOneof::BlockMeta(SubscribeUpdateBlockMeta {
                slot,
                bank_id: slot,
                parent_slot: slot - 1,
                blockhash: Hash::default().to_string(),
                parent_blockhash: Hash::default().to_string(),
                entries_count,
                ..Default::default()
            })),
            ..Default::default()
        };
        coordinator
            .convert_geyser_update(&metadata(100, 0))
            .unwrap();
        coordinator
            .convert_geyser_update(&metadata(101, 1))
            .unwrap();
        coordinator
            .convert_geyser_update(&metadata(5000, 1))
            .unwrap();
        assert!(coordinator
            .pending_blocks
            .contains_key(&BankKey::new(101, 101)));

        let error = coordinator
            .convert_geyser_update(&metadata(5001, 1))
            .err()
            .expect("missing entries must stop ingestion");
        assert!(matches!(error, CoordinatorError::UnresolvedBlock {
            slot: 101,
            observed_slot: 5001
        }));
        assert!(coordinator
            .pending_blocks
            .contains_key(&BankKey::new(101, 101)));
        assert!(coordinator
            .pending_blocks
            .contains_key(&BankKey::new(5000, 5000)));
    }
}

#[derive(Default)]
struct PendingBlock {
    entry_transactions: BTreeMap<u64, u64>,
    metadata: Option<BlockMetaEvInfo>,
}

impl PendingBlock {
    fn is_complete(&self) -> bool {
        self.metadata.is_some_and(|meta| {
            self.entry_transactions.len() as u64 == meta.entries_count
                && self
                    .entry_transactions
                    .keys()
                    .copied()
                    .eq(0..meta.entries_count)
                && self
                    .entry_transactions
                    .values()
                    .try_fold(0_u64, |sum, count| sum.checked_add(*count))
                    == Some(meta.executed_transaction_count)
        })
    }
}

/// How far back `frozen_banks` reaches. A parent older than this is not checked.
const FROZEN_SLOT_WINDOW: Slot = 4096;

/// A parent can arrive after its child, and a confirm can lag well behind the freeze, so wait
/// this many slots before failing the run so its caller can replay without skipping data.
pub const MISSING_PARENT_GRACE_SLOTS: Slot = 300;

/// Banks per generation of `bank_slots`.
const BANK_SLOT_CAPACITY: usize = 8192;

impl<R: Send + 'static> BlockMachineCoordinator<R> {
    fn new(
        input_rx: mpsc::Receiver<CoordinatorInput>,
        parsed_rx: mpsc::Receiver<CoordinatorMessage<R>>,
        instruction_output_tx: Option<mpsc::Sender<InstructionSlot<R>>>,
        account_output_tx: Option<mpsc::Sender<AccountSlot<R>>>,
        account_commit_at: AccountCommitAt,
        require_tx_gate: bool,
    ) -> Self {
        Self {
            // Freeze on BlockMeta: the coordinator does not use footer data, and mainnet has
            // no footers before Alpenglow.
            wrapper: BlocksStateMachineWrapper::default().with_config(BlockMachineConfig {
                require_block_footer: false,
            }),
            state: CoordinatorState::new(account_commit_at),
            input_rx,
            parsed_rx,
            instruction_output_tx,
            account_output_tx,
            require_tx_gate,
            frozen_banks: BTreeMap::new(),
            pending_slots: BTreeMap::new(),
            record_slots: BTreeSet::new(),
            last_expected_slot: None,
            bank_slots: Default::default(),
            pending_blocks: BTreeMap::new(),
            last_metadata_slot: None,
            committed_banks: BTreeMap::new(),
            committed_parents: BTreeMap::new(),
        }
    }

    /// Main event loop. Any CoordinatorError terminates the coordinator.
    pub async fn run(
        input_rx: mpsc::Receiver<CoordinatorInput>,
        parsed_rx: mpsc::Receiver<CoordinatorMessage<R>>,
        instruction_output_tx: Option<mpsc::Sender<InstructionSlot<R>>>,
        account_output_tx: Option<mpsc::Sender<AccountSlot<R>>>,
        account_commit_at: AccountCommitAt,
        require_tx_gate: bool,
    ) -> Result<(), CoordinatorError> {
        Self::new(
            input_rx,
            parsed_rx,
            instruction_output_tx,
            account_output_tx,
            account_commit_at,
            require_tx_gate,
        )
        .run_inner()
        .await
    }

    async fn run_inner(mut self) -> Result<(), CoordinatorError> {
        if !self.require_tx_gate {
            tracing::info!(
                "require_tx_gate=false: tx gate disabled, transaction status stats will not be \
                 reported"
            );
        }
        loop {
            let events: Vec<CoordinatorEvent<R>> = tokio::select! {
                Some(input) = self.input_rx.recv() => {
                    match input {
                        CoordinatorInput::GeyserUpdate(update) => {
                            self.convert_geyser_update(update.as_ref())?
                        }
                        CoordinatorInput::AccountEventSeen { slot, bank_id } => {
                            vec![CoordinatorEvent::AccountEventSeen { slot, bank_id }]
                        }
                    }
                }
                Some(msg) = self.parsed_rx.recv() => {
                    let bank_id = match &msg {
                        CoordinatorMessage::InstructionParsed { bank_id, .. }
                        | CoordinatorMessage::AccountParsed { bank_id, .. }
                        | CoordinatorMessage::TransactionParsed { bank_id, .. }
                        | CoordinatorMessage::ParseStats { bank_id, .. } => *bank_id,
                    };
                    if self.state.is_invalidated_bank(BankKey::new(msg.slot(), bank_id)) {
                        Vec::new()
                    } else {
                        self.expect_slot(msg.slot());
                        Self::convert_parsed_message(msg)
                    }
                }
                else => {
                    if let Some(slot) = self.unresolved_required_block() {
                        return Err(CoordinatorError::UnresolvedBlock {
                            slot,
                            observed_slot: self.last_metadata_slot.unwrap_or(slot),
                        });
                    }
                    tracing::warn!("Coordinator channels closed, shutting down");
                    break;
                }
            };

            for mut event in events {
                self.forget_discarded_entries(&mut event);
                self.state.apply(event)?;
            }

            if let Some(ref instruction_tx) = self.instruction_output_tx {
                for ix_slot in self.state.drain_instruction_flushable()? {
                    tracing::debug!(
                        slot = %ColorSlot(ix_slot.slot),
                        tx_count = ix_slot.executed_transaction_count,
                        record_count = ix_slot.records.len(),
                        parent_slot = ix_slot.parent_slot,
                        "Flushing instruction slot"
                    );
                    instruction_tx.send(ix_slot).await.map_err(|e| {
                        CoordinatorError::InstructionOutputChannelClosed { slot: e.0.slot }
                    })?;
                }
            } else {
                // No instruction output channel — still drain to release buffer entries.
                let _ = self.state.drain_instruction_flushable()?;
            }

            if let Some(ref account_tx) = self.account_output_tx {
                for acct_slot in self.state.drain_account_flushable() {
                    tracing::debug!(
                        slot = %ColorSlot(acct_slot.slot),
                        record_count = acct_slot.records.len(),
                        decoded_account_count = acct_slot.decoded_account_count,
                        "Flushing account slot"
                    );
                    account_tx.send(acct_slot).await.map_err(|e| {
                        CoordinatorError::AccountOutputChannelClosed { slot: e.0.slot }
                    })?;
                }
            } else {
                // No account output channel — still drain to release buffer entries.
                let _ = self.state.drain_account_flushable();
            }
            if let Some(through) = self.output_frontier() {
                self.committed_banks.retain(|slot, _| *slot > through);
                self.committed_parents.retain(|slot, _| *slot > through);
            }
        }
        Ok(())
    }

    /// Boundary layer: feed updates into the wrapper, convert outputs to events.
    fn convert_geyser_update(
        &mut self,
        update: &SubscribeUpdate,
    ) -> Result<Vec<CoordinatorEvent<R>>, CoordinatorError> {
        let mut events = Vec::new();

        // The block machine's adapter panics on a malformed hash, and a block whose parent is
        // not below it can never flush. So skip such an update before the wrapper sees it.
        let malformed = match &update.update_oneof {
            Some(UpdateOneof::BlockMeta(meta))
                if meta.blockhash.parse::<Hash>().is_err() || meta.parent_slot >= meta.slot =>
            {
                Some((meta.slot, meta.bank_id))
            },
            Some(UpdateOneof::Entry(entry)) if entry.hash.len() != 32 => {
                Some((entry.slot, entry.bank_id))
            },
            _ => None,
        };
        if let Some((slot, bank_id)) = malformed {
            tracing::warn!(slot, bank_id, "Malformed BlockMeta or Entry; skipping it");
            return Ok(events);
        }

        let bank = match &update.update_oneof {
            Some(UpdateOneof::Entry(entry)) => Some((entry.slot, entry.bank_id)),
            Some(UpdateOneof::BlockMeta(meta)) => Some((meta.slot, meta.bank_id)),
            Some(UpdateOneof::EntryUpdateParent(parent)) => {
                Some((parent.slot, parent.cleared_bank_id))
            },
            Some(UpdateOneof::Slot(status)) => status.bank_id.map(|bank_id| (status.slot, bank_id)),
            _ => None,
        };
        if let Some((slot, bank_id)) = bank {
            self.check_bank_slot(slot, bank_id)?;
            if self.state.is_invalidated_bank(BankKey::new(slot, bank_id)) {
                if matches!(&update.update_oneof, Some(UpdateOneof::Slot(status)) if matches!(status.status(), yellowstone_grpc_proto::geyser::SlotStatus::SlotConfirmed | yellowstone_grpc_proto::geyser::SlotStatus::SlotFinalized))
                {
                    return Err(CoordinatorError::UnresolvedBlock {
                        slot,
                        observed_slot: self.last_metadata_slot.unwrap_or(slot),
                    });
                }
                return Ok(events);
            }
            if matches!(
                &update.update_oneof,
                Some(UpdateOneof::Entry(_) | UpdateOneof::BlockMeta(_))
            ) || matches!(&update.update_oneof, Some(UpdateOneof::Slot(status)) if status.status() == yellowstone_grpc_proto::geyser::SlotStatus::SlotCreatedBank)
            {
                self.state.note_observed_slot(slot);
            }
            if let Some(UpdateOneof::Slot(status)) = &update.update_oneof
                && matches!(
                    status.status(),
                    yellowstone_grpc_proto::geyser::SlotStatus::SlotConfirmed
                        | yellowstone_grpc_proto::geyser::SlotStatus::SlotFinalized
                )
            {
                if self
                    .committed_banks
                    .get(&slot)
                    .is_some_and(|known| *known != bank_id)
                {
                    return Err(CoordinatorError::UnresolvedBlock {
                        slot,
                        observed_slot: self.last_metadata_slot.unwrap_or(slot),
                    });
                }
                self.committed_banks.insert(slot, bank_id);
                // The wrapper may defer this status until entries arrive. Record the parent
                // now so a missing block cannot be mistaken for an unrelated fork.
                let parent = status
                    .parent
                    .or_else(|| {
                        self.pending_blocks
                            .get(&BankKey::new(slot, bank_id))
                            .and_then(|pending| pending.metadata)
                            .map(|meta| meta.parent_slot)
                    })
                    .or_else(|| self.frozen_banks.get(&BankKey::new(slot, bank_id)).copied());
                if let Some(parent) = parent {
                    self.committed_parents.insert(slot, parent);
                }
                if !self.is_confirmed_or_flushed(slot) {
                    self.pending_slots.entry(slot).or_insert(slot);
                }
            }
            if matches!(
                update.update_oneof,
                Some(UpdateOneof::Entry(_) | UpdateOneof::BlockMeta(_))
            ) && (self.state.is_bank_discarded(BankKey::new(slot, bank_id))
                || self
                    .state
                    .last_instruction_flushed_slot()
                    .is_some_and(|last| slot <= last))
            {
                return Ok(events);
            }
        }

        // UpdateParent invalidates one bank, not the slot's replacement. Block machine 0.11
        // ignores this event, so release its parsed records here and reject late callbacks.
        if let Some(UpdateOneof::EntryUpdateParent(parent)) = &update.update_oneof {
            if self.committed_banks.get(&parent.slot) == Some(&parent.cleared_bank_id) {
                return Err(CoordinatorError::UnresolvedBlock {
                    slot: parent.slot,
                    observed_slot: self.last_metadata_slot.unwrap_or(parent.slot),
                });
            }
            return Ok(vec![CoordinatorEvent::BankDiscarded {
                slot: parent.slot,
                bank_id: parent.cleared_bank_id,
                reason: DiscardReason::Forked,
            }]);
        }

        let Some(info) = <SubscribeUpdate as GeyserEventAdapter>::extract_geyser_ev_info(update)
        else {
            return Ok(events);
        };
        match info {
            GeyserEventInfo::BlockMeta(meta) => {
                if self.committed_banks.get(&meta.slot) == Some(&meta.bank_id) {
                    self.committed_parents.insert(meta.slot, meta.parent_slot);
                }
                self.expire_pending_metadata(meta.slot)?;
                let key = BankKey::new(meta.slot, meta.bank_id);
                if self.wrapper.sm.is_bank_trackable(meta.bank_id)
                    && !self.wrapper.sm.is_bank_frozen(meta.bank_id)
                {
                    self.expect_slot(meta.slot);
                    // Match the wrapper's first-metadata-wins behavior.
                    self.pending_blocks
                        .entry(key)
                        .or_default()
                        .metadata
                        .get_or_insert(meta);
                    self.release_complete_metadata(key);
                }
            },
            GeyserEventInfo::Entry(entry) => {
                // Count only entries accepted by the wrapper, using the same index-keyed
                // replacement semantics so duplicate deliveries cannot complete a bank early.
                if self
                    .wrapper
                    .handle_new_geyser_event(GeyserEventInfo::Entry(entry))
                    .is_ok()
                {
                    let key = BankKey::new(entry.slot, entry.bank_id);
                    self.pending_blocks
                        .entry(key)
                        .or_default()
                        .entry_transactions
                        .insert(entry.index, entry.executed_transaction_count);
                    self.release_complete_metadata(key);
                }
            },
            other => {
                // A rejected event does not invalidate the slot's other candidates.
                let _ = self.wrapper.handle_new_geyser_event(other);
            },
        }

        while let Some(output) = self.wrapper.pop_next_state_machine_output() {
            match output {
                BlockStateMachineOutput::FrozenBlock(frozen) => {
                    self.pending_blocks
                        .remove(&BankKey::new(frozen.slot, frozen.bank_id));
                    events.extend(self.discard_lost_parents(
                        frozen.slot,
                        frozen.bank_id,
                        frozen.parent_slot,
                    )?);

                    // The counts come from the server, so an overflow counts as a mismatch.
                    let observed_tx_count = frozen.entries.iter().try_fold(0_u64, |sum, entry| {
                        sum.checked_add(entry.executed_txn_count)
                    });

                    // The block machine freezes a bank even when entries are missing, for
                    // example when the stream starts in the middle of the slot.
                    if frozen.entries.len() as u64 != frozen.entries_count
                        || observed_tx_count != Some(frozen.executed_transaction_count)
                    {
                        tracing::warn!(
                            slot = frozen.slot,
                            bank_id = frozen.bank_id,
                            expected_entries = frozen.entries_count,
                            observed_entries = frozen.entries.len(),
                            expected_transactions = frozen.executed_transaction_count,
                            observed_transactions = ?observed_tx_count,
                            "Incomplete block; stopping before advancing past it"
                        );
                        return Err(CoordinatorError::IncompleteBlock {
                            slot: frozen.slot,
                            bank_id: frozen.bank_id,
                        });
                    }
                    let expected_tx_count = if self.require_tx_gate {
                        frozen.executed_transaction_count
                    } else {
                        0
                    };
                    let metadata = BlockMetadata {
                        parent_slot: frozen.parent_slot,
                        blockhash: frozen.blockhash,
                        expected_tx_count,
                    };
                    events.push(CoordinatorEvent::BlockFrozen {
                        slot: frozen.slot,
                        bank_id: frozen.bank_id,
                        metadata,
                    });
                },
                BlockStateMachineOutput::SlotStatus(status) => match status.commitment {
                    CommitmentLevel::Confirmed => {
                        self.note_confirm(status.slot, status.bank_id);
                        events.push(CoordinatorEvent::SlotConfirmed {
                            slot: status.slot,
                            bank_id: status.bank_id,
                        });
                    },
                    CommitmentLevel::Finalized => {
                        events.push(CoordinatorEvent::SlotFinalized {
                            slot: status.slot,
                            bank_id: status.bank_id,
                        });
                    },
                    CommitmentLevel::Processed => {},
                },
                BlockStateMachineOutput::DeadSlotDetected(dead) => {
                    self.pending_slots.remove(&dead.slot);
                    self.record_slots.remove(&dead.slot);
                    events.push(CoordinatorEvent::SlotDiscarded {
                        slot: dead.slot,
                        reason: DiscardReason::Dead,
                        bank_ids: dead.bank_ids,
                    });
                },
                BlockStateMachineOutput::ForksDetected(fork) => {
                    // A repaired ancestor can cause a fork report for a parent that a
                    // confirmed child still needs. Keep its wait, including a confirm wait
                    // installed earlier in this batch before the state sees the child.
                    if self.pending_slots.get(&fork.slot) == Some(&fork.slot)
                        && self.state.confirmed_child_of(fork.slot).is_none()
                    {
                        self.pending_slots.remove(&fork.slot);
                    }
                    self.record_slots.remove(&fork.slot);
                    events.push(CoordinatorEvent::SlotDiscarded {
                        slot: fork.slot,
                        reason: DiscardReason::Forked,
                        bank_ids: fork.bank_ids,
                    });
                },
                BlockStateMachineOutput::BankDiscarded(discarded) => {
                    if self.committed_banks.get(&discarded.slot) == Some(&discarded.bank_id) {
                        return Err(CoordinatorError::UnresolvedBlock {
                            slot: discarded.slot,
                            observed_slot: self.last_metadata_slot.unwrap_or(discarded.slot),
                        });
                    }
                    // A frozen loser can be the slot's only frozen bank, which hides a
                    // winner whose BlockMeta never comes.
                    if self
                        .frozen_banks
                        .remove(&BankKey::new(discarded.slot, discarded.bank_id))
                        .is_some()
                        && !self.has_frozen_bank(discarded.slot)
                    {
                        self.pending_slots.insert(discarded.slot, discarded.slot);
                    }
                    events.push(CoordinatorEvent::BankDiscarded {
                        slot: discarded.slot,
                        bank_id: discarded.bank_id,
                        reason: DiscardReason::Forked,
                    });
                },
            }
        }

        // Each discarded bank also comes out as BankDiscarded above. Drain the queue so it
        // does not grow.
        while self.wrapper.pop_next_dlq().is_some() {}

        Ok(events)
    }

    fn release_complete_metadata(&mut self, key: BankKey) {
        let Some(pending) = self.pending_blocks.get_mut(&key) else {
            return;
        };
        if pending.is_complete() {
            let meta = pending.metadata.take().expect("complete bank has metadata");
            if self
                .wrapper
                .handle_new_geyser_event(GeyserEventInfo::BlockMeta(meta))
                .is_err()
            {
                self.pending_blocks.remove(&key);
            }
        }
    }

    /// A timeout is evidence of missing data, never permission to skip a block.
    fn expire_pending_metadata(&mut self, slot: Slot) -> Result<(), CoordinatorError> {
        let previous = self.last_metadata_slot.replace(slot);
        // Like a first frozen bank after a jump, one distant metadata event must not age out
        // existing waits. Subsequent nearby slots establish progress even if entries are lost
        // for every bank, so the pending metadata cannot grow without bound.
        if self.is_far_ahead(slot)
            && !previous
                .is_some_and(|last| slot > last && slot <= last.saturating_add(FROZEN_SLOT_WINDOW))
        {
            return Ok(());
        }
        let cutoff = slot.saturating_sub(MISSING_PARENT_GRACE_SLOTS);
        if self.committed_banks.keys().any(|committed| {
            *committed < cutoff
                && self
                    .state
                    .first_observed_slot()
                    .is_none_or(|first| *committed >= first)
                && self.output_frontier().is_none_or(|last| *committed > last)
        }) && let Some(missing) = self.unresolved_required_block()
        {
            return Err(CoordinatorError::UnresolvedBlock {
                slot: missing,
                observed_slot: slot,
            });
        }
        let expired: Vec<_> = self
            .pending_blocks
            .range(..BankKey::first(cutoff))
            .filter_map(|(&key, pending)| pending.metadata.map(|meta| (key, meta)))
            .collect();
        for (key, _) in expired {
            if self.is_proven_skipped(key.slot)
                || self
                    .committed_banks
                    .get(&key.slot)
                    .is_some_and(|bank| *bank != key.bank_id)
            {
                self.pending_blocks.remove(&key);
            } else {
                return Err(CoordinatorError::UnresolvedBlock {
                    slot: key.slot,
                    observed_slot: slot,
                });
            }
        }
        Ok(())
    }

    fn is_proven_skipped(&self, slot: Slot) -> bool {
        self.committed_parents
            .range(slot.saturating_add(1)..)
            .any(|(_, &parent)| parent < slot)
    }

    fn unresolved_required_block(&self) -> Option<Slot> {
        let frontier = self.output_frontier();
        let start = self.state.first_observed_slot();
        self.committed_parents
            .values()
            .chain(self.committed_banks.keys())
            .copied()
            .filter(|slot| {
                start.is_none_or(|first| *slot >= first) && frontier.is_none_or(|last| *slot > last)
            })
            .min()
    }

    fn output_frontier(&self) -> Option<Slot> {
        match (
            self.instruction_output_tx.is_some(),
            self.account_output_tx.is_some(),
        ) {
            (true, true) => self
                .state
                .last_instruction_flushed_slot()
                .zip(self.state.last_account_flushed_slot())
                .map(|(instructions, accounts)| instructions.min(accounts)),
            (false, true) => self.state.last_account_flushed_slot(),
            _ => self.state.last_instruction_flushed_slot(),
        }
    }

    fn forget_discarded_entries(&mut self, event: &mut CoordinatorEvent<R>) {
        match event {
            CoordinatorEvent::BankDiscarded { slot, bank_id, .. } => {
                self.pending_blocks.remove(&BankKey::new(*slot, *bank_id));
            },
            CoordinatorEvent::SlotDiscarded {
                slot,
                reason: DiscardReason::Dead | DiscardReason::Incomplete,
                bank_ids,
            } => {
                // A metadata-first bank may not be registered in the wrapper yet. Include it
                // in the tombstones so a late entry cannot bring a dead bank back.
                bank_ids.extend(
                    self.pending_blocks
                        .range(BankKey::slot_range(*slot))
                        .map(|(key, _)| key.bank_id),
                );
                self.pending_blocks.retain(|key, _| key.slot != *slot);
            },
            _ => {},
        }
    }

    /// Bank IDs are per node, and one stream comes from one node, so a bank is in one slot.
    /// Block machine 0.11 asserts this and panics, so stop with an error before it sees it.
    fn check_bank_slot(&mut self, slot: Slot, bank_id: u64) -> Result<(), CoordinatorError> {
        let [current, previous] = &mut self.bank_slots;
        if let Some(&other_slot) = current.get(&bank_id).or_else(|| previous.get(&bank_id))
            && other_slot != slot
        {
            // Agave gives bank ID 0 only to its first bank. A repeated 0 is a missing field.
            if bank_id == 0 {
                return Err(CoordinatorError::MissingBankId { slot });
            }
            return Err(CoordinatorError::BankInTwoSlots {
                bank_id,
                slot,
                other_slot,
            });
        }
        if current.len() >= BANK_SLOT_CAPACITY {
            *previous = std::mem::take(current);
        }
        current.insert(bank_id, slot);
        Ok(())
    }

    /// A record for a slot that the chain skips would block every later slot. Note the slot, so
    /// that `discard_lost_parents` can treat it like a parent that never froze.
    fn expect_slot(&mut self, slot: Slot) {
        self.state.note_observed_slot(slot);
        if self.last_expected_slot.replace(slot) == Some(slot)
            || self
                .state
                .last_flushed_slot()
                .is_some_and(|last| slot <= last)
            || self.state.is_slot_discarded(slot)
            || self.is_far_ahead(slot)
            || self.has_frozen_bank(slot)
        {
            return;
        }
        // A frozen bank above can already name or skip the slot. No later edge reaches a slot
        // below every frozen bank.
        if self
            .frozen_banks
            .first_key_value()
            .is_some_and(|(low, _)| slot < low.slot)
            || self
                .frozen_banks
                .range(BankKey::first(slot.saturating_add(1))..)
                .any(|(_, &parent)| parent <= slot)
        {
            self.pending_slots.entry(slot).or_insert(slot);
            return;
        }
        self.record_slots.insert(slot);
    }

    /// A confirmed bank puts its parent on the chain. A frozen parent whose confirm never comes
    /// would block every later slot, so note it for `discard_lost_parents`. The state applies
    /// this batch later, so `discard_lost_parents` checks the confirm again at the deadline.
    fn note_confirm(&mut self, slot: Slot, bank_id: u64) {
        self.pending_slots.remove(&slot);
        if let Some(&parent) = self.frozen_banks.get(&BankKey::new(slot, bank_id))
            && self.has_frozen_bank(parent)
            && !self.is_confirmed_or_flushed(parent)
        {
            self.pending_slots.entry(parent).or_insert(slot);
        }
    }

    /// The state prunes old confirms, so a slot at or behind the instruction frontier counts too.
    fn is_confirmed_or_flushed(&self, slot: Slot) -> bool {
        self.state.is_slot_confirmed(slot)
            || self
                .state
                .last_instruction_flushed_slot()
                .is_some_and(|last| slot <= last)
    }

    /// Block machine 0.11 never freezes a bank whose BlockMeta is lost, and the child then
    /// waits on it. Fail once the stream is well past an unresolved block; only committed
    /// parent links may prove that a slot was skipped rather than lost.
    fn discard_lost_parents(
        &mut self,
        slot: Slot,
        bank_id: u64,
        parent: Slot,
    ) -> Result<Vec<CoordinatorEvent<R>>, CoordinatorError> {
        // A slot this far past every frozen bank is bogus, or the first after a jump. Either way
        // it is recorded, so a lost parent of its own is still found, but it must not age out
        // the slots in grace or prune the window.
        let far_ahead = self.is_far_ahead(slot);
        if self
            .frozen_banks
            .first_key_value()
            .is_some_and(|(low, _)| parent >= low.slot)
            && !self.has_frozen_bank(parent)
        {
            self.pending_slots.entry(parent).or_insert(parent);
        }

        self.frozen_banks
            .insert(BankKey::new(slot, bank_id), parent);
        // Only a wait for a freeze ends here; a wait for a confirm outlives a second bank's freeze.
        if self.pending_slots.get(&slot) == Some(&slot) {
            self.pending_slots.remove(&slot);
        }
        // A child may have confirmed while this parent still lacked metadata. Freezing
        // ends that wait, but the winning parent bank must still confirm before it flushes.
        if !self.is_confirmed_or_flushed(slot)
            && let Some(child) = self.state.confirmed_child_of(slot)
        {
            self.pending_slots.entry(slot).or_insert(child);
        }
        self.record_slots.remove(&slot);

        // A slot with records must freeze if it is below every frozen bank, or if this parent
        // edge names or skips it.
        let low = self
            .frozen_banks
            .first_key_value()
            .map_or(slot, |(low, _)| low.slot);
        while let Some(&passed) = self
            .record_slots
            .range(..low)
            .chain(self.record_slots.range(parent..slot))
            .next()
        {
            self.record_slots.remove(&passed);
            self.pending_slots.entry(passed).or_insert(passed);
        }
        if far_ahead {
            return Ok(Vec::new());
        }

        let window_start = slot.saturating_sub(FROZEN_SLOT_WINDOW);
        self.frozen_banks = self.frozen_banks.split_off(&BankKey::first(window_start));
        self.record_slots = self.record_slots.split_off(&window_start);
        self.pending_blocks = self.pending_blocks.split_off(&BankKey::first(window_start));

        let cutoff = slot.saturating_sub(MISSING_PARENT_GRACE_SLOTS);
        let due: Vec<Slot> = self
            .pending_slots
            .iter()
            .filter(|(_, bound)| **bound < cutoff)
            .map(|(&pending, _)| pending)
            .collect();
        let mut lost = Vec::new();
        for pending in due {
            let bound = self.pending_slots.remove(&pending).unwrap_or(pending);
            // A confirm that the state applied after `note_confirm` ran ends the wait for it.
            if bound == pending || !self.is_confirmed_or_flushed(pending) {
                if self
                    .state
                    .last_instruction_flushed_slot()
                    .is_some_and(|last| pending <= last)
                {
                    continue;
                }
                if !self.is_proven_skipped(pending) {
                    return Err(CoordinatorError::UnresolvedBlock {
                        slot: pending,
                        observed_slot: slot,
                    });
                }
                lost.push(pending);
            }
        }
        Ok(lost
            .into_iter()
            .map(|lost_slot| {
                tracing::warn!(
                    slot,
                    lost_slot,
                    "Committed parent link proves slot was skipped"
                );
                CoordinatorEvent::SlotDiscarded {
                    slot: lost_slot,
                    reason: DiscardReason::Incomplete,
                    bank_ids: Vec::new(),
                }
            })
            .collect())
    }

    /// A slot this far past every frozen bank is bogus. It must not age out the slots in grace.
    fn is_far_ahead(&self, slot: Slot) -> bool {
        self.frozen_banks
            .last_key_value()
            .is_some_and(|(high, _)| slot > high.slot.saturating_add(FROZEN_SLOT_WINDOW))
    }

    fn has_frozen_bank(&self, slot: Slot) -> bool {
        self.frozen_banks
            .range(BankKey::slot_range(slot))
            .next()
            .is_some()
    }

    fn convert_parsed_message(msg: CoordinatorMessage<R>) -> Vec<CoordinatorEvent<R>> {
        match msg {
            CoordinatorMessage::InstructionParsed {
                slot,
                bank_id,
                key,
                record,
            } => vec![CoordinatorEvent::InstructionRecordParsed {
                slot,
                bank_id,
                key,
                record,
            }],
            CoordinatorMessage::AccountParsed {
                slot,
                bank_id,
                key,
                record,
            } => vec![CoordinatorEvent::AccountRecordParsed {
                slot,
                bank_id,
                key,
                record,
            }],
            CoordinatorMessage::TransactionParsed { slot, bank_id } => {
                vec![CoordinatorEvent::TransactionParsed { slot, bank_id }]
            },
            CoordinatorMessage::ParseStats {
                slot,
                bank_id,
                kind,
            } => vec![CoordinatorEvent::ParseStats {
                slot,
                bank_id,
                kind,
            }],
        }
    }
}
