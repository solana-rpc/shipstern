use shipstern::HandlerResult;
use shipstern_block_coordinator::{
    AccountRecordSortKey, CoordinatorHandle, InstructionRecordSortKey, ParseStatsKind,
};
use shipstern_core::{
    instruction::{InstructionUpdate, Path},
    AccountUpdate, TransactionUpdate,
};
use tokio_util::sync::CancellationToken;

#[cfg(feature = "experimental-account-parser")]
use crate::kafka_sink::AccountMsg;
use crate::{events::PreparedRecord, sink::KafkaSink};

/// Handler that parses transaction instructions and account updates eagerly
/// (at processed commitment) and sends the resulting `PreparedRecord`s to the
/// `BlockMachineCoordinator`.
///
/// After processing all instructions in a transaction, signals `TransactionParsed`
/// so the coordinator can track the fully-parsed gate.
#[derive(Clone)]
pub struct BufferingHandler {
    parsers: KafkaSink,
    handle: CoordinatorHandle<PreparedRecord>,
    cancel: CancellationToken,
}

impl std::fmt::Debug for BufferingHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BufferingHandler").finish()
    }
}

fn sort_key(tx_index: u64, path: &Path) -> InstructionRecordSortKey {
    InstructionRecordSortKey::new(
        tx_index,
        path.as_slice().iter().map(|&i| i as usize).collect(),
    )
}

impl BufferingHandler {
    pub fn new(
        parsers: KafkaSink,
        handle: CoordinatorHandle<PreparedRecord>,
        cancel: CancellationToken,
    ) -> Self {
        Self {
            parsers,
            handle,
            cancel,
        }
    }

    fn cancelable_send<T>(&self, result: Result<(), tokio::sync::mpsc::error::SendError<T>>) {
        if result.is_err() {
            self.cancel.cancel();
        }
    }
}

impl shipstern::Handler<TransactionUpdate, TransactionUpdate> for BufferingHandler {
    async fn handle(
        &self,
        update: &TransactionUpdate,
        _raw: &TransactionUpdate,
    ) -> HandlerResult<()> {
        let slot = update.slot;
        let bank_id = update.bank_id;

        let Some(ref tx_info) = update.transaction else {
            // Still count this as parsed so the gate count matches.
            self.cancelable_send(self.handle.send_transaction_parsed(slot, bank_id).await);

            return Ok(());
        };

        let tx_index = tx_info.index;

        // Skip failed transactions — no instruction parsing, no records to Kafka.
        if tx_info.meta.as_ref().and_then(|m| m.err.as_ref()).is_some() {
            self.cancelable_send(
                self.handle
                    .send_parse_stats(slot, bank_id, ParseStatsKind::TransactionStatusFailed)
                    .await,
            );

            self.cancelable_send(self.handle.send_transaction_parsed(slot, bank_id).await);

            return Ok(());
        }

        self.cancelable_send(
            self.handle
                .send_parse_stats(slot, bank_id, ParseStatsKind::TransactionStatusSucceeded)
                .await,
        );

        let instructions = match InstructionUpdate::build_from_txn(update) {
            Ok(ixs) => ixs,
            Err(e) => {
                tracing::warn!(?e, slot, tx_index, "Failed to parse instructions");

                self.cancelable_send(self.handle.send_transaction_parsed(slot, bank_id).await);

                return Ok(());
            },
        };

        for ix_update in &instructions {
            for (ix, flat_index) in ix_update.visit_all_with_flat_indices() {
                let (result, had_error) = self
                    .parsers
                    .parse_instruction(slot, tx_index, &ix.shared.signature, &flat_index, ix)
                    .await;

                if let Some(record) = result {
                    let is_decoded = record.is_decoded;

                    let send_result = self
                        .handle
                        .send_instruction_parsed(
                            slot,
                            bank_id,
                            sort_key(tx_index, &flat_index),
                            record,
                        )
                        .await;

                    if let Err(e) = send_result {
                        tracing::error!(slot, tx_index, "Failed to send parsed record");

                        self.cancelable_send(Err(e));
                    }

                    // Fallback instruction records are still filtered/error outcomes.
                    // Count them so slot stats reflect decode quality.
                    if !is_decoded {
                        let kind = if had_error {
                            ParseStatsKind::InstructionError
                        } else {
                            ParseStatsKind::InstructionFiltered
                        };

                        self.cancelable_send(
                            self.handle.send_parse_stats(slot, bank_id, kind).await,
                        );
                    }
                } else {
                    let kind = if had_error {
                        ParseStatsKind::InstructionError
                    } else {
                        ParseStatsKind::InstructionFiltered
                    };

                    self.cancelable_send(self.handle.send_parse_stats(slot, bank_id, kind).await);
                }
            }
        }

        // Signal this transaction is fully parsed.
        self.cancelable_send(self.handle.send_transaction_parsed(slot, bank_id).await);

        Ok(())
    }
}

