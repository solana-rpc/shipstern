use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use solana_clock::Slot;
use solana_commitment_config::CommitmentLevel;
use solana_hash::Hash;
use tokio::sync::mpsc;
use yellowstone_block_machine::{
    event::{EntryEvInfo, GeyserEventAdapter, GeyserEventInfo, SlotStatusKind, SlotUpdateEvInfo},
    state_machine::BlockStateMachineOutput,
    wrapper::{BlockMachineConfig, BlocksStateMachineWrapper},
};
use yellowstone_grpc_proto::geyser::{subscribe_update::UpdateOneof, SubscribeUpdate};

use crate::{
    commitments::{Commitments, Forward},
    state::{CoordinatorEvent, CoordinatorState},
    types::{
        AccountCommitAt, AccountSlot, BankKey, BlockMetadata, ColorSlot, CoordinatorError,
        CoordinatorInput, CoordinatorMessage, DiscardReason, InstructionSlot,
    },
};

///
/// Core orchestrator that owns a `BlocksStateMachineWrapper` and delegates ordering and flush
/// decisions to the pure-ish `CoordinatorState`.
///
/// A slot that never resolves, such as one whose BlockMeta was lost, holds back every later slot.
/// Rather than guess which records to drop, the coordinator stops with `TooManyPendingBanks` once
/// more banks wait than the limit allows, so the caller restarts from its checkpoint and replays.
///
pub struct BlockMachineCoordinator<R> {
    wrapper: BlocksStateMachineWrapper,
    state: CoordinatorState<R>,
    input_rx: mpsc::Receiver<CoordinatorInput>,
    parsed_rx: mpsc::Receiver<CoordinatorMessage<R>>,
    instruction_output_tx: Option<mpsc::Sender<InstructionSlot<R>>>,
    account_output_tx: Option<mpsc::Sender<AccountSlot<R>>>,
    /// When false, Gate 1 is disabled by forcing expected_tx_count to 0.
    require_tx_gate: bool,
    /// Stop once more banks than this wait to flush.
    max_pending_banks: usize,
    /// The slot of each recent bank, in two generations so that memory stays bounded.
    bank_slots: [HashMap<u64, Slot>; 2],
    /// Banks that froze before all their entries arrived.
    short_banks: BTreeMap<BankKey, ShortBank<R>>,
    /// Banks whose CreatedBank came before anything flushed, so the stream saw them from the start.
    created_at_startup: HashSet<BankKey>,
    /// Which bank of each slot is committed, by its own status or a child's parent blockhash.
    commitments: Commitments,
}

///
/// A bank that froze short of entries. LaserStream can send a bank's last entry after its
/// BlockMeta, and block machine 0.11 freezes on BlockMeta and drops that entry, so the
/// coordinator counts late entries itself. The bank freezes in the state as usual, but its
/// commitment statuses wait here until the counts match, so it cannot flush. It counts as a
/// pending bank, so an entry that never comes ends in `TooManyPendingBanks` rather than a gap.
///
struct ShortBank<R> {
    entries_count: u64,
    entry_indexes: BTreeSet<u64>,
    missing_transactions: u64,
    statuses: Vec<CoordinatorEvent<R>>,
}

