//! CoordinatorSource — transparent tap between geyser stream and Shipstern Runtime.
//!
//! Implements Shipstern's `SourceTrait` to forward raw geyser events to the coordinator
//! while also forwarding transaction events to the Shipstern Runtime.
//!

use std::{path::PathBuf, time::Duration};

use async_trait::async_trait;
use futures_util::StreamExt;
use shipstern::{
    sources::{FromConfig, SourceExitStatus, SourceTrait},
    Error as ShipsternError,
};
use shipstern_core::{CommitmentLevel, Filters};
use shipstern_yellowstone_grpc_source::YellowstoneGrpcConfig;
use tokio::sync::{mpsc::Sender, oneshot};
use yellowstone_grpc_client::GeyserGrpcClient;
use yellowstone_grpc_proto::{
    geyser::{
        subscribe_update::UpdateOneof, SubscribeRequest, SubscribeRequestFilterBlocksMeta,
        SubscribeRequestFilterEntry, SubscribeUpdate,
    },
    tonic::{transport::ClientTlsConfig, Status},
};

use crate::{fixtures::FixtureWriter, types::CoordinatorInput};

const DEFAULT_STREAM_IDLE_WARN_SECS: u64 = 0;

const fn default_stream_idle_warn_secs() -> u64 { DEFAULT_STREAM_IDLE_WARN_SECS }

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
    /// - entries (block reconstruction)
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
            .insert("coordinator".to_string(), SubscribeRequestFilterEntry {});
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

/// Shipstern source that taps the geyser stream for the coordinator.
///
/// On each `SubscribeUpdate`:
/// 1. Forward the raw event to the coordinator (clone for BlockSM-relevant events)
/// 2. Forward Account/Transaction events to the Shipstern Runtime (move, no clone)
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
    async fn connect(
        &self,
        filters: Filters,
        tx: Sender<Result<SubscribeUpdate, Status>>,
        status_tx: oneshot::Sender<SourceExitStatus>,
    ) -> Result<(), ShipsternError> {
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

        if let Some(reconnect_config) = config.reconnect_config() {
            tracing::info!(source_label, ?reconnect_config, "Auto-reconnect enabled");
            builder = builder.set_reconnect_config(reconnect_config);
        }

        let mut client = builder.connect().await?;

        let subscribe_request = SubscribeRequest::from(filters)
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

        let (_sub_tx, stream) = client
            .subscribe_with_request(Some(subscribe_request))
            .await?;

        let mut stream = std::pin::pin!(stream);

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
                        && coordinator_tx
                            .send(CoordinatorInput::AccountEventSeen { slot: acct.slot })
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
                    // Entry, Slot, and BlockMeta events are needed for block reconstruction.
                    let is_block_sm_event = matches!(
                        subscribe_update.update_oneof,
                        Some(
                            UpdateOneof::Entry(_)
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

        let _ = status_tx.send(exit_status);

        Ok(())
    }
}