impl shipstern::Handler<AccountUpdate, AccountUpdate> for BufferingHandler {
    async fn handle(&self, update: &AccountUpdate, _raw: &AccountUpdate) -> HandlerResult<()> {
        let slot = update.slot;
        // Only startup accounts lack a bank; CoordinatorSource rejects any other account without one.
        let Some(bank_id) = update.bank_id else {
            return Ok(());
        };

        let (record, had_error) = self.parsers.parse_account(slot, update).await;
        if let Some(record) = record {
            let info = update
                .account
                .as_ref()
                .expect("parse_account returned Some so account must exist");

            let pubkey: [u8; 32] = match info.pubkey.as_slice().try_into() {
                Ok(pk) => pk,
                Err(_) => {
                    tracing::warn!(
                        slot,
                        pubkey_len = info.pubkey.len(),
                        "Malformed pubkey in account update"
                    );

                    self.cancelable_send(
                        self.handle
                            .send_parse_stats(slot, bank_id, ParseStatsKind::AccountError)
                            .await,
                    );

                    return Ok(());
                },
            };

            let write_version = info.write_version;

            let key = AccountRecordSortKey::new(write_version, pubkey);

            let send_result = self
                .handle
                .send_account_parsed(slot, bank_id, key, record)
                .await;

            if let Err(e) = send_result {
                tracing::error!(slot, "Failed to send account parsed record");

                self.cancelable_send(Err(e));
            }
        } else {
            let kind = if had_error {
                ParseStatsKind::AccountError
            } else {
                ParseStatsKind::AccountFiltered
            };

            self.cancelable_send(self.handle.send_parse_stats(slot, bank_id, kind).await);
        }
        // No send_transaction_parsed — accounts don't affect the TX gate.
        Ok(())
    }
}

#[cfg(feature = "experimental-account-parser")]
/// Mode B handler: parses accounts at finalized commitment and sends directly
/// to the AccountPassthroughSink via a channel (no coordinator buffering).
#[derive(Clone)]
pub struct PassthroughAccountHandler {
    parsers: KafkaSink,
    tx: tokio::sync::mpsc::Sender<AccountMsg>,
    cancel: CancellationToken,
}

#[cfg(feature = "experimental-account-parser")]
impl std::fmt::Debug for PassthroughAccountHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PassthroughAccountHandler").finish()
    }
}

#[cfg(feature = "experimental-account-parser")]
impl PassthroughAccountHandler {
    pub fn new(
        parsers: KafkaSink,
        tx: tokio::sync::mpsc::Sender<AccountMsg>,
        cancel: CancellationToken,
    ) -> Self {
        Self {
            parsers,
            tx,
            cancel,
        }
    }
}

#[cfg(feature = "experimental-account-parser")]
impl shipstern::Handler<AccountUpdate, AccountUpdate> for PassthroughAccountHandler {
    async fn handle(&self, update: &AccountUpdate, _raw: &AccountUpdate) -> HandlerResult<()> {
        let slot = update.slot;
        let (record, had_error) = self.parsers.parse_account(slot, update).await;

        if let Err(e) = self
            .tx
            .send(AccountMsg::Record {
                slot,
                record,
                had_error,
            })
            .await
        {
            tracing::error!(?e, slot, "Failed to send passthrough account msg");

            self.cancel.cancel();
        }
        Ok(())
    }
}
