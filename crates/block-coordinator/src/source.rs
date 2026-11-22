//! CoordinatorSource — transparent tap between geyser stream and Shipstern Runtime.
//!
//! Implements Shipstern's `SourceTrait` to forward raw geyser events to the coordinator
//! while also forwarding transaction events to the Shipstern Runtime.
//!

use std::{
    collections::{BTreeSet, HashMap},
    path::PathBuf,
    time::Duration,
};

use async_trait::async_trait;
use futures_util::{stream::BoxStream, StreamExt};
use shipstern::{
    sources::{FromConfig, SourceContext, SourceExitStatus, SourceTrait},
    Error as ShipsternError,
};
use shipstern_core::CommitmentLevel;
use shipstern_yellowstone_grpc_source::YellowstoneGrpcConfig;
use solana_clock::Slot;
use solana_hash::Hash;
use tokio::sync::mpsc::Sender;
use yellowstone_grpc_client::{
    BankRef, GeyserGrpcClient, ReconnectConfig, ReconnectEvent, SlotWinner,
};
use yellowstone_grpc_proto::{
    geyser::{
        subscribe_update::UpdateOneof, SlotStatus, SubscribeRequest,
        SubscribeRequestFilterBlocksMeta, SubscribeRequestFilterEntry, SubscribeUpdate,
    },
    tonic::{transport::ClientTlsConfig, Status},
};

use crate::{
    fixtures::FixtureWriter,
    types::{tag_bank_id, BankKey, CoordinatorInput, DiscardedBank, SlotOutcome},
};

const DEFAULT_STREAM_IDLE_WARN_SECS: u64 = 0;

const fn default_stream_idle_warn_secs() -> u64 { DEFAULT_STREAM_IDLE_WARN_SECS }

/// Post Alpenglow validation for bank IDs in updates.
/// The coordinator keys records by bank, so the source stops when the server sends no bank
/// IDs. Other updates show a missing ID only as 0, and startup accounts have no bank.
fn validate_bank_id(update: &SubscribeUpdate) -> Result<(), &'static str> {
    match update.update_oneof.as_ref() {
        Some(UpdateOneof::Account(account)) if account.bank_id.is_none() && !account.is_startup => {
            Err("Account update has no bank ID")
        },
        // A server on yellowstone-grpc proto 13 or newer sets a bank ID on each of these statuses.
        Some(UpdateOneof::Slot(slot))
            if slot.bank_id.is_none()
                && matches!(
                    SlotStatus::try_from(slot.status),
                    Ok(SlotStatus::SlotCreatedBank
                        | SlotStatus::SlotProcessed
                        | SlotStatus::SlotConfirmed
                        | SlotStatus::SlotFinalized)
                ) =>
        {
            Err("Slot status has no bank ID")
        },
        _ => Ok(()),
    }
}

/// How many slots behind the newest update a bank's replay epoch is kept. A bank this far behind
/// is long finalized, so the server sends nothing more for it.
const EPOCH_RETENTION_SLOTS: Slot = 4096;

/// An item of the update stream: an update, or the banks a reconnect cut off.
#[allow(
    clippy::large_enum_variant,
    reason = "Keep updates inline to avoid an allocation per message"
)]
enum StreamItem {
    Update(SubscribeUpdate),
    Discard {
        banks: Vec<BankRef>,
        winners: Vec<SlotWinner>,
    },
}

impl From<ReconnectEvent> for StreamItem {
    fn from(event: ReconnectEvent) -> Self {
        match event {
            ReconnectEvent::Update { update, .. } => Self::Update(update),
            ReconnectEvent::DiscardBanks { banks, winners, .. } => Self::Discard { banks, winners },
        }
    }
}

///
/// The delivery of each bank that a reconnect cut off, counted from 0.
///
/// The handlers send parsed records on their own channel, so a record of a cut-off delivery can
/// reach the coordinator after the replay began. The handlers echo the bank ID of the update they
/// parsed, so the source puts the epoch in the high bits of that ID, and the coordinator reads it
/// back. A bank never cut off keeps the ID the server sent.
///
#[derive(Default)]
struct ReplayEpochs {
    epochs: HashMap<BankKey, u16>,
}

