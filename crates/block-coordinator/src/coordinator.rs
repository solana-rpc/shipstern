use std::collections::HashMap;

use solana_clock::Slot;
use solana_commitment_config::CommitmentLevel;
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

            let pending = self.state.pending_bank_count();
            if pending > self.max_pending_banks {
                return Err(CoordinatorError::TooManyPendingBanks {
                    pending,
                    limit: self.max_pending_banks,
                    oldest_slot: self.state.oldest_pending_slot(),
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

        if let Some(UpdateOneof::EntryUpdateParent(parent)) = &update.update_oneof {
            return self.clear_bank(parent.slot, parent.cleared_bank_id);
        }

        let Some(info) = <SubscribeUpdate as GeyserEventAdapter>::extract_geyser_ev_info(update)
        else {
            return Ok(events);
        };
        if let Some(bank_id) = info.bank_id() {
            let slot = info.slot();
            self.check_bank_slot(slot, bank_id)?;
            // A late event must not revive a cleared, dead or losing bank in the wrapper.
            if self.state.is_invalidated_bank(BankKey::new(slot, bank_id)) {
                tracing::debug!(slot, bank_id, "Event for an invalidated bank; dropping it");
                return Ok(events);
            }
        }
        // The wrapper rejects only an event for a discarded or already frozen bank, or a
        // commitment with no bank ID. None removes the slot, so do not discard its other banks.
        if self.wrapper.handle_new_geyser_event(info).is_err() {
            return Ok(events);
        }

        while let Some(output) = self.wrapper.pop_next_state_machine_output() {
            match output {
                BlockStateMachineOutput::FrozenBlock(frozen) => {
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
                BlockStateMachineOutput::SlotStatus(status) => match status.commitment {
                    CommitmentLevel::Confirmed => {
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

        Ok(events)
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
        if self.state.is_bank_frozen_or_confirmed(key) {
            return Err(CoordinatorError::ClearedCommittedBank { slot, bank_id });
        }
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
}
