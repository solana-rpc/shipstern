use std::time::Duration;

use async_trait::async_trait;
use clap::ValueEnum;
use futures_util::{stream::BoxStream, SinkExt, StreamExt};
use shipstern::{
    sources::{FilterUpdateSource, FromConfig, SourceContext, SourceExitStatus, SourceTrait},
    CommitmentLevel, Error as ShipsternError,
};
use shipstern_core::{AccountsDataSlice, Filters, PrefilterError};
use tokio::sync::{mpsc, watch};
use yellowstone_grpc_client::{GeyserGrpcClient, SubscribeRequestSink};
use yellowstone_grpc_proto::{
    geyser::{SubscribeRequest, SubscribeUpdate},
    tonic::{codec::CompressionEncoding, transport::ClientTlsConfig, Status},
};

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

/// Yellowstone connection configuration.
#[derive(Debug, clap::Args, serde::Deserialize, Clone)]
#[serde(rename_all = "kebab-case")]
pub struct YellowstoneGrpcConfig {
    /// The endpoint of the Yellowstone server.
    #[arg(long, env)]
    pub endpoint: String,
    /// The token to use for authentication.
    #[arg(long, env)]
    pub x_token: Option<String>,
    /// The timeout for the connection.
    #[arg(long, env, default_value_t = 120)]
    pub timeout: u64,

    #[arg(long, env)]
    pub commitment_level: Option<CommitmentLevel>,

    #[arg(long, env)]
    pub from_slot: Option<u64>,

    /// Receive only these `{ offset, length }` windows of each account's data. It
    /// applies to the whole subscription, so it lives here and not on a `Prefilter`.
    /// Windows must be ascending and non-overlapping, checked on connect.
    ///
    #[arg(skip)]
    #[serde(default)]
    pub accounts_data_slice: Vec<AccountsDataSlice>,

    #[arg(long, env)]
    pub max_decoding_message_size: Option<usize>,

    #[arg(long, env)]
    pub accept_compression: Option<ShipsternCompressionEncoding>,
}

impl YellowstoneGrpcConfig {
    /// Check the settings the server would refuse the whole subscription over, so a
    /// bad data-slice window fails at startup instead of on subscribe.
    ///
    /// # Errors
    ///
    /// Returns an error if [`Self::accounts_data_slice`] is not in ascending
    /// order of offset, overlaps, or carries a zero-length window.
    ///
    pub fn validate(&self) -> Result<(), PrefilterError> {
        AccountsDataSlice::validate_all(&self.accounts_data_slice)
    }
}

/// Updates from one connection.
type UpdateStream = BoxStream<'static, Result<SubscribeUpdate, Status>>;

/// Hand a terminal status to the runtime and turn it into the exit status.
async fn stream_error(
    tx: &mpsc::Sender<Result<SubscribeUpdate, Status>>,
    status: Status,
) -> SourceExitStatus {
    let code = status.code();
    let message = status.message().to_string();
    let _ = tx.send(Err(status)).await;
    SourceExitStatus::StreamError { code, message }
}

/// Dial the server and subscribe with `request`.
async fn open_stream(
    config: &YellowstoneGrpcConfig,
    request: SubscribeRequest,
) -> Result<(SubscribeRequestSink, UpdateStream), ShipsternError> {
    let timeout = Duration::from_secs(config.timeout);
    let builder = GeyserGrpcClient::build_from_shared(config.endpoint.clone())?
        .x_token(config.x_token.clone())?
        .max_decoding_message_size(config.max_decoding_message_size.unwrap_or(usize::MAX))
        .accept_compressed(config.accept_compression.unwrap_or_default().into())
        .connect_timeout(timeout)
        .timeout(timeout)
        .tls_config(ClientTlsConfig::new().with_native_roots())?;

    let mut client = builder.connect().await?;

    tracing::debug!(
        has_accounts = !request.accounts.is_empty(),
        account_filters = ?request.accounts.keys().collect::<Vec<_>>(),
        has_transactions = !request.transactions.is_empty(),
        transaction_filters = ?request.transactions.keys().collect::<Vec<_>>(),
        has_blocks_meta = !request.blocks_meta.is_empty(),
        blocks_meta_filters = ?request.blocks_meta.keys().collect::<Vec<_>>(),
        has_slots = !request.slots.is_empty(),
        slots_filters = ?request.slots.keys().collect::<Vec<_>>(),
        block_footer_filters = ?request.block_footer.keys().collect::<Vec<_>>(),
        from_slot = ?request.from_slot,
        commitment = ?request.commitment,
        "Subscribing to gRPC stream"
    );

    let (sink, stream) = client.subscribe_with_request(Some(request)).await?;
    Ok((sink, stream.boxed()))
}