impl ReplayEpochs {
    /// The bank ID to give the handlers for a bank of `slot`.
    fn tag(&self, slot: Slot, bank_id: u64) -> Result<u64, &'static str> {
        let epoch = self
            .epochs
            .get(&BankKey::new(slot, bank_id))
            .copied()
            .unwrap_or(0);
        tag_bank_id(bank_id, epoch).ok_or("Bank ID is too large to carry a replay epoch")
    }

    /// Start the next delivery of each cut-off bank. `tip` is the newest slot seen.
    fn discard(
        &mut self,
        banks: &[BankRef],
        winners: &[SlotWinner],
        tip: Option<Slot>,
    ) -> Result<Vec<DiscardedBank>, String> {
        if let Some(tip) = tip {
            self.epochs
                .retain(|key, _| key.slot.saturating_add(EPOCH_RETENTION_SLOTS) >= tip);
        }

        // One bank can be cut off in more than one connection, and it is replayed once.
        let keys: BTreeSet<BankKey> = banks
            .iter()
            .map(|bank| BankKey::new(bank.slot, bank.bank_id))
            .collect();

        let mut discarded = Vec::with_capacity(keys.len());
        for key in keys {
            let outcome = winners
                .iter()
                .find_map(|winner| match winner {
                    SlotWinner::Unknown { slot } if *slot == key.slot => {
                        Some(Ok(SlotOutcome::Skipped))
                    },
                    SlotWinner::Finalized { slot, blockhash } if *slot == key.slot => Some(
                        blockhash
                            .parse::<Hash>()
                            .map(SlotOutcome::Finalized)
                            .map_err(|_| {
                                format!("Finalized block of slot {slot} has a malformed blockhash")
                            }),
                    ),
                    _ => None,
                })
                .ok_or_else(|| format!("No outcome for cut-off slot {}", key.slot))??;

            let epoch = self.epochs.get(&key).copied().unwrap_or(0);
            let epoch = epoch.checked_add(1).ok_or_else(|| {
                format!(
                    "Bank {} of slot {} was cut off too often",
                    key.bank_id, key.slot
                )
            })?;
            self.epochs.insert(key, epoch);
            discarded.push(DiscardedBank {
                slot: key.slot,
                bank_id: key.bank_id,
                epoch,
                outcome,
            });
        }
        Ok(discarded)
    }
}

/// Give the handlers the bank ID that names the delivery. Returns the slot and tagged bank ID of
/// an account update, which the coordinator counts.
fn tag_runtime_update(
    update: &mut SubscribeUpdate,
    epochs: &ReplayEpochs,
) -> Result<Option<(Slot, u64)>, &'static str> {
    match update.update_oneof.as_mut() {
        Some(UpdateOneof::Account(account)) => {
            let Some(bank_id) = account.bank_id else {
                return Ok(None);
            };
            let tagged = epochs.tag(account.slot, bank_id)?;
            account.bank_id = Some(tagged);
            Ok(Some((account.slot, tagged)))
        },
        Some(UpdateOneof::Transaction(transaction)) => {
            transaction.bank_id = epochs.tag(transaction.slot, transaction.bank_id)?;
            Ok(None)
        },
        _ => Ok(None),
    }
}

/// The retry budget of the client's bank replay, or `None` for a source that does not reconnect.
fn bank_replay(config: &YellowstoneGrpcConfig, source_label: &str) -> Option<ReconnectConfig> {
    let reconnect = config.reconnect_config()?;
    // The client's replay refuses a from_slot, since it picks the replay boundary itself.
    if config.from_slot.is_some() {
        tracing::info!(
            source_label,
            "from_slot is set, so the coordinator source does not reconnect"
        );
        return None;
    }
    Some(reconnect)
}

/// Config for CoordinatorSource.
///
/// Wraps the real source config plus channels for coordinator inputs.
/// The channels are set programmatically after deserialization — they can't
/// come from a config file, hence `#[serde(skip)]` + `#[arg(skip)]`.
#[derive(Debug, serde::Deserialize, clap::Args)]
pub struct CoordinatorSourceConfig {
    #[command(flatten)]
    #[serde(flatten)]
    pub source: YellowstoneGrpcConfig,

