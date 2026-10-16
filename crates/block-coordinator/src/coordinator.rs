use std::collections::{BTreeMap, BTreeSet, HashMap};

use solana_clock::Slot;
use solana_hash::Hash;
use tokio::sync::mpsc;
use yellowstone_block_machine::{
    event::GeyserEventAdapter,
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
/// ordering/flush decisions to the pure-ish `CoordinatorState`. It also discards a slot that
/// never freezes or never confirms, because only the wrapper outputs show that.
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
    /// Recent slots that must freeze: named as a parent, or skipped while they have records.
    named_parents: BTreeSet<Slot>,
    /// Slots that must freeze and have not frozen yet.
    missing_parents: BTreeSet<Slot>,
    /// Recent slots with records and no frozen bank, that no frozen bank names or skips yet.
    record_slots: BTreeSet<Slot>,
    /// Frozen parents of confirmed banks with no confirm yet, and the confirmed child slot.
    unconfirmed_parents: BTreeMap<Slot, Slot>,
    /// Recent confirmed slots.
    confirmed_slots: BTreeSet<Slot>,
    /// The last slot that `expect_slot` checked, to skip repeated records.
    last_expected_slot: Option<Slot>,
    /// The slot of each recent bank, in two generations so that memory stays bounded.
    bank_slots: [HashMap<u64, Slot>; 2],
}

/// How far back `frozen_banks` reaches. A parent older than this is not checked.
const FROZEN_SLOT_WINDOW: Slot = 4096;