/// About 25 seconds of 400 ms slots behind a slot that never resolves. Accounts committed at
/// finalized trail confirmation by about 32 slots on TowerBFT, so such a consumer may want more.
pub const DEFAULT_MAX_PENDING_BANKS: usize = 64;

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
        max_pending_banks: usize,
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
            max_pending_banks,
            bank_slots: Default::default(),
            short_banks: BTreeMap::new(),
            created_at_startup: HashSet::new(),
            commitments: Commitments::default(),
        }
    }

    /// Main event loop. Any CoordinatorError terminates the coordinator. Pass
    /// `DEFAULT_MAX_PENDING_BANKS` unless accounts commit at finalized on a slow-finality cluster.
    pub async fn run(
        input_rx: mpsc::Receiver<CoordinatorInput>,
        parsed_rx: mpsc::Receiver<CoordinatorMessage<R>>,
        instruction_output_tx: Option<mpsc::Sender<InstructionSlot<R>>>,
        account_output_tx: Option<mpsc::Sender<AccountSlot<R>>>,
        account_commit_at: AccountCommitAt,
        require_tx_gate: bool,
        max_pending_banks: usize,
    ) -> Result<(), CoordinatorError> {
        Self::new(
            input_rx,
            parsed_rx,
            instruction_output_tx,
            account_output_tx,
            account_commit_at,
            require_tx_gate,
            max_pending_banks,
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
                            self.state.apply(CoordinatorEvent::AccountEventSeen { slot, bank_id })?;
                            Vec::new()
                        }
                    }
                }
                // The state drops a parsed message for a discarded bank on its own.
                Some(msg) = self.parsed_rx.recv() => {
                    self.state.apply(Self::convert_parsed_message(msg))?;
                    Vec::new()
                }
                else => {
                    tracing::warn!("Coordinator channels closed, shutting down");
                    break;
                }
            };

            for event in events {
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

            // The state drops a bank's buffer when the bank can no longer flush.
            self.short_banks.retain(|&key, _| self.state.has_bank(key));
            if let Some(through) = self.state.flushed_through() {
                self.commitments.prune_through(through);
            }
            if self.state.last_flushed_slot().is_some() {
                self.created_at_startup = HashSet::new();
            }

            let (unflushed, oldest_unflushed) = self
                .commitments
                .unflushed(self.state.flushed_through(), |key| {
                    self.state.is_invalidated_bank(key)
                });
            let pending = self.state.pending_bank_count().max(unflushed);
            if pending > self.max_pending_banks {
                return Err(CoordinatorError::TooManyPendingBanks {
                    pending,
                    limit: self.max_pending_banks,
                    oldest_slot: self
                        .state
                        .oldest_pending_slot()
                        .into_iter()
                        .chain(oldest_unflushed)
                        .min(),
                });
            }
        }
        Ok(())
    }

    /// Boundary layer: feed updates into the wrapper, convert outputs to events.
    fn convert_geyser_update(
        &mut self,
        update: &SubscribeUpdate,
    ) -> Result<Vec<CoordinatorEvent<R>>, CoordinatorError> {
        let events = Vec::new();

        // The block machine's adapter panics on a malformed hash, and a block whose parent is
        // not below it can never flush. So skip such an update before the wrapper sees it.
        let malformed = match &update.update_oneof {
            Some(UpdateOneof::BlockMeta(meta))
                if meta.blockhash.parse::<Hash>().is_err()
                    || meta.parent_blockhash.parse::<Hash>().is_err()
                    || meta.parent_slot >= meta.slot =>
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

        if let Some(UpdateOneof::EntryUpdateParent(parent)) = &update.update_oneof {
            let mut events = self.clear_bank(parent.slot, parent.cleared_bank_id)?;
            // The cleared bank no longer hides its replacement from a child's parent blockhash.
            self.forward_commitments()?;
            events.extend(self.drain_wrapper_outputs(None)?);
            return Ok(events);
        }

        let Some(info) = <SubscribeUpdate as GeyserEventAdapter>::extract_geyser_ev_info(update)
        else {
            return Ok(events);
        };
        if let Some(bank_id) = info.bank_id() {
            let slot = info.slot();
            self.check_bank_slot(slot, bank_id)?;
            // A late event must not revive a cleared, dead or losing bank in the wrapper, and the
            // cluster cannot commit one.
            if self.state.is_invalidated_bank(BankKey::new(slot, bank_id)) {
                if matches!(
                    &info,
                    GeyserEventInfo::Slot(status)
                        if matches!(status.status, SlotStatusKind::Confirmed | SlotStatusKind::Finalized)
                ) {
                    return Err(CoordinatorError::ConflictingBlockIdentity { slot, bank_id });
                }
                tracing::debug!(slot, bank_id, "Event for an invalidated bank; dropping it");
                return Ok(events);
            }
        }
        // The stream saw a block if it saw the block start or freeze. A commitment can name an
        // older slot, and a late entry can trail a BlockMeta from before the stream.
        let shows_block = match &info {
            GeyserEventInfo::Slot(status) => matches!(status.status, SlotStatusKind::CreatedBank),
            GeyserEventInfo::BlockMeta(_) => true,
            _ => false,
        };
        if shows_block {
            self.state.note_observed_slot(info.slot());
        }
        if let GeyserEventInfo::Slot(status) = &info
            && let (SlotStatusKind::CreatedBank, Some(bank_id)) = (status.status, status.bank_id)
            && self.state.last_flushed_slot().is_none()
            && self.created_at_startup.len() < BANK_SLOT_CAPACITY
        {
            self.created_at_startup
                .insert(BankKey::new(status.slot, bank_id));
        }
        // The wrapper drops an entry for a frozen bank, so count a short bank's late entry here.
        if let GeyserEventInfo::Entry(entry) = &info
            && self
                .short_banks
                .contains_key(&BankKey::new(entry.slot, entry.bank_id))
        {
            return Ok(self.add_late_entry(entry));
        }
        let dead_slot = matches!(
            &info,
            GeyserEventInfo::Slot(status) if matches!(status.status, SlotStatusKind::Dead)
        )
        .then(|| info.slot());
        // A commitment reaches the block machine only through `forward_commitments`.
        let commitment = match &info {
            GeyserEventInfo::Slot(status) => match status.status {
                SlotStatusKind::Confirmed => Some(CommitmentLevel::Confirmed),
                SlotStatusKind::Finalized => Some(CommitmentLevel::Finalized),
                _ => None,
            }
            .zip(status.bank_id)
            .map(|(level, bank_id)| (BankKey::new(status.slot, bank_id), level, status.parent)),
            _ => None,
        };
        let is_block_meta = matches!(info, GeyserEventInfo::BlockMeta(_));
        if let Some((key, level, parent_slot)) = commitment {
            self.commitments.record(key, level, parent_slot)?;
        } else {
            if let GeyserEventInfo::BlockMeta(meta) = &info {
                self.commitments.note_identity(meta)?;
            }
            // The wrapper rejects only an event for a discarded or already frozen bank, or a
            // commitment with no bank ID. None removes the slot, so do not discard its other banks.
            if self.wrapper.handle_new_geyser_event(info).is_err() {
                return Ok(events);
            }
        }
        if commitment.is_some() || is_block_meta {
            self.forward_commitments()?;
        }
        self.drain_wrapper_outputs(dead_slot)
    }

    /// Pass the verified commitments the block machine has not had yet.
    fn forward_commitments(&mut self) -> Result<(), CoordinatorError> {
        let forwards = self.commitments.reconcile(
            self.state.flushed_through(),
            |slot| self.state.is_before_stream(slot),
            |key| self.state.is_invalidated_bank(key),
        )?;
        for Forward {
            key,
            parent_slot,
            status,
        } in forwards
        {
            self.wrapper
                .handle_new_geyser_event(GeyserEventInfo::Slot(SlotUpdateEvInfo {
                    slot: key.slot,
                    parent: parent_slot,
                    status,
                    dead_error: false,
                    bank_id: Some(key.bank_id),
                }))
                .map_err(|_| CoordinatorError::ConflictingBlockIdentity {
                    slot: key.slot,
                    bank_id: key.bank_id,
                })?;
        }
        Ok(())
    }

    /// Convert the wrapper's outputs to events. `dead_slot` is the slot of a Dead status just fed.
    fn drain_wrapper_outputs(
        &mut self,
        dead_slot: Option<Slot>,
    ) -> Result<Vec<CoordinatorEvent<R>>, CoordinatorError> {
        let mut events = Vec::new();
        let mut dead_slot_reported = false;
        while let Some(output) = self.wrapper.pop_next_state_machine_output() {
            match output {
                BlockStateMachineOutput::FrozenBlock(frozen) => {
                    let key = BankKey::new(frozen.slot, frozen.bank_id);
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
                    // The counts come from the server, so an overflow counts as a mismatch.
                    let observed_tx_count = frozen.entries.iter().try_fold(0_u64, |sum, entry| {
                        sum.checked_add(entry.executed_txn_count)
                    });
                    let frozen_event = CoordinatorEvent::BlockFrozen {
                        slot: frozen.slot,
                        bank_id: frozen.bank_id,
                        metadata,
                    };
                    // The block machine keeps one entry per index.
                    if frozen.entries.len() as u64 == frozen.entries_count
                        && observed_tx_count == Some(frozen.executed_transaction_count)
                    {
                        events.push(frozen_event);
                        continue;
                    }

                    // The block machine freezes a bank even when entries are missing. Only missing
                    // entries can still arrive, so a surplus is final. Policy for a bank that was
                    // in flight when the stream started: before anything flushes, a bank with
                    // neither its CreatedBank nor its first entry seen is taken to have started
                    // before the stream and is discarded, or its child would wait for it forever.
                    let entry_indexes: BTreeSet<u64> = frozen
                        .entries
                        .iter()
                        .map(|entry| entry.entry_index)
                        .collect();
                    let started_mid_slot = self.state.last_flushed_slot().is_none()
                        && !self.created_at_startup.contains(&key)
                        && !entry_indexes.contains(&0);
                    let can_complete = !started_mid_slot
                        && (entry_indexes.len() as u64) < frozen.entries_count
                        && entry_indexes
                            .last()
                            .is_none_or(|&last| last < frozen.entries_count);
                    let Some(missing_transactions) = observed_tx_count
                        .and_then(|observed| {
                            frozen.executed_transaction_count.checked_sub(observed)
                        })
                        .filter(|_| can_complete)
                    else {
                        tracing::warn!(
                            slot = frozen.slot,
                            bank_id = frozen.bank_id,
                            expected_entries = frozen.entries_count,
                            observed_entries = entry_indexes.len(),
                            expected_transactions = frozen.executed_transaction_count,
                            observed_transactions = ?observed_tx_count,
                            started_mid_slot,
                            "Discarding incomplete block"
                        );
                        events.push(CoordinatorEvent::BankDiscarded {
                            slot: frozen.slot,
                            bank_id: frozen.bank_id,
                            reason: DiscardReason::Incomplete,
                        });
                        continue;
                    };
                    tracing::debug!(
                        slot = frozen.slot,
                        bank_id = frozen.bank_id,
                        expected_entries = frozen.entries_count,
                        observed_entries = entry_indexes.len(),
                        missing_transactions,
                        "Block froze short of entries; waiting for late entries"
                    );
                    // The run loop drops the short bank if the state refuses the freeze.
                    events.push(frozen_event);
                    self.short_banks.insert(key, ShortBank {
                        entries_count: frozen.entries_count,
                        entry_indexes,
                        missing_transactions,
                        statuses: Vec::new(),
                    });
                },
                BlockStateMachineOutput::SlotStatus(status) => {
                    let key = BankKey::new(status.slot, status.bank_id);
                    // The block machine's own guess for an ancestor is not verified.
                    if !self.commitments.allows(key, status.commitment) {
                        tracing::debug!(
                            slot = status.slot,
                            bank_id = status.bank_id,
                            commitment = ?status.commitment,
                            "Dropping a commitment the block machine inferred"
                        );
                        continue;
                    }
                    let event = match status.commitment {
                        CommitmentLevel::Confirmed => CoordinatorEvent::SlotConfirmed {
                            slot: status.slot,
                            bank_id: status.bank_id,
                        },
                        CommitmentLevel::Finalized => CoordinatorEvent::SlotFinalized {
                            slot: status.slot,
                            bank_id: status.bank_id,
                        },
                        CommitmentLevel::Processed => continue,
                    };
                    match self.short_banks.get_mut(&key) {
                        Some(short) => short.statuses.push(event),
                        None => events.push(event),
                    }
                },
                BlockStateMachineOutput::DeadSlotDetected(dead) => {
                    dead_slot_reported |= dead_slot == Some(dead.slot);
                    events.push(CoordinatorEvent::SlotDiscarded {
                        slot: dead.slot,
                        reason: DiscardReason::Dead,
                        bank_ids: dead.bank_ids,
                    });
                },
                BlockStateMachineOutput::ForksDetected(fork) => {
                    events.push(CoordinatorEvent::SlotDiscarded {
                        slot: fork.slot,
                        reason: DiscardReason::Forked,
                        bank_ids: fork.bank_ids,
                    });
                },
                BlockStateMachineOutput::BankDiscarded(discarded) => {
                    if self
                        .commitments
                        .contains(BankKey::new(discarded.slot, discarded.bank_id))
                    {
                        return Err(CoordinatorError::ConflictingBlockIdentity {
                            slot: discarded.slot,
                            bank_id: discarded.bank_id,
                        });
                    }
                    events.push(CoordinatorEvent::BankDiscarded {
                        slot: discarded.slot,
                        bank_id: discarded.bank_id,
                        reason: DiscardReason::Forked,
                    });
                },
            }
        }

        // Block machine 0.11 reports a slot's death only once. A later death, of a repair bank,
        // still discards the slot's banks inside it, so discard them here too, or they stay pending.
        // Drop only the banks: marking the slot dead again would let a child flush past the next
        // repair before its BlockMeta.
        if let Some(slot) = dead_slot
            && !dead_slot_reported
        {
            let dropped: Vec<BankKey> = self
                .state
                .banks_of(slot)
                .into_iter()
                .filter(|key| !self.wrapper.sm.is_bank_trackable(key.bank_id))
                .collect();
            events.extend(
                dropped
                    .into_iter()
                    .map(|key| CoordinatorEvent::BankDiscarded {
                        slot,
                        bank_id: key.bank_id,
                        reason: DiscardReason::Dead,
                    }),
            );
        }

        // Each discarded bank also comes out as BankDiscarded above. Drain the queue so it
        // does not grow.
        while self.wrapper.pop_next_dlq().is_some() {}

        Ok(events)
    }

    /// Count a short bank's late entry, and freeze the bank once its counts match.
    fn add_late_entry(&mut self, entry: &EntryEvInfo) -> Vec<CoordinatorEvent<R>> {
        let key = BankKey::new(entry.slot, entry.bank_id);
        let Some(short) = self.short_banks.get_mut(&key) else {
            return Vec::new();
        };
        if entry.index >= short.entries_count || !short.entry_indexes.insert(entry.index) {
            tracing::debug!(
                slot = entry.slot,
                bank_id = entry.bank_id,
                index = entry.index,
                "Repeated or out-of-range late entry; dropping it"
            );
            return Vec::new();
        }
        let Some(missing_transactions) = short
            .missing_transactions
            .checked_sub(entry.executed_transaction_count)
        else {
            return self.release_short_bank(key, false);
        };
        short.missing_transactions = missing_transactions;
        if (short.entry_indexes.len() as u64) < short.entries_count {
            return Vec::new();
        }
        let complete = missing_transactions == 0;
        self.release_short_bank(key, complete)
    }

    /// Release a short bank's held statuses, after discarding it as incomplete unless its counts
    /// match. A confirm of an incomplete bank discards its slot, as when the bank froze incomplete.
    fn release_short_bank(&mut self, key: BankKey, complete: bool) -> Vec<CoordinatorEvent<R>> {
        let Some(short) = self.short_banks.remove(&key) else {
            return Vec::new();
        };
        let BankKey { slot, bank_id } = key;
        // A dead or forked bank must not confirm.
        if self.state.is_invalidated_bank(key) {
            return Vec::new();
        }
        if complete {
            tracing::debug!(slot, bank_id, "Late entries completed the block");
            return short.statuses;
        }
        tracing::warn!(
            slot,
            bank_id,
            expected_entries = short.entries_count,
            observed_entries = short.entry_indexes.len(),
            missing_transactions = short.missing_transactions,
            "Discarding incomplete block"
        );
        let discarded = CoordinatorEvent::BankDiscarded {
            slot,
            bank_id,
            reason: DiscardReason::Incomplete,
        };
        std::iter::once(discarded).chain(short.statuses).collect()
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

    ///
    /// UpdateParent abandons one bank and replays its slot under a new bank ID. Block machine 0.11
    /// ignores the event, so release the bank's records here; its late events are rejected after.
    /// Agave clears only a bank still replaying, so clearing one that froze or confirmed stops the
    /// coordinator instead of stranding the bank's children.
    ///
    fn clear_bank(
        &mut self,
        slot: Slot,
        bank_id: u64,
    ) -> Result<Vec<CoordinatorEvent<R>>, CoordinatorError> {
        self.check_bank_slot(slot, bank_id)?;
        let key = BankKey::new(slot, bank_id);
        if self.state.is_invalidated_bank(key) {
            return Ok(Vec::new());
        }
        if self.commitments.contains(key) || self.state.is_bank_frozen_or_confirmed(key) {
            return Err(CoordinatorError::ClearedCommittedBank { slot, bank_id });
        }
        self.commitments.forget(key);
        Ok(vec![CoordinatorEvent::BankDiscarded {
            slot,
            bank_id,
            reason: DiscardReason::Cleared,
        }])
    }

    fn convert_parsed_message(msg: CoordinatorMessage<R>) -> CoordinatorEvent<R> {
        match msg {
            CoordinatorMessage::InstructionParsed {
                slot,
                bank_id,
                key,
                record,
            } => CoordinatorEvent::InstructionRecordParsed {
                slot,
                bank_id,
                key,
                record,
            },
            CoordinatorMessage::AccountParsed {
                slot,
                bank_id,
                key,
                record,
            } => CoordinatorEvent::AccountRecordParsed {
                slot,
                bank_id,
                key,
                record,
            },
            CoordinatorMessage::TransactionParsed { slot, bank_id } => {
                CoordinatorEvent::TransactionParsed { slot, bank_id }
            },
            CoordinatorMessage::ParseStats {
                slot,
                bank_id,
                kind,
            } => CoordinatorEvent::ParseStats {
                slot,
                bank_id,
                kind,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use yellowstone_grpc_proto::geyser::SubscribeUpdateEntryUpdateParent;

    use super::*;
    use crate::types::InstructionRecordSortKey;

    fn coordinator() -> BlockMachineCoordinator<String> {
        let (_input_tx, input_rx) = mpsc::channel(1);
        let (_parsed_tx, parsed_rx) = mpsc::channel(1);
        BlockMachineCoordinator::new(
            input_rx,
            parsed_rx,
            None,
            None,
            AccountCommitAt::Confirmed,
            true,
            DEFAULT_MAX_PENDING_BANKS,
        )
    }

    fn feed(
        coordinator: &mut BlockMachineCoordinator<String>,
        update: SubscribeUpdate,
    ) -> Result<(), CoordinatorError> {
        for event in coordinator.convert_geyser_update(&update)? {
            coordinator.state.apply(event)?;
        }
        Ok(())
    }

    fn clear(slot: Slot, cleared_bank_id: u64) -> SubscribeUpdate {
        SubscribeUpdate {
            update_oneof: Some(UpdateOneof::EntryUpdateParent(
                SubscribeUpdateEntryUpdateParent {
                    slot,
                    cleared_bank_id,
                    parent_slot: slot - 2,
                    ..Default::default()
                },
            )),
            ..Default::default()
        }
    }

    fn record(coordinator: &mut BlockMachineCoordinator<String>, slot: Slot, bank_id: u64) {
        coordinator
            .state
            .apply(BlockMachineCoordinator::convert_parsed_message(
                CoordinatorMessage::InstructionParsed {
                    slot,
                    bank_id,
                    key: InstructionRecordSortKey::new(0, vec![0]),
                    record: format!("{slot}/{bank_id}"),
                },
            ))
            .unwrap();
    }

    /// The real order: records of the abandoned bank, then the clear, before any replacement.
    #[test]
    fn update_parent_releases_the_cleared_banks_records_at_once() {
        let mut coordinator = coordinator();
        record(&mut coordinator, 42, 7);
        assert_eq!(coordinator.state.pending_bank_count(), 1);

        feed(&mut coordinator, clear(42, 7)).unwrap();
        assert_eq!(coordinator.state.pending_bank_count(), 0);

        // A late record or a repeated clear cannot bring the bank back.
        record(&mut coordinator, 42, 7);
        feed(&mut coordinator, clear(42, 7)).unwrap();
        assert_eq!(coordinator.state.pending_bank_count(), 0);
    }

    /// Agave clears only a bank still replaying. Clearing one that froze stops the coordinator
    /// instead of stranding its children.
    #[test]
    fn update_parent_for_a_frozen_bank_stops_the_coordinator() {
        let mut coordinator = coordinator();
        coordinator
            .state
            .apply(CoordinatorEvent::BlockFrozen {
                slot: 42,
                bank_id: 7,
                metadata: BlockMetadata {
                    parent_slot: 41,
                    blockhash: Hash::new_unique(),
                    expected_tx_count: 0,
                },
            })
            .unwrap();

        let result = feed(&mut coordinator, clear(42, 7));
        assert!(matches!(
            result,
            Err(CoordinatorError::ClearedCommittedBank {
                slot: 42,
                bank_id: 7
            })
        ));
    }

    /// Finality goes to the block machine slot by slot, so it still prunes its fork graph.
    #[test]
    fn verified_finality_still_prunes_the_wrapper_fork_graph() {
        use yellowstone_grpc_proto::geyser::{
            SlotStatus, SubscribeUpdateBlockMeta, SubscribeUpdateSlot,
        };

        let (_, input_rx) = mpsc::channel(1);
        let (_, parsed_rx) = mpsc::channel(1);
        let mut coordinator = BlockMachineCoordinator::<()>::new(
            input_rx,
            parsed_rx,
            None,
            None,
            AccountCommitAt::Finalized,
            true,
            DEFAULT_MAX_PENDING_BANKS,
        );
        let mut parent_hash = Hash::default();
        for slot in 1..=1500 {
            let blockhash = Hash::new_unique();
            let updates = [
                UpdateOneof::BlockMeta(SubscribeUpdateBlockMeta {
                    slot,
                    bank_id: slot,
                    parent_slot: slot - 1,
                    blockhash: blockhash.to_string(),
                    parent_blockhash: parent_hash.to_string(),
                    ..Default::default()
                }),
                UpdateOneof::Slot(SubscribeUpdateSlot {
                    slot,
                    bank_id: Some(slot),
                    parent: Some(slot - 1),
                    status: SlotStatus::SlotFinalized as i32,
                    ..Default::default()
                }),
            ];
            for update in updates {
                let events = coordinator
                    .convert_geyser_update(&SubscribeUpdate {
                        update_oneof: Some(update),
                        ..Default::default()
                    })
                    .unwrap();
                for event in events {
                    coordinator.state.apply(event).unwrap();
                }
                coordinator.state.drain_instruction_flushable().unwrap();
                coordinator.state.drain_account_flushable();
            }
            parent_hash = blockhash;
        }
        assert_eq!(coordinator.state.last_account_flushed_slot(), Some(1500));
        assert!(coordinator
            .wrapper
            .fork_graph()
            .oldest_rooted_slot()
            .is_some_and(|oldest| oldest > 1));
        assert!(coordinator.wrapper.fork_graph().len() < 1500);
    }

    /// At confirmed, both outputs flush before the bank's finality arrives. That finality still
    /// reaches the block machine, so its fork graph stays bounded.
    #[test]
    fn finality_after_a_confirmed_flush_still_prunes_the_wrapper_fork_graph() {
        use yellowstone_grpc_proto::geyser::{
            SlotStatus, SubscribeUpdateBlockMeta, SubscribeUpdateSlot,
        };

        let (_, input_rx) = mpsc::channel(1);
        let (_, parsed_rx) = mpsc::channel(1);
        let mut coordinator = BlockMachineCoordinator::<()>::new(
            input_rx,
            parsed_rx,
            None,
            None,
            AccountCommitAt::Confirmed,
            false,
            DEFAULT_MAX_PENDING_BANKS,
        );
        let feed = |coordinator: &mut BlockMachineCoordinator<()>, update| {
            for event in coordinator
                .convert_geyser_update(&SubscribeUpdate {
                    update_oneof: Some(update),
                    ..Default::default()
                })
                .unwrap()
            {
                coordinator.state.apply(event).unwrap();
            }
            coordinator.state.drain_instruction_flushable().unwrap();
            coordinator.state.drain_account_flushable();
            if let Some(through) = coordinator.state.flushed_through() {
                coordinator.commitments.prune_through(through);
            }
        };
        let status = |slot: Slot, status: SlotStatus| {
            UpdateOneof::Slot(SubscribeUpdateSlot {
                slot,
                bank_id: Some(slot),
                parent: Some(slot - 1),
                status: status as i32,
                ..Default::default()
            })
        };
        let hash = |slot: Slot| Hash::new_from_array([slot as u8; 32]);
        for slot in 1..=1500 {
            feed(
                &mut coordinator,
                UpdateOneof::BlockMeta(SubscribeUpdateBlockMeta {
                    slot,
                    bank_id: slot,
                    parent_slot: slot - 1,
                    blockhash: hash(slot).to_string(),
                    parent_blockhash: hash(slot - 1).to_string(),
                    ..Default::default()
                }),
            );
            feed(&mut coordinator, status(slot, SlotStatus::SlotConfirmed));
            // Finality trails confirmation by a few slots.
            if slot > 32 {
                feed(
                    &mut coordinator,
                    status(slot - 32, SlotStatus::SlotFinalized),
                );
            }
        }
        assert_eq!(coordinator.state.last_account_flushed_slot(), Some(1500));
        assert!(coordinator.wrapper.fork_graph().len() < 1500);
    }
}