/// A `Source` implementation for the Yellowstone gRPC API.
#[derive(Debug)]
pub struct YellowstoneGrpcSource {
    config: YellowstoneGrpcConfig,
}

/// Build the wire subscription for `filters`, layering on the commitment the
/// `From<Filters>` conversion leaves unset.
///
/// `from_slot` is deliberately not applied here. It is a one-time start
/// position rather than a steady-state setting, and repeating it on a
/// mid-stream update is destructive: yellowstone-grpc-geyser treats it as a
/// replay request and either replays every slot since, or ends the stream
/// with `from_slot is not supported` when it has no replay buffer, while
/// richat rejects the request outright if the set contains blocks. Only the
/// initial subscribe sets it.
///
fn build_subscribe_request(filters: Filters, config: &YellowstoneGrpcConfig) -> SubscribeRequest {
    let mut request: SubscribeRequest = filters.into();

    if let Some(commitment_level) = config.commitment_level {
        request.commitment = Some(commitment_level as i32);
    }

    // Request-level rather than per-parser, so `From<Filters>` cannot fill it:
    // the window belongs to the connection, not to any one pipeline. Unlike
    // `from_slot` it is steady state, so a later request that omitted it would
    // widen every account back to full data.
    request.accounts_data_slice = config
        .accounts_data_slice
        .iter()
        .copied()
        .map(Into::into)
        .collect();

    request
}

/// Yield the newest filter set once it changes, or never resolve when the
/// caller never asked for updates.
///
/// `select!` evaluates a disabled branch's expression before deciding not to
/// poll it, so this cannot be an `unwrap` guarded by a precondition.
///
/// Returns `None` once every handle is gone and no unseen set remains.
///
async fn next_filter_update(
    filter_updates_rx: &mut Option<watch::Receiver<Filters>>,
) -> Option<Filters> {
    match filter_updates_rx {
        Some(rx) => {
            rx.changed().await.ok()?;

            Some(rx.borrow_and_update().clone())
        },
        None => std::future::pending().await,
    }
}

///
/// Send `filters` to the server as the new subscription.
///
/// A rejection means the request channel is gone. The stream behind it has ended
/// too, so the set is dropped and the subscription keeps its previous filters.
///
async fn send_filter_update<S>(
    sink: &mut S,
    config: &YellowstoneGrpcConfig,
    filters: Filters,
    filter_updates_sent: &mut u64,
) where
    S: SinkExt<SubscribeRequest> + Unpin,
    S::Error: std::fmt::Display,
{
    let request = build_subscribe_request(filters, config);

    tracing::debug!(
        // Entry counts, one per parser, not pubkey counts.
        accounts = request.accounts.len(),
        transactions = request.transactions.len(),
        slots = request.slots.len(),
        blocks = request.blocks.len(),
        blocks_meta = request.blocks_meta.len(),
        block_footer = request.block_footer.len(),
        "Sending filter update to the live subscription"
    );

    if let Err(err) = sink.send(request).await {
        tracing::warn!(
            %err,
            "Filter update rejected by the sink and dropped; the subscription keeps its \
             previous filters"
        );

        return;
    }

    *filter_updates_sent += 1;
}