/// A parent can arrive after its child, so wait this many slots before discarding it.
/// Yellowstone drops a slot that does not resolve within the same 300 slots.
const MISSING_PARENT_GRACE_SLOTS: Slot = 300;

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
            named_parents: BTreeSet::new(),
            missing_parents: BTreeSet::new(),
            record_slots: BTreeSet::new(),
            unconfirmed_parents: BTreeMap::new(),
            confirmed_slots: BTreeSet::new(),
            last_expected_slot: None,
            bank_slots: Default::default(),
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
                    self.expect_slot(msg.slot());
                    Self::convert_parsed_message(msg)
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
            Some(UpdateOneof::Slot(status)) => status.bank_id.map(|bank_id| (status.slot, bank_id)),
            _ => None,
        };
        if let Some((slot, bank_id)) = bank {
            self.check_bank_slot(slot, bank_id)?;
        }

        let Some(info) = <SubscribeUpdate as GeyserEventAdapter>::extract_geyser_ev_info(update)
        else {
            return Ok(events);
        };
        // The wrapper rejects only an event for a discarded or already frozen bank, or a
        // commitment with no bank ID. None removes the slot, so do not discard its other banks.
        if self.wrapper.handle_new_geyser_event(info).is_err() {
            return Ok(events);
        }

        while let Some(output) = self.wrapper.pop_next_state_machine_output() {
            match output {
                BlockStateMachineOutput::FrozenBlock(frozen) => {
                    events.extend(self.discard_lost_parents(
                        frozen.slot,
                        frozen.bank_id,
                        frozen.parent_slot,
                    ));

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
                            "Discarding incomplete block"
                        );
                        events.push(CoordinatorEvent::BankDiscarded {
                            slot: frozen.slot,
                            bank_id: frozen.bank_id,
                            reason: DiscardReason::Incomplete,
                        });
                        continue;
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
                BlockStateMachineOutput::SlotStatus(status)
                    if status.commitment
                        == solana_commitment_config::CommitmentLevel::Confirmed =>
                {
                    self.note_confirm(status.slot, status.bank_id);
                    events.push(CoordinatorEvent::SlotConfirmed {
                        slot: status.slot,
                        bank_id: status.bank_id,
                    });
                },
                BlockStateMachineOutput::SlotStatus(status)
                    if status.commitment
                        == solana_commitment_config::CommitmentLevel::Finalized =>
                {
                    events.push(CoordinatorEvent::SlotFinalized {
                        slot: status.slot,
                        bank_id: status.bank_id,
                    });
                },
                BlockStateMachineOutput::SlotStatus(_) => {},
                BlockStateMachineOutput::DeadSlotDetected(dead) => {
                    self.missing_parents.remove(&dead.slot);
                    self.named_parents.remove(&dead.slot);
                    self.record_slots.remove(&dead.slot);
                    events.push(CoordinatorEvent::SlotDiscarded {
                        slot: dead.slot,
                        reason: DiscardReason::Dead,
                        bank_ids: dead.bank_ids,
                    });
                },
                BlockStateMachineOutput::ForksDetected(fork) => {
                    self.missing_parents.remove(&fork.slot);
                    self.named_parents.remove(&fork.slot);
                    self.record_slots.remove(&fork.slot);
                    events.push(CoordinatorEvent::SlotDiscarded {
                        slot: fork.slot,
                        reason: DiscardReason::Forked,
                        bank_ids: fork.bank_ids,
                    });
                },
                BlockStateMachineOutput::BankDiscarded(discarded) => {
                    // A frozen loser can be the parent's only frozen bank, which hides a
                    // winner whose BlockMeta never comes.
                    if self
                        .frozen_banks
                        .remove(&(discarded.slot, discarded.bank_id))
                        .is_some()
                        && !self.has_frozen_bank(discarded.slot)
                        && self.named_parents.contains(&discarded.slot)
                    {
                        self.missing_parents.insert(discarded.slot);
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

    /// Block machine 0.11 panics when one bank ID shows up in two slots, so stop with an error.
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
            .is_some_and(|(&(low, _), _)| slot < low)
            || self
                .frozen_banks
                .range((slot.saturating_add(1), 0)..)
                .any(|(_, &parent)| parent <= slot)
        {
            self.named_parents.insert(slot);
            self.missing_parents.insert(slot);
            return;
        }
        self.record_slots.insert(slot);
    }

    /// A confirmed bank puts its parent on the chain. A frozen parent whose confirm never comes
    /// would block every later slot, so note it for `discard_lost_parents`.
    fn note_confirm(&mut self, slot: Slot, bank_id: u64) {
        self.confirmed_slots.insert(slot);
        self.unconfirmed_parents.remove(&slot);
        if let Some(&parent) = self.frozen_banks.get(&(slot, bank_id))
            && self.has_frozen_bank(parent)
            && !self.confirmed_slots.contains(&parent)
        {
            self.unconfirmed_parents.entry(parent).or_insert(slot);
        }
    }

    /// Block machine 0.11 never freezes a bank whose BlockMeta is lost, and the child then
    /// waits on it forever. Discard such a parent once the stream is well past it.
    fn discard_lost_parents(
        &mut self,
        slot: Slot,
        bank_id: u64,
        parent: Slot,
    ) -> Vec<CoordinatorEvent<R>> {
        let far_ahead = self.is_far_ahead(slot);
        if self
            .frozen_banks
            .first_key_value()
            .is_some_and(|(&(low, _), _)| parent >= low)
            && !self.has_frozen_bank(parent)
        {
            self.missing_parents.insert(parent);
        }

        self.frozen_banks.insert((slot, bank_id), parent);
        self.named_parents.insert(parent);
        self.missing_parents.remove(&slot);
        self.record_slots.remove(&slot);

        // A slot with records must freeze if it is below every frozen bank, or if this parent
        // edge names or skips it.
        let low = self
            .frozen_banks
            .first_key_value()
            .map_or(slot, |(&(low, _), _)| low);
        while let Some(&passed) = self
            .record_slots
            .range(..low)
            .chain(self.record_slots.range(parent..slot))
            .next()
        {
            self.record_slots.remove(&passed);
            self.named_parents.insert(passed);
            self.missing_parents.insert(passed);
        }

        let window_start = slot.saturating_sub(FROZEN_SLOT_WINDOW);
        self.frozen_banks = self.frozen_banks.split_off(&(window_start, 0));
        self.named_parents = self.named_parents.split_off(&window_start);
        self.record_slots = self.record_slots.split_off(&window_start);
        self.confirmed_slots = self.confirmed_slots.split_off(&window_start);

        let cutoff = slot.saturating_sub(MISSING_PARENT_GRACE_SLOTS);
        let still_open = self.missing_parents.split_off(&cutoff);
        let mut lost = std::mem::replace(&mut self.missing_parents, still_open);
        self.unconfirmed_parents.retain(|&parent, &mut child| {
            child >= cutoff || {
                lost.insert(parent);
                false
            }
        });
        lost.into_iter()
            .map(|lost_slot| {
                tracing::warn!(
                    slot,
                    lost_slot,
                    "Slot never froze or confirmed; discarding it"
                );
                CoordinatorEvent::SlotDiscarded {
                    slot: lost_slot,
                    reason: DiscardReason::Incomplete,
                    bank_ids: Vec::new(),
                }
            })
            .collect()
    }

    /// A slot this far past every frozen bank must not age out the slots in grace.
    fn is_far_ahead(&self, slot: Slot) -> bool {
        self.frozen_banks
            .last_key_value()
            .is_some_and(|(&(high, _), _)| slot > high.saturating_add(FROZEN_SLOT_WINDOW))
    }

    fn has_frozen_bank(&self, slot: Slot) -> bool {
        self.frozen_banks
            .range((slot, 0)..=(slot, u64::MAX))
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
