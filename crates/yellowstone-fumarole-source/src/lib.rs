use std::{collections::BTreeMap, num::NonZero};

use async_trait::async_trait;
use bytesize::ByteSize;
use clap::ValueEnum;
use futures_util::StreamExt;
use shipstern::{
    sources::{SourceExitStatus, SourceTrait},
    CommitmentLevel, Error as ShipsternError,
};
use shipstern_core::Filters;
use tokio::sync::{mpsc::Sender, oneshot};
use yellowstone_fumarole_client::{
    stream::FumaroleEvent, FumaroleClient, FumaroleSubscribeConfig, DEFAULT_PARA_DATA_STREAMS,
};
pub use yellowstone_grpc_proto::tonic::codec::CompressionEncoding;
use yellowstone_grpc_proto::{
    geyser::{SubscribeRequest, SubscribeUpdate},
    tonic::Status,
};

/// A `Source` implementation for the Yellowstone gRPC API.
#[derive(Debug)]
pub struct YellowstoneFumaroleSource {
    filters: Filters,
    config: FumaroleConfig,
}

#[derive(Default, Copy, Debug, serde::Deserialize, Clone, ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum ShipsternCompressionEncoding {
    Gzip,
    #[default]
    Zstd,
}

impl From<ShipsternCompressionEncoding> for CompressionEncoding {
    fn from(val: ShipsternCompressionEncoding) -> Self {
        match val {
            ShipsternCompressionEncoding::Gzip => CompressionEncoding::Gzip,
            ShipsternCompressionEncoding::Zstd => CompressionEncoding::Zstd,
        }
    }
}

#[derive(Debug, Clone, Default, serde::Deserialize, clap::Args)]
#[serde(rename_all = "kebab-case")]
pub struct FumaroleConfig {
    /// The endpoint of the Yellowstone Fumarole server.
    pub endpoint: String,
    /// The token to use for authentication.
    pub x_token: Option<String>,
    /// Name of the persistent subscriber to use
    pub subscriber_name: String,
    /// Slot commitment level
    pub commitment_level: Option<CommitmentLevel>,
    /// max incoming decoded message size in bytes
    pub max_decoding_message_size: Option<usize>,
    /// accepted compression encoding
    pub accept_compression: Option<ShipsternCompressionEncoding>,
}

// fumarole-client 0.9 has no Default, so the struct literal must set the window fields it ignores.
#[expect(deprecated)]
impl From<FumaroleConfig> for yellowstone_fumarole_client::config::FumaroleConfig {
    fn from(config: FumaroleConfig) -> Self {
        yellowstone_fumarole_client::config::FumaroleConfig {
            endpoint: config.endpoint,
            x_token: config.x_token,
            max_decoding_message_size_bytes: config.max_decoding_message_size.unwrap_or(usize::MAX),
            x_metadata: BTreeMap::new(),
            response_compression: config.accept_compression.map(Into::into),
            request_compression: config.accept_compression.map(Into::into),
            initial_connection_window_size: ByteSize::mb(100),
            initial_stream_window_size: ByteSize::mib(9),
        }
    }
}

/// Keep the gRPC code of a stream error, so an auth failure does not read as an internal one.
fn status_of(error: &(dyn std::error::Error + 'static)) -> Status {
    let mut source = Some(error);
    while let Some(err) = source {
        if let Some(status) = err.downcast_ref::<Status>() {
            return status.clone();
        }
        source = err.source();
    }
    Status::internal(error.to_string())
}

#[async_trait]
impl SourceTrait for YellowstoneFumaroleSource {
    type Config = FumaroleConfig;

    fn new(config: Self::Config, filters: Filters) -> Self { Self { filters, config } }

    async fn connect(
        &self,
        tx: Sender<Result<SubscribeUpdate, Status>>,
        status_tx: oneshot::Sender<SourceExitStatus>,
    ) -> Result<(), ShipsternError> {
        let filters = self.filters.clone();
        let subscriber_name = self.config.subscriber_name.clone();

        let fumarole_subscribe_config = FumaroleSubscribeConfig {
            num_data_plane_tcp_connections: NonZero::new(DEFAULT_PARA_DATA_STREAMS).unwrap(),
            ..Default::default()
        };

        let mut fumarole_client = match FumaroleClient::connect(self.config.clone().into()).await {
            Ok(client) => client,
            Err(e) => {
                let msg = format!("Failed to connect to fumarole: {e}");
                tracing::error!(%msg, "Fumarole source connection failed");
                let _ = status_tx.send(SourceExitStatus::Error(msg));
                return Ok(());
            },
        };

        let mut subscribe_request = SubscribeRequest::from(filters);

        if let Some(commitment_level) = self.config.commitment_level {
            subscribe_request.commitment = Some(commitment_level as i32);
        }

        let mut subscription = match fumarole_client
            .subscribe_with_config(
                subscriber_name,
                subscribe_request,
                fumarole_subscribe_config,
            )
            .await
        {
            Ok(session) => session,
            Err(status) => {
                tracing::error!(
                    code = ?status.code(),
                    message = status.message(),
                    "Fumarole source subscription failed"
                );
                let _ = status_tx.send(SourceExitStatus::StreamError {
                    code: status.code(),
                    message: status.message().to_owned(),
                });
                return Ok(());
            },
        };

        let exit_status = loop {
            match subscription.next().await {
                Some(Ok(FumaroleEvent::Data { update, .. })) => {
                    if tx.send(Ok(update)).await.is_err() {
                        tracing::info!("Receiver dropped, stopping source");
                        break SourceExitStatus::ReceiverDropped;
                    }
                },
                Some(Ok(FumaroleEvent::SlotEnded { .. })) => {},
                Some(Err(error)) => {
                    let status = status_of(&error);
                    let _ = tx.send(Err(status.clone())).await;
                    break SourceExitStatus::StreamError {
                        code: status.code(),
                        message: status.message().to_string(),
                    };
                },
                None => {
                    tracing::info!("Source returned None, exiting");
                    break SourceExitStatus::StreamEnded;
                },
            }
        };

        let _ = status_tx.send(exit_status);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use shipstern::sources::SourceTrait;
    use shipstern_core::Filters;
    use tokio::sync::{mpsc, oneshot};

    use super::{FumaroleConfig, YellowstoneFumaroleSource};

    #[test]
    fn connect_reports_connection_errors_instead_of_panicking() {
        let runtime = tokio::runtime::Runtime::new().expect("runtime should build");

        runtime.block_on(async {
            let source = YellowstoneFumaroleSource::new(
                FumaroleConfig {
                    endpoint: "http://127.0.0.1:9".to_string(),
                    subscriber_name: "test-subscriber".to_string(),
                    ..FumaroleConfig::default()
                },
                Filters::new(HashMap::new()),
            );
            let (tx, mut rx) = mpsc::channel(1);
            let (status_tx, status_rx) = oneshot::channel();

            source
                .connect(tx, status_tx)
                .await
                .expect("connect should report connection failure through source status");

            let status = status_rx.await.expect("source status should be sent");
            let shipstern::sources::SourceExitStatus::Error(msg) = status else {
                panic!("expected source error, got {status:?}");
            };
            assert!(msg.contains("Failed to connect to fumarole"));
            assert!(rx.try_recv().is_err());
        });
    }
}