    /// Optional label used in logs to distinguish multiple coordinator sources in logs.
    #[serde(default)]
    #[arg(long)]
    pub source_label: Option<String>,

    /// Warn when no stream data has arrived for this many seconds.
    /// Set to 0 to disable idle/resume logs.
    #[serde(default = "default_stream_idle_warn_secs")]
    #[arg(long, default_value_t = DEFAULT_STREAM_IDLE_WARN_SECS)]
    pub stream_idle_warn_secs: u64,

    /// Channel to send CoordinatorInput events to the coordinator.
    #[serde(skip)]
    #[arg(skip)]
    pub coordinator_input_tx: Option<Sender<CoordinatorInput>>,

    /// Path to write captured fixture data (length-delimited protobuf).
    #[serde(skip)]
    #[arg(skip)]
    pub fixture_path: Option<PathBuf>,

    /// Number of BlockMeta messages to capture before stopping.
    #[serde(skip)]
    #[arg(skip)]
    pub fixture_slots: Option<usize>,
}

///
/// Add all subscriptions required by the BlockSM coordinator:
/// - entries and bank invalidations (block reconstruction)
/// - blocks_meta (BlockSummary)
/// - slot lifecycle (FirstShredReceived, CreatedBank, Completed, Dead).
///
/// It also removes the block and block footer filters: the source forwards neither.
///
fn add_coordinator_subscriptions(request: &mut SubscribeRequest) {
    request.blocks.clear();
    request.block_footer.clear();
    request
        .entry
        .insert("coordinator".to_string(), SubscribeRequestFilterEntry {
            include_update_parent: Some(true),
        });
    request.blocks_meta.insert(
        "coordinator".to_string(),
        SubscribeRequestFilterBlocksMeta {},
    );
    request.slots.entry("coordinator".to_string()).or_default();
    for filter in request.slots.values_mut() {
        filter.interslot_updates = Some(true);
        filter.filter_by_commitment = None;
    }
}

///
/// Shipstern source that taps the geyser stream for the coordinator.
///
/// On each `SubscribeUpdate`:
/// 1. Forward the raw event to the coordinator (clone for BlockSM-relevant events)
/// 2. Forward Account/Transaction events to the Shipstern Runtime (move, no clone)
///
/// After a stream loss the source reconnects through the client's bank replay, within the
/// `auto-reconnect` and `reconnect-max-retries` budget. It does not reconnect when `from_slot`
/// is set. The client names the banks that were cut off and replays them, and the source tells
/// the coordinator to drop what it holds for them. The source stops when the server sends no
/// bank IDs.
///
/// The source rewrites `bank_id` in the account and transaction updates it gives the runtime, but
/// only for a bank the server replays after a cut-off.
///
#[derive(Debug)]
pub struct CoordinatorSource {
    config: CoordinatorSourceConfig,
}

impl FromConfig for CoordinatorSource {
    type Config = CoordinatorSourceConfig;

    fn from_config(config: Self::Config) -> Self { Self { config } }
}

