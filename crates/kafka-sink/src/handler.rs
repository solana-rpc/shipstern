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
        // Only startup accounts have no bank. CoordinatorSource stops on any other one.
        let Some(bank_id) = update.bank_id else {
            tracing::debug!(slot, "Account update without bank_id skipped");
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

            // A fallback account still counts as failed, like a fallback instruction. Only a
            // parse error makes one. Send the stat first: the record can complete the slot, and
            // the coordinator drops a stat that arrives after the slot flushed.
            if !record.is_decoded {
                self.cancelable_send(
                    self.handle
                        .send_parse_stats(slot, bank_id, ParseStatsKind::AccountErrorFallback)
                        .await,
                );
            }

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

#[cfg(all(test, feature = "experimental-account-parser"))]
mod tests {
    use std::borrow::Cow;

    use prost::Message;
    use shipstern::Handler;
    use shipstern_block_coordinator::{
        BlockMetadata, CoordinatorEvent, CoordinatorMessage, CoordinatorState,
    };
    use shipstern_core::{
        AccountUpdateInfo, ParseError, ParseResult, Parser, Prefilter, ProgramParser, Pubkey,
    };
    use tokio::sync::mpsc;

    use super::*;
    use crate::sink::KafkaSinkBuilder;

    const OWNER: [u8; 32] = [1; 32];

    #[derive(Clone, PartialEq, Message)]
    struct Empty {}

    struct FailingAccountParser;

    impl Parser for FailingAccountParser {
        type Input = AccountUpdate;
        type Output = Empty;

        fn id(&self) -> Cow<'static, str> { "failing-account-parser".into() }

        fn prefilter(&self) -> Prefilter { Prefilter::default() }

        async fn parse(&self, _value: &Self::Input) -> ParseResult<Self::Output> {
            Err(ParseError::from(std::io::Error::other("test parse error")))
        }
    }

    impl ProgramParser for FailingAccountParser {
        fn program_id(&self) -> Pubkey { OWNER.into() }
    }

    fn account_update() -> AccountUpdate {
        AccountUpdate {
            slot: 100,
            bank_id: Some(7),
            is_startup: false,
            account: Some(AccountUpdateInfo {
                txn_signature: None,
                write_version: 11,
                pubkey: vec![2_u8; 32],
                data: vec![9_u8],
                executable: false,
                lamports: 1,
                owner: OWNER.to_vec(),
                rent_epoch: 0,
            }),
        }
    }

    /// Runs one account update through a handler whose parser fails, and returns the messages
    /// it sent to the coordinator.
    async fn handle_failing_account_update() -> Vec<CoordinatorMessage<PreparedRecord>> {
        let parsers = KafkaSinkBuilder::new()
            .account_parser_with_fallback(
                FailingAccountParser,
                "test",
                "test.accounts",
                "failed.test.accounts",
            )
            .build();

        let (tx, mut rx) = mpsc::channel(8);
        let handler = BufferingHandler::new(
            parsers,
            CoordinatorHandle::new(tx),
            CancellationToken::new(),
        );

        let update = account_update();

        handler
            .handle(&update, &update)
            .await
            .expect("handle account update");

        drop(handler);

        let mut messages = Vec::new();
        while let Some(message) = rx.recv().await {
            messages.push(message);
        }

        messages
    }

    #[tokio::test]
    async fn account_parse_error_routed_to_fallback_is_still_counted_as_error() {
        let mut fallback_records = 0;
        let mut error_stats = 0;

        for message in handle_failing_account_update().await {
            match message {
                CoordinatorMessage::AccountParsed { record, .. } => {
                    assert_eq!(record.topic, "failed.test.accounts");

                    fallback_records += 1;
                },
                CoordinatorMessage::ParseStats {
                    kind: ParseStatsKind::AccountErrorFallback,
                    ..
                } => error_stats += 1,
                _ => {},
            }
        }

        assert_eq!(fallback_records, 1);
        assert_eq!(error_stats, 1);
    }

    #[tokio::test]
    async fn fallback_account_completing_its_slot_keeps_its_error_stat() {
        let update = account_update();
        let (slot, bank_id) = (update.slot, update.bank_id.expect("bank id"));

        // A confirmed slot that waits for this one account only.
        let mut state = CoordinatorState::<PreparedRecord>::default();
        let setup = [
            CoordinatorEvent::BlockFrozen {
                slot,
                bank_id,
                metadata: BlockMetadata {
                    parent_slot: slot - 1,
                    blockhash: Default::default(),
                    expected_tx_count: 0,
                },
            },
            CoordinatorEvent::AccountEventSeen { slot, bank_id },
            CoordinatorEvent::SlotConfirmed { slot, bank_id },
        ];
        for event in setup {
            state.apply(event).expect("apply setup event");
        }
        let _ = state
            .drain_instruction_flushable()
            .expect("drain instructions");

        // The coordinator drains account slots after every message.
        let mut flushed = Vec::new();
        for message in handle_failing_account_update().await {
            let event = match message {
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
                CoordinatorMessage::ParseStats {
                    slot,
                    bank_id,
                    kind,
                } => CoordinatorEvent::ParseStats {
                    slot,
                    bank_id,
                    kind,
                },
                _ => continue,
            };

            state.apply(event).expect("apply handler event");
            flushed.extend(state.drain_account_flushable());
        }

        assert_eq!(flushed.len(), 1);
        assert_eq!(flushed[0].records.len(), 1);
        assert_eq!(flushed[0].failed_account_count, 1);
        assert_eq!(state.late_account_record_drops(), 0);
    }
}
