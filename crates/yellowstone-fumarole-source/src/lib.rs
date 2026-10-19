use std::{collections::BTreeMap, num::NonZero};

use async_trait::async_trait;
use bytesize::ByteSize;
use clap::ValueEnum;
use futures_util::StreamExt;
use shipstern::{
    sources::{FromConfig, SourceContext, SourceExitStatus, SourceTrait},
    CommitmentLevel, Error as ShipsternError,
};
use yellowstone_fumarole_client::{
    stream::FumaroleEvent, FumaroleClient, FumaroleSubscribeConfig, DEFAULT_PARA_DATA_STREAMS,
};
pub use yellowstone_grpc_proto::tonic::codec::CompressionEncoding;
use yellowstone_grpc_proto::{geyser::SubscribeRequest, tonic::Status};

/// A `Source` implementation for the Yellowstone gRPC API.
#[derive(Debug)]
pub struct YellowstoneFumaroleSource {
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

/// fumarole-client 0.9 panics below its SHARDED_DOWNLOAD_MINIMUM_MINOR_VERSION.
const MIN_SERVICE_MINOR_VERSION: u64 = 39;

/// The client checks only the minor version, so the major version is ignored here too.
fn is_supported_service_version(version: &str) -> bool {
    semver::Version::parse(version).is_ok_and(|v| v.minor >= MIN_SERVICE_MINOR_VERSION)
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

impl FromConfig for YellowstoneFumaroleSource {
    type Config = FumaroleConfig;

    fn from_config(config: Self::Config) -> Self { Self { config } }
}

#[async_trait]
impl SourceTrait for YellowstoneFumaroleSource {
    async fn connect(&self, ctx: SourceContext) -> Result<SourceExitStatus, ShipsternError> {
        let SourceContext { filters, tx, .. } = ctx;

        let subscriber_name = self.config.subscriber_name.clone();

        let fumarole_subscribe_config = FumaroleSubscribeConfig {
            num_data_plane_tcp_connections: NonZero::new(DEFAULT_PARA_DATA_STREAMS).unwrap(),
            ..Default::default()
        };

        let mut fumarole_client = match FumaroleClient::connect(self.config.clone().into()).await {
            Ok(client) => client,
            Err(e) => {
                tracing::error!(err = %e, "Fumarole source connection failed");
                return Err(ShipsternError::Other(Box::new(e)));
            },
        };

        let version = fumarole_client
            .version()
            .await
            .map_err(ShipsternError::YellowstoneStatus)?
            .version;
        if !is_supported_service_version(&version) {
            return Err(ShipsternError::Other(
                format!(
                    "Fumarole service version {version} is not supported; fumarole-client 0.9 \
                     needs minor version {MIN_SERVICE_MINOR_VERSION} or newer"
                )
                .into(),
            ));
        }

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
                return Err(ShipsternError::YellowstoneStatus(status));
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

        Ok(exit_status)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use shipstern::sources::{FromConfig, SourceContext, SourceTrait};
    use shipstern_core::Filters;
    use tokio::sync::mpsc;

    use super::{is_supported_service_version, FumaroleConfig, YellowstoneFumaroleSource};

    /// The 0.4 client had this check backwards, so pin both sides of the minimum.
    #[test]
    fn service_version_check_needs_minor_39() {
        assert!(!is_supported_service_version("0.38.0"));
        assert!(is_supported_service_version("0.39.0"));
        assert!(!is_supported_service_version("not a version"));
    }

    #[test]
    fn connect_reports_connection_errors_instead_of_panicking() {
        let runtime = tokio::runtime::Runtime::new().expect("runtime should build");

        runtime.block_on(async {
            let source = YellowstoneFumaroleSource::from_config(FumaroleConfig {
                endpoint: "http://127.0.0.1:9".to_string(),
                subscriber_name: "test-subscriber".to_string(),
                ..FumaroleConfig::default()
            });
            let (tx, mut rx) = mpsc::channel(1);

            let result = source
                .connect(SourceContext::new(Filters::new(HashMap::new()), tx))
                .await;

            let Err(shipstern::Error::Other(err)) = result else {
                panic!("expected a connect error, got {result:?}");
            };
            assert!(err.is::<yellowstone_fumarole_client::ConnectError>());
            assert!(rx.try_recv().is_err());
        });
    }
}