#[async_trait]
impl SourceTrait for CoordinatorSource {
    async fn connect(&self, ctx: SourceContext) -> Result<SourceExitStatus, ShipsternError> {
        let SourceContext { filters, tx, .. } = ctx;

        let coordinator_tx = self.config.coordinator_input_tx.as_ref().ok_or_else(|| {
            ShipsternError::Io(std::io::Error::other(
                "coordinator_input_tx must be set before connect",
            ))
        })?;

        let mut fixture_writer = match (&self.config.fixture_path, self.config.fixture_slots) {
            (Some(path), Some(slots)) => {
                tracing::info!(?path, slots, "Fixture capture enabled");
                Some(FixtureWriter::new(path, slots).map_err(ShipsternError::Io)?)
            },
            _ => None,
        };

        let config = &self.config.source;
        let source_label = self
            .config
            .source_label
            .as_deref()
            .unwrap_or("CoordinatorSource");
        let timeout = Duration::from_secs(config.timeout);

        let mut builder = GeyserGrpcClient::build_from_shared(config.endpoint.clone())?
            .x_token(config.x_token.clone())?
            .max_decoding_message_size(config.max_decoding_message_size.unwrap_or(usize::MAX))
            .accept_compressed(config.accept_compression.unwrap_or_default().into())
            .connect_timeout(timeout)
            .timeout(timeout)
            .tls_config(ClientTlsConfig::new().with_native_roots())?;

        let replay = bank_replay(config, source_label);
        if let Some(reconnect) = &replay {
            builder = builder.set_reconnect_config(reconnect.clone());
        }

        let mut client = builder.connect().await?;

        let mut subscribe_request = SubscribeRequest::from(filters);
        {
            // The source forwards only account and transaction updates to the runtime.
            let undelivered: Vec<&str> = [
                ("block", subscribe_request.blocks.is_empty()),
                ("block meta", subscribe_request.blocks_meta.is_empty()),
                ("block footer", subscribe_request.block_footer.is_empty()),
                ("slot", subscribe_request.slots.is_empty()),
            ]
            .into_iter()
            .filter_map(|(kind, empty)| (!empty).then_some(kind))
            .collect();

            if !undelivered.is_empty() {
                tracing::warn!(
                    source_label,
                    ?undelivered,
                    "The coordinator source forwards only account and transaction updates"
                );
            }
        }
        add_coordinator_subscriptions(&mut subscribe_request);
        subscribe_request.from_slot = config.from_slot.or(subscribe_request.from_slot);
        // Processed: the two-gate flush needs every transaction before the confirmation.
        subscribe_request.commitment = Some(CommitmentLevel::Processed as i32);

        tracing::info!(
            source_label,
            has_transactions = !subscribe_request.transactions.is_empty(),
            has_blocks_meta = !subscribe_request.blocks_meta.is_empty(),
            has_slots = !subscribe_request.slots.is_empty(),
            has_entries = !subscribe_request.entry.is_empty(),
            from_slot = ?subscribe_request.from_slot,
            commitment = ?subscribe_request.commitment,
            "subscribing to gRPC stream"
        );

        let (_sub_tx, mut stream): (_, BoxStream<'_, Result<StreamItem, Status>>) =
            if replay.is_some() {
                let (sink, stream) = client
                    .subscribe_with_reconnect(Some(subscribe_request))
                    .await?;
                let stream = stream.map(|event| event.map(StreamItem::from));
                (sink, stream.boxed())
            } else {
                let (sink, stream) = client
                    .subscribe_with_request(Some(subscribe_request))
                    .await?;
                let stream = stream.map(|update| update.map(StreamItem::Update));
                (sink, stream.boxed())
            };

        tracing::info!(source_label, "gRPC stream started");

        let idle_warn_secs = self.config.stream_idle_warn_secs;
        let stream_idle_timeout = Duration::from_secs(idle_warn_secs);
        let mut last_seen_slot: Option<u64> = None;
        let mut idle_since: Option<std::time::Instant> = None;
        let mut epochs = ReplayEpochs::default();

        let exit_status = 'stream: loop {
            let item = if idle_warn_secs == 0 {
                match stream.next().await {
                    Some(update) => update,
                    None => break SourceExitStatus::StreamEnded,
                }
            } else {
                match tokio::time::timeout(stream_idle_timeout, stream.next()).await {
                    Ok(Some(update)) => {
                        // Stream resumed after idle — log recovery.
                        if let Some(since) = idle_since.take() {
                            tracing::info!(
                                source_label,
                                idle_duration_ms = since.elapsed().as_millis() as u64,
                                ?last_seen_slot,
                                endpoint = %self.config.source.endpoint,
                                "stream resumed"
                            );
                        }
                        update
                    },
                    Ok(None) => break SourceExitStatus::StreamEnded,
                    Err(_) => {
                        // Timeout — stream idle.
                        if idle_since.is_none() {
                            idle_since = Some(std::time::Instant::now());
                            tracing::warn!(
                                source_label,
                                idle_warn_secs,
                                ?last_seen_slot,
                                endpoint = %self.config.source.endpoint,
                                "stream idle"
                            );
                        }
                        continue;
                    },
                }
            };

            let mut update = match item {
                Ok(StreamItem::Update(update)) => Ok(update),
                Ok(StreamItem::Discard { banks, winners }) => {
                    let discarded = match epochs.discard(&banks, &winners, last_seen_slot) {
                        Ok(discarded) => discarded,
                        Err(message) => break SourceExitStatus::Error(message),
                    };
                    tracing::warn!(
                        source_label,
                        bank_count = discarded.len(),
                        "Reconnected; the server replays the banks that were cut off"
                    );
                    if coordinator_tx
                        .send(CoordinatorInput::BanksDiscarded(discarded))
                        .await
                        .is_err()
                    {
                        tracing::error!("Coordinator input channel closed");
                        break SourceExitStatus::Error(
                            "Coordinator input channel closed".to_string(),
                        );
                    }
                    continue;
                },
                Err(status) => Err(status),
            };

            if let Ok(subscribe_update) = &update {
                // The block machine reads a status it does not know as processed, so a
                // status from a newer server would advance the wrong bank. Skip it.
                if let Some(UpdateOneof::Slot(slot)) = &subscribe_update.update_oneof
                    && SlotStatus::try_from(slot.status).is_err()
                {
                    tracing::warn!(
                        source_label,
                        slot = slot.slot,
                        status = slot.status,
                        "Unknown slot status; not forwarding it to the coordinator"
                    );
                    continue;
                }
                if let Err(message) = validate_bank_id(subscribe_update) {
                    break SourceExitStatus::Error(message.to_string());
                }
                // Capture raw protobuf to fixture file if enabled.
                if let Some(ref mut writer) = fixture_writer {
                    match writer.write(subscribe_update) {
                        Ok(true) => {},
                        Ok(false) => {
                            tracing::info!(path = ?self.config.fixture_path, "Fixture capture complete");
                            break SourceExitStatus::Completed;
                        },
                        Err(e) => {
                            tracing::error!(?e, "Fixture write failed");
                            break SourceExitStatus::Error(e.to_string());
                        },
                    }
                }
            }

            let account_seen = match &mut update {
                Ok(subscribe_update) => match tag_runtime_update(subscribe_update, &epochs) {
                    Ok(seen) => seen,
                    Err(message) => break SourceExitStatus::Error(message.to_string()),
                },
                Err(_) => None,
            };

            match &update {
                Ok(subscribe_update) => {
                    // Send lightweight AccountEventSeen for each Account event.
                    if let Some((slot, bank_id)) = account_seen
                        && coordinator_tx
                            .send(CoordinatorInput::AccountEventSeen { slot, bank_id })
                            .await
                            .is_err()
                    {
                        tracing::error!("Coordinator input channel closed");
                        break 'stream SourceExitStatus::Error(
                            "Coordinator input channel closed".to_string(),
                        );
                    }

                    // Track last seen slot for idle/resume logging.
                    let event_slot = match &subscribe_update.update_oneof {
                        Some(UpdateOneof::Slot(s)) => Some(s.slot),
                        Some(UpdateOneof::BlockMeta(bm)) => Some(bm.slot),
                        Some(UpdateOneof::Transaction(tx)) => Some(tx.slot),
                        Some(UpdateOneof::Account(acct)) => Some(acct.slot),
                        _ => None,
                    };
                    if let Some(slot) = event_slot {
                        last_seen_slot = Some(slot);
                    }

                    // Forward BlockSM-relevant events to the coordinator.
                    // Parent updates also release records from the bank that was cleared.
                    let is_block_sm_event = matches!(
                        subscribe_update.update_oneof,
                        Some(
                            UpdateOneof::Entry(_)
                                | UpdateOneof::EntryUpdateParent(_)
                                | UpdateOneof::Slot(_)
                                | UpdateOneof::BlockMeta(_)
                        )
                    );

                    if is_block_sm_event {
                        // Clone for coordinator (Account/Transaction events go to Runtime uncloned)
                        if coordinator_tx
                            .send(CoordinatorInput::GeyserUpdate(Box::new(
                                subscribe_update.clone(),
                            )))
                            .await
                            .is_err()
                        {
                            tracing::error!("Coordinator input channel closed");
                            break 'stream SourceExitStatus::Error(
                                "Coordinator input channel closed".to_string(),
                            );
                        }
                    }

                    // Only forward Account and Transaction events to the Runtime.
                    if !matches!(
                        subscribe_update.update_oneof,
                        Some(UpdateOneof::Account(_) | UpdateOneof::Transaction(_))
                    ) {
                        continue;
                    }
                },
                Err(status) => {
                    tracing::warn!(code = ?status.code(), message = %status.message(), "Received error status from stream");
                    let code = status.code();
                    let message = status.message().to_string();
                    let _ = tx.send(Err(status.clone())).await;
                    break SourceExitStatus::StreamError { code, message };
                },
            }

