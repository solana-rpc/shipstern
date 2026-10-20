//! CoordinatorSource — transparent tap between geyser stream and Shipstern Runtime.
//!
//! Implements Shipstern's `SourceTrait` to forward raw geyser events to the coordinator
//! while also forwarding transaction events to the Shipstern Runtime.
//!

use std::{path::PathBuf, time::Duration};

use async_trait::async_trait;
use futures_util::StreamExt;
use shipstern::{
    sources::{stream_error, FromConfig, SourceContext, SourceExitStatus, SourceTrait},
    Error as ShipsternError,
};
use shipstern_core::CommitmentLevel;
use shipstern_yellowstone_grpc_source::{
    build_subscribe_request, open_stream, YellowstoneGrpcConfig,
};
use tokio::sync::mpsc::Sender;
use yellowstone_grpc_proto::geyser::{
    subscribe_update::UpdateOneof, SlotStatus, SubscribeRequest, SubscribeRequestFilterBlocksMeta,
    SubscribeRequestFilterEntry, SubscribeUpdate,
};

use crate::{fixtures::FixtureWriter, types::CoordinatorInput};

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
        // A server on Geyser proto 13 or newer sets a bank ID on each of these statuses.
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

trait CoordinatorSubscription {
    /// Add all subscriptions required by the BlockSM coordinator:
    /// - entries and bank invalidations (block reconstruction)
    /// - blocks_meta (BlockSummary)
    /// - slot lifecycle (FirstShredReceived, CreatedBank, Completed, Dead).
    fn with_coordinator_subscriptions(self) -> Self;

    /// Set the starting slot for the gRPC stream.
    fn with_from_slot(self, from_slot: Option<u64>) -> Self;

    /// Set commitment to Processed — required for the two-gate flush pattern
    /// so the coordinator sees all transactions before confirmation.
    fn with_commitment_processed(self) -> Self;
}

impl CoordinatorSubscription for SubscribeRequest {
    fn with_coordinator_subscriptions(mut self) -> Self {
        self.entry
            .insert("coordinator".to_string(), SubscribeRequestFilterEntry {
                include_update_parent: Some(true),
            });
        self.blocks_meta.insert(
            "coordinator".to_string(),
            SubscribeRequestFilterBlocksMeta {},
        );
        self.slots.entry("coordinator".to_string()).or_default();
        for filter in self.slots.values_mut() {
            filter.interslot_updates = Some(true);
            filter.filter_by_commitment = None;
        }
        self
    }

    fn with_from_slot(mut self, from_slot: Option<u64>) -> Self {
        self.from_slot = from_slot.or(self.from_slot);
        self
    }

    fn with_commitment_processed(mut self) -> Self {
        self.commitment = Some(CommitmentLevel::Processed as i32);
        self
    }
}

///
/// Shipstern source that taps the geyser stream for the coordinator.
///
/// On each `SubscribeUpdate`:
/// 1. Forward the raw event to the coordinator (clone for BlockSM-relevant events)
/// 2. Forward Account/Transaction events to the Shipstern Runtime (move, no clone)
///
/// The source does not reconnect, so a stream loss ends it. It also stops when the
/// server sends no bank IDs.
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

        // The gRPC source's builder applies the config's data slice. Its commitment is
        // replaced below, since the coordinator needs every bank.
        let subscribe_request = build_subscribe_request(filters, config)
            .with_coordinator_subscriptions()
            .with_from_slot(config.from_slot)
            .with_commitment_processed();

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

        // Checks the config before dialing. The sink stays open for the whole run.
        let (_sink, mut stream) = open_stream(config, subscribe_request).await?;

        tracing::info!(source_label, "gRPC stream started");

        let idle_warn_secs = self.config.stream_idle_warn_secs;
        let stream_idle_timeout = Duration::from_secs(idle_warn_secs);
        let mut last_seen_slot: Option<u64> = None;
        let mut idle_since: Option<std::time::Instant> = None;

        let exit_status = 'stream: loop {
            let update = if idle_warn_secs == 0 {
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

            match &update {
                Ok(subscribe_update) => {
                    // Send lightweight AccountEventSeen for each Account event.
                    if let Some(UpdateOneof::Account(acct)) = &subscribe_update.update_oneof
                        && let Some(bank_id) = acct.bank_id
                        && coordinator_tx
                            .send(CoordinatorInput::AccountEventSeen {
                                slot: acct.slot,
                                bank_id,
                            })
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
                    break stream_error(&tx, status.clone()).await;
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

    use super::{validate_bank_id, CoordinatorSubscription};

    #[test]
    fn coordinator_subscription_requests_cleared_bank_updates() {
        let request = yellowstone_grpc_proto::geyser::SubscribeRequest::default()
            .with_coordinator_subscriptions();
        assert_eq!(
            request.entry["coordinator"].include_update_parent,
            Some(true)
        );
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