#[async_trait]
impl SourceTrait for YellowstoneGrpcSource {
    ///
    /// Open the subscription and pump updates until the stream ends, sending
    /// each filter set published on the context's update slot to the server so
    /// it replaces the live subscription.
    ///
    /// The source never reconnects. A lost stream ends the run at every
    /// commitment: with `StreamEnded` when the server closes it, and with
    /// `StreamError` when the server sends a status.
    ///
    async fn connect(&self, ctx: SourceContext) -> Result<SourceExitStatus, ShipsternError> {
        let SourceContext {
            filters,
            tx,
            mut filter_updates,
            ..
        } = ctx;

        let config = &self.config;

        // The server refuses out-of-order or overlapping windows and answers
        // with an error instead of a stream, so check before dialing and say
        // which window is at fault.
        config
            .validate()
            .map_err(|err| ShipsternError::Other(Box::new(err)))?;

        // A closed filter channel means no RuntimeHandle can publish more updates.
        let has_handle = filter_updates.has_changed().is_ok();

        // A closed slot can still hold one set that the runtime did not read.
        let filters = if has_handle {
            filters
        } else {
            filter_updates.borrow_and_update().clone()
        };
        let mut filter_updates_rx = Some(filter_updates);

        let mut request = build_subscribe_request(filters, config);
        request.from_slot = config.from_slot;

        let (mut sink, mut stream) = open_stream(config, request).await?;

        tracing::debug!("gRPC stream started");

        let mut filter_updates_sent: u64 = 0;

        let exit_status = loop {
            tokio::select! {
                update = stream.next() => match update {
                    Some(Ok(update)) => {
                        if tx.send(Ok(update)).await.is_err() {
                            tracing::info!("Receiver dropped, stopping source");
                            // Defensive only - normally unreachable because Signal/Buffer
                            // branch wins first when receiver drops.
                            break SourceExitStatus::ReceiverDropped;
                        }
                    },
                    Some(Err(status)) => {
                        // A server that rejects a filter set answers on the stream
                        // rather than the sink, so this is where a bad update
                        // surfaces. Report the count so an operator can tell that
                        // apart from an unrelated server error.
                        tracing::warn!(
                            code = ?status.code(),
                            message = %status.message(),
                            filter_updates_sent,
                            "Received error status from stream"
                        );

                        break stream_error(&tx, status).await;
                    },
                    None => break SourceExitStatus::StreamEnded,
                },

                update = next_filter_update(&mut filter_updates_rx) => {
                    let Some(new_filters) = update else {
                        // Every handle is gone and no new one can be taken, so retire this
                        // branch for the rest of the run.
                        tracing::debug!(
                            "Last runtime handle dropped, filter updates are off for this run"
                        );
                        filter_updates_rx = None;
                        continue;
                    };

                    send_filter_update(&mut sink, config, new_filters, &mut filter_updates_sent)
                        .await;
                },
            }
        };

        Ok(exit_status)
    }
}

impl FilterUpdateSource for YellowstoneGrpcSource {}

impl FromConfig for YellowstoneGrpcSource {
    type Config = YellowstoneGrpcConfig;

