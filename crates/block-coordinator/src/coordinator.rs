use std::collections::{BTreeSet, HashMap};

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
        AccountCommitAt, AccountSlot, BlockMetadata, ColorSlot, CoordinatorError, CoordinatorInput,
        CoordinatorMessage, DiscardReason, InstructionSlot,
    },
};

/// Core orchestrator that owns a `BlocksStateMachineWrapper` and delegates
/// ordering/flush decisions to the pure-ish `CoordinatorState`.
pub struct BlockMachineCoordinator<R> {
    wrapper: BlocksStateMachineWrapper,
    state: CoordinatorState<R>,
    input_rx: mpsc::Receiver<CoordinatorInput>,
    parsed_rx: mpsc::Receiver<CoordinatorMessage<R>>,
    instruction_output_tx: Option<mpsc::Sender<InstructionSlot<R>>>,
    account_output_tx: Option<mpsc::Sender<AccountSlot<R>>>,
    /// When false, Gate 1 is disabled by forcing expected_tx_count to 0.
    require_tx_gate: bool,
    /// Recent frozen banks, to find a parent whose BlockMeta never came.
    frozen_banks: BTreeSet<(Slot, u64)>,
    /// Parents of frozen blocks that have not frozen yet.
    missing_parents: BTreeSet<Slot>,
    /// The slot of each recent bank, in two generations so that memory stays bounded.
    bank_slots: [HashMap<u64, Slot>; 2],
}

/// How far back `frozen_banks` reaches. A parent older than this is not checked.
const FROZEN_SLOT_WINDOW: Slot = 4096;

/// A parent can arrive after its child, so wait this many slots before discarding it.
const MISSING_PARENT_GRACE_SLOTS: Slot = 64;

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
            frozen_banks: BTreeSet::new(),
            missing_parents: BTreeSet::new(),
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
                        CoordinatorInput::AccountEventSeen { slot } => {
                            vec![CoordinatorEvent::AccountEventSeen { slot }]
                        }
                    }
                }
                Some(msg) = self.parsed_rx.recv() => {
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
        // The wrapper rejects only events for a discarded bank or a duplicate block. Neither
        // removes the slot, so a rejection must not discard the slot.
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
                        events.push(CoordinatorEvent::SlotDiscarded {
                            slot: frozen.slot,
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
                        metadata,
                    });
                },
                BlockStateMachineOutput::SlotStatus(status)
                    if status.commitment
                        == solana_commitment_config::CommitmentLevel::Confirmed =>
                {
                    events.push(CoordinatorEvent::SlotConfirmed { slot: status.slot });
                },
                BlockStateMachineOutput::SlotStatus(status)
                    if status.commitment
                        == solana_commitment_config::CommitmentLevel::Finalized =>
                {
                    events.push(CoordinatorEvent::SlotFinalized { slot: status.slot });
                },
                BlockStateMachineOutput::SlotStatus(_) => {},
                BlockStateMachineOutput::DeadSlotDetected(dead) => {
                    self.missing_parents.remove(&dead.slot);
                    events.push(CoordinatorEvent::SlotDiscarded {
                        slot: dead.slot,
                        reason: DiscardReason::Dead,
                    });
                },
                BlockStateMachineOutput::ForksDetected(fork) => {
                    self.missing_parents.remove(&fork.slot);
                    events.push(CoordinatorEvent::SlotDiscarded {
                        slot: fork.slot,
                        reason: DiscardReason::Forked,
                    });
                },
                // Records are kept per slot, so one bank of a slot cannot be dropped alone.
                BlockStateMachineOutput::BankDiscarded(_) => {},
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

    /// Block machine 0.11 never freezes a bank whose BlockMeta is lost, and the child then
    /// waits on it forever. Discard such a parent once the stream is well past it.
    fn discard_lost_parents(
        &mut self,
        slot: Slot,
        bank_id: u64,
        parent: Slot,
    ) -> Vec<CoordinatorEvent<R>> {
        if self.is_far_ahead(slot) {
            return Vec::new();
        }
        if self
            .frozen_banks
            .first()
            .is_some_and(|&(low, _)| parent >= low)
            && !self.has_frozen_bank(parent)
        {
            self.missing_parents.insert(parent);
        }

        self.frozen_banks.insert((slot, bank_id));
        self.missing_parents.remove(&slot);

        let window_start = slot.saturating_sub(FROZEN_SLOT_WINDOW);
        self.frozen_banks = self.frozen_banks.split_off(&(window_start, 0));

        let still_open = self
            .missing_parents
            .split_off(&slot.saturating_sub(MISSING_PARENT_GRACE_SLOTS));
        let lost = std::mem::replace(&mut self.missing_parents, still_open);
        lost.into_iter()
            .map(|parent| {
                tracing::warn!(slot, parent, "Parent slot never froze; discarding it");
                CoordinatorEvent::SlotDiscarded {
                    slot: parent,
                    reason: DiscardReason::Incomplete,
                }
            })
            .collect()
    }

    /// A slot this far past every frozen bank is bogus. It must not age out the slots in grace.
    fn is_far_ahead(&self, slot: Slot) -> bool {
        self.frozen_banks
            .last()
            .is_some_and(|&(high, _)| slot > high.saturating_add(FROZEN_SLOT_WINDOW))
    }

    fn has_frozen_bank(&self, slot: Slot) -> bool {
        self.frozen_banks
            .range((slot, 0)..=(slot, u64::MAX))
            .next()
            .is_some()
    }

    fn convert_parsed_message(msg: CoordinatorMessage<R>) -> Vec<CoordinatorEvent<R>> {
        match msg {
            CoordinatorMessage::InstructionParsed { slot, key, record } => {
                vec![CoordinatorEvent::InstructionRecordParsed { slot, key, record }]
            },
            CoordinatorMessage::AccountParsed { slot, key, record } => {
                vec![CoordinatorEvent::AccountRecordParsed { slot, key, record }]
            },
            CoordinatorMessage::TransactionParsed { slot } => {
                vec![CoordinatorEvent::TransactionParsed { slot }]
            },
            CoordinatorMessage::ParseStats { slot, kind } => {
                vec![CoordinatorEvent::ParseStats { slot, kind }]
            },
        }
    }
}