            if tx.send(update).await.is_err() {
                tracing::error!("Runtime channel closed");
                break SourceExitStatus::ReceiverDropped;
            }
        };

        if let Some(writer) = fixture_writer
            && let Err(e) = writer.finish()
        {
            tracing::error!(?e, "Failed to flush fixture writer on shutdown");
        }

        tracing::debug!("CoordinatorSource gRPC stream ended");

        Ok(exit_status)
    }
}

#[cfg(test)]
mod tests {
    use yellowstone_grpc_proto::geyser::{
        subscribe_update::UpdateOneof, SlotStatus, SubscribeUpdate, SubscribeUpdateAccount,
        SubscribeUpdateSlot,
    };

    use super::{add_coordinator_subscriptions, validate_bank_id};

    #[test]
    fn coordinator_subscription_requests_cleared_bank_updates() {
        let mut request = yellowstone_grpc_proto::geyser::SubscribeRequest::default();
        add_coordinator_subscriptions(&mut request);
        assert_eq!(
            request.entry["coordinator"].include_update_parent,
            Some(true)
        );
    }

    #[test]
    fn coordinator_subscription_drops_block_and_footer_filters() {
        let mut request = yellowstone_grpc_proto::geyser::SubscribeRequest::default();
        request
            .blocks
            .insert("blocks".to_string(), Default::default());
        request
            .block_footer
            .insert("footers".to_string(), Default::default());
        add_coordinator_subscriptions(&mut request);
        assert!(request.blocks.is_empty());
        assert!(request.block_footer.is_empty());
    }

