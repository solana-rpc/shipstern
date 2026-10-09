use std::collections::BTreeSet;

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
    /// Recent slots with a frozen bank, to find a parent whose BlockMeta never came.
    frozen_slots: BTreeSet<Slot>,
    /// Parents of frozen blocks that have not frozen yet.
    missing_parents: BTreeSet<Slot>,
}

/// How far back `frozen_slots` reaches. A parent older than this is not checked.
const FROZEN_SLOT_WINDOW: Slot = 4096;

/// A parent can arrive after its child, so wait this many slots before discarding it.
const MISSING_PARENT_GRACE_SLOTS: Slot = 64;

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
            // Freeze on BlockMeta: shipstern does not use footer data, and mainnet has no
            // footers before Alpenglow.
            wrapper: BlocksStateMachineWrapper::default().with_config(BlockMachineConfig {
                require_block_footer: false,
            }),
            state: CoordinatorState::new(account_commit_at),
            input_rx,
            parsed_rx,
            instruction_output_tx,
            account_output_tx,
            require_tx_gate,
            frozen_slots: BTreeSet::new(),
            missing_parents: BTreeSet::new(),
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
                            self.convert_geyser_update(update.as_ref())
                        }
                        CoordinatorInput::AccountEventSeen { slot, bank_id } => {
                            vec![CoordinatorEvent::AccountEventSeen { slot, bank_id }]
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
    fn convert_geyser_update(&mut self, update: &SubscribeUpdate) -> Vec<CoordinatorEvent<R>> {
        let mut events = Vec::new();

        // Guard: validate BlockMeta.blockhash BEFORE feeding to wrapper.
        // The block machine's adapter panics on a malformed hash, so skip such an update here.
        let malformed = match &update.update_oneof {
            Some(UpdateOneof::BlockMeta(meta)) => meta.blockhash.parse::<Hash>().is_err(),
            Some(UpdateOneof::Entry(entry)) => entry.hash.len() != 32,
            _ => false,
        };
        if malformed {
            tracing::warn!("BlockMeta or Entry has a malformed hash — skipping");
            return events;
        }

        let Some(info) = <SubscribeUpdate as GeyserEventAdapter>::extract_geyser_ev_info(update)
        else {
            return events;
        };
        // The wrapper rejects only events for a discarded bank or a duplicate block. Neither
        // removes the slot, so a rejection must not discard the other banks of the slot.
        if self.wrapper.handle_new_geyser_event(info).is_err() {
            return events;
        }

        while let Some(output) = self.wrapper.pop_next_state_machine_output() {
            match output {
                BlockStateMachineOutput::FrozenBlock(frozen) => {
                    events.extend(self.discard_lost_parents(frozen.slot, frozen.parent_slot));

                    let observed_tx_count = frozen
                        .entries
                        .iter()
                        .map(|entry| entry.executed_txn_count)
                        .sum::<u64>();
                    // The block machine freezes a bank even when entries are missing, for
                    // example when the stream starts in the middle of the slot.
                    if frozen.entries.len() as u64 != frozen.entries_count
                        || observed_tx_count != frozen.executed_transaction_count
                    {
                        tracing::warn!(
                            slot = frozen.slot,
                            bank_id = frozen.bank_id,
                            expected_entries = frozen.entries_count,
                            observed_entries = frozen.entries.len(),
                            expected_transactions = frozen.executed_transaction_count,
                            observed_transactions = observed_tx_count,
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
                        observed_tx_count
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

        events
    }

    /// Block machine 0.11 never freezes a bank whose BlockMeta is lost, and the child then
    /// waits on it forever. Discard such a parent once the stream is well past it.
    fn discard_lost_parents(&mut self, slot: Slot, parent: Slot) -> Vec<CoordinatorEvent<R>> {
        if self.frozen_slots.first().is_some_and(|&low| parent >= low)
            && !self.frozen_slots.contains(&parent)
        {
            self.missing_parents.insert(parent);
        }
        self.frozen_slots.insert(slot);
        self.missing_parents.remove(&slot);
        self.frozen_slots = self
            .frozen_slots
            .split_off(&slot.saturating_sub(FROZEN_SLOT_WINDOW));

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
                    bank_ids: Vec::new(),
                }
            })
            .collect()
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