    fn from_config(config: Self::Config) -> Self { Self { config } }
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};

    use clap::Parser;
    use shipstern_core::{AccountPrefilter, Filters, Prefilter, Pubkey};

    use super::{
        build_subscribe_request, send_filter_update, CommitmentLevel, SubscribeRequest,
        YellowstoneGrpcConfig,
    };

    fn config_from(toml_src: &str) -> YellowstoneGrpcConfig {
        toml::from_str(toml_src).expect("config must deserialize")
    }

    /// Stands in for the client's sink, which has no constructor without the client's
    /// `test-tools` feature. A rejecting sink acts like one whose request channel is gone.
    #[derive(Default)]
    struct TestSink {
        sent: Vec<SubscribeRequest>,
        rejecting: bool,
    }

    #[derive(Debug)]
    struct Rejected;

    impl std::fmt::Display for Rejected {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("request channel closed")
        }
    }

    /// A set carrying one account owner, so a test can tell the request the
    /// sink received apart from an empty one.
    fn filters_owned_by(marker: u8) -> Filters {
        Filters::new(HashMap::from([("p".to_owned(), Prefilter {
            account: Some(AccountPrefilter {
                accounts: HashSet::new(),
                owners: HashSet::from([Pubkey::new([marker; 32])]),
                ..Default::default()
            }),
            ..Default::default()
        })]))
    }

    /// The owners the sink actually received under parser `p`.
    fn received_owners(request: &SubscribeRequest) -> Vec<String> {
        let mut owners = request
            .accounts
            .get("p")
            .expect("the request must carry the parser's account filter")
            .owner
            .clone();
        owners.sort();

        owners
    }

    impl futures_util::Sink<SubscribeRequest> for TestSink {
        type Error = Rejected;

        fn poll_ready(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<(), Self::Error>> {
            if self.rejecting {
                return std::task::Poll::Ready(Err(Rejected));
            }

            std::task::Poll::Ready(Ok(()))
        }

        fn start_send(
            mut self: std::pin::Pin<&mut Self>,
            item: SubscribeRequest,
        ) -> Result<(), Self::Error> {
            self.sent.push(item);

            Ok(())
        }

        fn poll_flush(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<(), Self::Error>> {
            std::task::Poll::Ready(Ok(()))
        }

        fn poll_close(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<(), Self::Error>> {
            std::task::Poll::Ready(Ok(()))
        }
    }

    /// A set that reaches the sink arrives as the requested subscription, not an empty one,
    /// and advances the counter that ties a later stream error to the update.
    #[tokio::test]
    async fn accepted_filter_update_reaches_the_sink_intact() {
        let config = config_from(
            r#"
            endpoint = "https://example.rpcpool.com"
            timeout = 60
        "#,
        );
        let mut sink = TestSink::default();
        let mut sent = 0;

        send_filter_update(&mut sink, &config, filters_owned_by(1), &mut sent).await;

        assert_eq!(sent, 1);
        assert_eq!(sink.sent.len(), 1);
        assert_eq!(received_owners(&sink.sent[0]), [
            Pubkey::new([1; 32]).to_string()
        ]);
    }

    /// A set the sink refuses is dropped, and the counter does not count it, so a
    /// later stream error is not blamed on an update the server never saw.
    #[tokio::test]
    async fn rejected_filter_update_is_dropped_and_not_counted() {
        let config = config_from(
            r#"
            endpoint = "https://example.rpcpool.com"
            timeout = 60
        "#,
        );
        let mut sink = TestSink {
            rejecting: true,
            ..TestSink::default()
        };
        let mut sent = 0;

        send_filter_update(&mut sink, &config, filters_owned_by(1), &mut sent).await;

        assert_eq!(sent, 0);
        assert!(sink.sent.is_empty());
    }

    /// The startup subscribe and every later filter update are built by the
    /// same function, so the commitment `Filters` does not carry lands on both
    /// rather than only on the initial request.
    #[test]
    fn subscribe_request_carries_commitment() {
        let config = config_from(
            r#"
            endpoint = "https://example.rpcpool.com"
            timeout = 60
            commitment-level = "finalized"
        "#,
        );

        let request = build_subscribe_request(Filters::new(HashMap::new()), &config);

        assert_eq!(request.commitment, Some(CommitmentLevel::Finalized as i32));
    }

    /// A configured `from-slot` must never reach a mid-stream update. Servers
    /// read it as a replay request, so repeating it would replay the whole gap
    /// or end the stream outright. Only the initial subscribe sets it, and it
    /// is set at that call site rather than here.
    #[test]
    fn subscribe_request_omits_from_slot() {
        let config = config_from(
            r#"
            endpoint = "https://example.rpcpool.com"
            timeout = 60
            from-slot = 350000000
        "#,
        );

        let request = build_subscribe_request(Filters::new(HashMap::new()), &config);

        assert_eq!(request.from_slot, None);
    }

    /// Without a commitment the request keeps what the `Filters` conversion
    /// produced, which leaves it unset.
    #[test]
    fn subscribe_request_omits_unset_commitment() {
        let config = config_from(
            r#"
            endpoint = "https://example.rpcpool.com"
            timeout = 60
        "#,
        );

        let request = build_subscribe_request(Filters::new(HashMap::new()), &config);

        assert_eq!(request.commitment, None);
    }

    /// The reconnect options are gone, and the config has no `deny_unknown_fields`, so a
    /// config file that still sets them loads as before and the keys are ignored. Nothing
    /// reads the old env vars either.
    #[test]
    fn config_file_with_removed_reconnect_keys_still_loads() {
        let config = config_from(
            r#"
            endpoint = "https://example.rpcpool.com"
            timeout = 60
            auto-reconnect = true
            reconnect-max-retries = 25
        "#,
        );

        assert_eq!(config.endpoint, "https://example.rpcpool.com");
        assert_eq!(config.timeout, 60);
    }

    /// The flags of the removed options are rejected, so a command line that still passes
    /// one fails at startup instead of running without the reconnect it asks for.
    #[test]
    fn removed_reconnect_flags_are_rejected() {
        #[derive(Debug, Parser)]
        struct Cli {
            #[command(flatten)]
            config: YellowstoneGrpcConfig,
        }

        let parse = |flag| Cli::try_parse_from(["shipstern", "--endpoint=https://x.example", flag]);

        for flag in ["--auto-reconnect", "--reconnect-max-retries=3"] {
            let err = parse(flag).expect_err("a removed flag must be rejected");

            assert_eq!(
                err.kind(),
                clap::error::ErrorKind::UnknownArgument,
                "{flag}: {err}"
            );
        }
    }

    /// A data slice is steady state, so it has to be on every request the
    /// subscription builds. Asserted on the built request rather than on the
    /// config, since the config carrying it is not the same as it being sent.
    #[test]
    fn accounts_data_slice_reaches_the_subscribe_request() {
        let config: YellowstoneGrpcConfig = toml::from_str(
            r#"
            endpoint = "https://example.rpcpool.com"
            timeout = 60

            [[accounts-data-slice]]
            offset = 0
            length = 8

            [[accounts-data-slice]]
            offset = 32
            length = 32
        "#,
        )
        .expect("config must deserialize");

        let request = build_subscribe_request(Filters::new(HashMap::new()), &config);
        let windows: Vec<_> = request
            .accounts_data_slice
            .iter()
            .map(|slice| (slice.offset, slice.length))
            .collect();

        assert_eq!(windows, vec![(0, 8), (32, 32)]);
    }

    /// With no windows configured the request has to look exactly as it did
    /// before the field existed.
    #[test]
    fn no_data_slice_leaves_the_request_unchanged() {
        let config: YellowstoneGrpcConfig = toml::from_str(
            r#"
            endpoint = "https://example.rpcpool.com"
            timeout = 60
        "#,
        )
        .expect("config must deserialize");

        let request = build_subscribe_request(Filters::new(HashMap::new()), &config);

        assert!(request.accounts_data_slice.is_empty());
        assert_eq!(request.commitment, None);
        assert_eq!(request.from_slot, None);
    }

    /// The `accounts-data-slice` key is the only way to set the windows, so
    /// the TOML shape is the whole user-facing surface of the feature.
    #[test]
    fn deserializes_accounts_data_slice_from_config() {
        let config: YellowstoneGrpcConfig = toml::from_str(
            r#"
            endpoint = "https://example.rpcpool.com"
            timeout = 60

            [[accounts-data-slice]]
            offset = 0
            length = 8

            [[accounts-data-slice]]
            offset = 32
            length = 32
        "#,
        )
        .expect("config with data slices must deserialize");

        let windows: Vec<_> = config
            .accounts_data_slice
            .iter()
            .map(|slice| (slice.offset, slice.length))
            .collect();

        assert_eq!(windows, vec![(0, 8), (32, 32)]);
        config
            .validate()
            .expect("ascending disjoint windows are valid");
    }

    /// A config predating the field must still load and must ask for full
    /// account data, or every existing deployment changes what it receives.
    #[test]
    fn accounts_data_slice_defaults_to_empty() {
        let config: YellowstoneGrpcConfig = toml::from_str(
            r#"
            endpoint = "https://example.rpcpool.com"
            timeout = 60
        "#,
        )
        .expect("config must deserialize");

        assert!(config.accounts_data_slice.is_empty());
        config.validate().expect("an empty window list is valid");
    }

    /// The server answers a malformed window list with an error instead of a
    /// stream, so it is worth refusing before dialing.
    #[test]
    fn validate_rejects_malformed_data_slices() {
        let config_with = |windows: &str| -> YellowstoneGrpcConfig {
            toml::from_str(&format!(
                r#"
                endpoint = "https://example.rpcpool.com"
                timeout = 60
                {windows}
            "#
            ))
            .expect("config must deserialize")
        };

        let overlapping = config_with(
            r#"
            [[accounts-data-slice]]
            offset = 0
            length = 16

            [[accounts-data-slice]]
            offset = 8
            length = 8
        "#,
        );

        assert!(overlapping.validate().is_err(), "overlap must be refused");

        let descending = config_with(
            r#"
            [[accounts-data-slice]]
            offset = 32
            length = 8

            [[accounts-data-slice]]
            offset = 0
            length = 8
        "#,
        );

        assert!(
            descending.validate().is_err(),
            "descending offsets must be refused"
        );
    }
}