    #[test]
    fn missing_bank_id_fails_closed() {
        let mut update = SubscribeUpdate {
            update_oneof: Some(UpdateOneof::Account(SubscribeUpdateAccount {
                slot: 42,
                is_startup: false,
                bank_id: None,
                ..Default::default()
            })),
            ..Default::default()
        };
        assert!(validate_bank_id(&update).is_err());
        if let Some(UpdateOneof::Account(account)) = &mut update.update_oneof {
            account.is_startup = true;
        }
        assert!(validate_bank_id(&update).is_ok());

        update.update_oneof = Some(UpdateOneof::Slot(SubscribeUpdateSlot {
            slot: 42,
            status: SlotStatus::SlotConfirmed as i32,
            bank_id: None,
            ..Default::default()
        }));
        assert!(validate_bank_id(&update).is_err());

        update.update_oneof = Some(UpdateOneof::Slot(SubscribeUpdateSlot {
            slot: 42,
            status: SlotStatus::SlotCreatedBank as i32,
            bank_id: None,
            ..Default::default()
        }));
        assert!(validate_bank_id(&update).is_err());

        // A first-shred status has no bank by design, so it must not stop the stream.
        update.update_oneof = Some(UpdateOneof::Slot(SubscribeUpdateSlot {
            slot: 42,
            status: SlotStatus::SlotFirstShredReceived as i32,
            bank_id: None,
            ..Default::default()
        }));
        assert!(validate_bank_id(&update).is_ok());
    }
}
