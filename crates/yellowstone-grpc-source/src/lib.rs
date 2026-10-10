use std::time::Duration;

use async_trait::async_trait;
use clap::ValueEnum;
use futures_util::{future::Either, SinkExt, StreamExt};
use shipstern::{
    sources::{FilterUpdateSource, SourceExitStatus, SourceTrait},
    CommitmentLevel, Error as ShipsternError,
};
use shipstern_core::{AccountsDataSlice, Filters, PrefilterError};
use tokio::sync::{mpsc::Sender, oneshot, watch};
use yellowstone_grpc_client::{Backoff, GeyserGrpcClient, ReconnectConfig, ReconnectEvent};
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

const fn default_auto_reconnect() -> bool { true }

/// Reconnect defaults, deliberately sturdier than the client library's
/// (3 retries / 10ms base), which gives up in under ~100ms. With these,
/// 10 retries doubling from 500ms cover outages up to ~4 minutes
/// (500ms, 1s, 2s, 4s, 8s, 16s, 32s, 64s, 128s, 256s).
const DEFAULT_RECONNECT_MAX_RETRIES: u32 = 10;
const DEFAULT_RECONNECT_INITIAL_BACKOFF: Duration = Duration::from_millis(500);
const DEFAULT_RECONNECT_MULTIPLIER: f64 = 2.0;

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

    ///
    /// Enable the client's auto-reconnect on the gRPC stream.
    ///
    /// Only a processed stream with no `from_slot` and no filter updates reconnects,
    /// because the client replays banks and cannot replay a changed request. Other
    /// streams stop on stream loss. A reconnect repeats the updates of the cut-off block.
    /// Defaults to `true`.
    ///
    #[arg(long, env, default_value_t = true)]
    #[serde(default = "default_auto_reconnect")]
    pub auto_reconnect: bool,

    /// Max reconnect attempts before the stream gives up.
    ///
    /// Only applies when `auto_reconnect` is set. Falls back to the client
    /// library default when unset.
    #[arg(long, env)]
    pub reconnect_max_retries: Option<u32>,
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

    /// Build the auto-reconnect config from the user-facing flags, with sturdier
    /// backoff defaults than the client library.
    ///
    /// Example output:
    /// ```rust, ignore
    /// // auto_reconnect = false
    /// assert!(config.reconnect_config().is_none());
    ///
    /// // auto_reconnect = true, no overrides -> sturdy defaults
    /// let rc = config.reconnect_config().unwrap();
    /// assert_eq!(rc.backoff.max_retries, 10);
    /// assert_eq!(rc.backoff.multiplier, 2.0);
    /// ```
    ///
    /// Returns `None` when auto-reconnect is off or the retry budget is zero, which
    /// the client treats the same way.
    pub fn reconnect_config(&self) -> Option<ReconnectConfig> {
        if !self.auto_reconnect {
            return None;
        }

        let max_retries = self
            .reconnect_max_retries
            .unwrap_or(DEFAULT_RECONNECT_MAX_RETRIES);

        if max_retries == 0 {
            return None;
        }

        let backoff = Backoff::new(
            DEFAULT_RECONNECT_INITIAL_BACKOFF,
            DEFAULT_RECONNECT_MULTIPLIER,
            max_retries,
        );

        Some(ReconnectConfig::default().with_backoff(backoff))
    }

    /// The client reconnects only a processed stream that starts at the tip and keeps
    /// its first request, so other setups must stop on stream loss.
    fn can_reconnect(&self, filter_updates: bool) -> bool {
        matches!(
            self.commitment_level,
            None | Some(CommitmentLevel::Processed)
        ) && self.from_slot.is_none()
            && !filter_updates
    }
}

/// A `Source` implementation for the Yellowstone gRPC API.
#[derive(Debug)]
pub struct YellowstoneGrpcSource {
    filters: Filters,
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
/// richat rejects the request outright if the set contains blocks. The client
/// library agrees, overwriting the field with the live checkpoint on
/// reconnect rather than reusing the configured value. Only the initial
/// subscribe sets it.
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

///
/// Unwrap a reconnect event into the update the runtime takes.
///
/// After a reconnect the client names the banks that were cut off, then replays them.
/// Updates already sent cannot be taken back, so handlers can see them twice.
///
async fn reconnect_update(
    event: Result<ReconnectEvent, Status>,
) -> Option<Result<SubscribeUpdate, Status>> {
    match event {
        Ok(ReconnectEvent::Update { update, .. }) => Some(Ok(update)),
        Ok(ReconnectEvent::DiscardBanks { banks, reason, .. }) => {
            tracing::warn!(
                bank_count = banks.len(),
                ?reason,
                "Reconnect replays banks that were cut off; handlers can see their updates twice"
            );
            None
        },
        Err(status) => Some(Err(status)),
    }
}

#[async_trait]
impl SourceTrait for YellowstoneGrpcSource {
    type Config = YellowstoneGrpcConfig;

    fn new(config: Self::Config, filters: Filters) -> Self { Self { config, filters } }

    async fn connect(
        &self,
        tx: Sender<Result<SubscribeUpdate, Status>>,
        status_tx: oneshot::Sender<SourceExitStatus>,
    ) -> Result<(), ShipsternError> {
        self.run(tx, status_tx, None).await
    }

    async fn connect_with_filter_updates(
        &self,
        tx: Sender<Result<SubscribeUpdate, Status>>,
        status_tx: oneshot::Sender<SourceExitStatus>,
        filter_updates_rx: watch::Receiver<Filters>,
    ) -> Result<(), ShipsternError> {
        self.run(tx, status_tx, Some(filter_updates_rx)).await
    }
}

impl FilterUpdateSource for YellowstoneGrpcSource {}

impl YellowstoneGrpcSource {
    /// Open the subscription and pump updates until the stream ends, sending
    /// each filter set published on `filter_updates_rx` to the server so it
    /// replaces the live subscription.
    ///
    /// `filter_updates_rx` is `None` when the caller never asked for updates,
    /// which is what `connect` passes.
    ///
    async fn run(
        &self,
        tx: Sender<Result<SubscribeUpdate, Status>>,
        status_tx: oneshot::Sender<SourceExitStatus>,
        mut filter_updates_rx: Option<watch::Receiver<Filters>>,
    ) -> Result<(), ShipsternError> {
        let filters = self.filters.clone();
        let config = self.config.clone();
        let timeout = Duration::from_secs(config.timeout);

        // The server refuses out-of-order or overlapping windows and answers
        // with an error instead of a stream, so check before dialing and say
        // which window is at fault.
        config
            .validate()
            .map_err(|err| ShipsternError::Other(Box::new(err)))?;

        let mut builder = GeyserGrpcClient::build_from_shared(config.endpoint.clone())?
            .x_token(config.x_token.clone())?
            .max_decoding_message_size(config.max_decoding_message_size.unwrap_or(usize::MAX))
            .accept_compressed(config.accept_compression.unwrap_or_default().into())
            .connect_timeout(timeout)
            .timeout(timeout)
            .tls_config(ClientTlsConfig::new().with_native_roots())?;

        // With no RuntimeHandle the update channel is already closed, so no filter
        // update can arrive and the client may reconnect the stream.
        let filter_updates = filter_updates_rx
            .as_ref()
            .is_some_and(|rx| rx.has_changed().is_ok());
        let reconnect_config = config.reconnect_config();
        let reconnect = reconnect_config.is_some() && config.can_reconnect(filter_updates);

        match reconnect_config {
            Some(reconnect_config) if reconnect => {
                tracing::debug!(?reconnect_config, "Auto-reconnect enabled");
                builder = builder.set_reconnect_config(reconnect_config);
            },
            Some(_) => tracing::warn!(
                commitment = ?config.commitment_level,
                from_slot = ?config.from_slot,
                filter_updates,
                "Auto-reconnect needs processed commitment, no from_slot and no filter \
                 updates; this stream stops on stream loss"
            ),
            None => {},
        }

        let mut client = builder.connect().await?;

        let mut subscribe_request = build_subscribe_request(filters, &config);
        subscribe_request.from_slot = config.from_slot;

        tracing::debug!(
            has_accounts = !subscribe_request.accounts.is_empty(),
            account_filters = ?subscribe_request.accounts.keys().collect::<Vec<_>>(),
            has_transactions = !subscribe_request.transactions.is_empty(),
            transaction_filters = ?subscribe_request.transactions.keys().collect::<Vec<_>>(),
            has_blocks_meta = !subscribe_request.blocks_meta.is_empty(),
            blocks_meta_filters = ?subscribe_request.blocks_meta.keys().collect::<Vec<_>>(),
            has_slots = !subscribe_request.slots.is_empty(),
            slots_filters = ?subscribe_request.slots.keys().collect::<Vec<_>>(),
            from_slot = ?subscribe_request.from_slot,
            commitment = ?subscribe_request.commitment,
            "Subscribing to gRPC stream"
        );

        let (mut sink, stream) = if reconnect {
            let (sink, stream) = client
                .subscribe_with_reconnect(Some(subscribe_request))
                .await?;
            (sink, Either::Left(stream.filter_map(reconnect_update)))
        } else {
            let (sink, stream) = client
                .subscribe_with_request(Some(subscribe_request))
                .await?;
            (sink, Either::Right(stream))
        };

        let mut stream = std::pin::pin!(stream);

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
                        let code = status.code();
                        let message = status.message().to_string();
                        let _ = tx.send(Err(status)).await;
                        break SourceExitStatus::StreamError { code, message };
                    },
                    None => {
                        break SourceExitStatus::StreamEnded;
                    },
                },

                update = next_filter_update(&mut filter_updates_rx) => {
                    let Some(filters) = update else {
                        // Every handle is gone and no new one can be taken, so retire this
                        // branch for the rest of the run.
                        tracing::debug!(
                            "Last runtime handle dropped, filter updates are off for this run"
                        );
                        filter_updates_rx = None;
                        continue;
                    };

                    send_filter_update(&mut sink, &config, filters, &mut filter_updates_sent)
                        .await;
                },
            }
        };

        let _ = status_tx.send(exit_status);

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};

    use shipstern_core::{AccountPrefilter, Filters, Prefilter, Pubkey};

    use super::{
        build_subscribe_request, send_filter_update, CommitmentLevel, SubscribeRequest,
        YellowstoneGrpcConfig,
    };

    fn config_from(toml_src: &str) -> YellowstoneGrpcConfig {
        toml::from_str(toml_src).expect("config must deserialize")
    }

    /// Stands in for the client's sink, which has private fields and no
    /// constructor.
    #[derive(Default)]
    struct TestSink {
        sent: Vec<SubscribeRequest>,
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
        type Error = std::convert::Infallible;

        fn poll_ready(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<(), Self::Error>> {
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

    /// A set that reaches the sink arrives as the subscription the caller asked
    /// for rather than an empty one, and advances the counter that tells an
    /// operator a stream error followed an update.
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

    /// The client replays banks on reconnect, so only a processed stream from the
    /// tip with a fixed request may reconnect.
    #[test]
    fn reconnect_needs_processed_tip_and_fixed_request() {
        let config = |extra: &str| {
            config_from(&format!(
                "endpoint = \"https://example.rpcpool.com\"\ntimeout = 60\n{extra}"
            ))
        };

        assert!(config("").can_reconnect(false));
        assert!(config("commitment-level = \"processed\"").can_reconnect(false));
        assert!(!config("").can_reconnect(true));
        assert!(!config("commitment-level = \"confirmed\"").can_reconnect(false));
        assert!(!config("from-slot = 10").can_reconnect(false));
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

    /// A config file predating the reconnect fields must still deserialize:
    /// missing `Option` keys become `None`, and the missing `auto-reconnect`
    /// key defaults to `true` via `#[serde(default = "default_auto_reconnect")]`,
    /// so legacy configs get auto-reconnect.
    #[test]
    fn deserializes_legacy_config_with_reconnect_on_by_default() {
        let legacy = r#"
            endpoint = "https://example.rpcpool.com"
            x-token = "secret"
            timeout = 60
        "#;

        let config: YellowstoneGrpcConfig =
            toml::from_str(legacy).expect("legacy config must deserialize");

        assert!(config.auto_reconnect);
        assert!(config.reconnect_config().is_some());
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

    /// Auto-reconnect can be explicitly disabled.
    #[test]
    fn reconnect_can_be_disabled() {
        let disabled = r#"
            endpoint = "https://example.rpcpool.com"
            timeout = 60
            auto-reconnect = false
        "#;

        let config: YellowstoneGrpcConfig =
            toml::from_str(disabled).expect("config must deserialize");

        assert!(!config.auto_reconnect);
        assert!(config.reconnect_config().is_none());
    }

    /// A zero retry budget stops the client on the first stream error, so it
    /// has to read as "no reconnect" here too.
    #[test]
    fn zero_retries_reads_as_no_reconnect() {
        let config: YellowstoneGrpcConfig = toml::from_str(
            r#"
            endpoint = "https://example.rpcpool.com"
            timeout = 60
            auto-reconnect = true
            reconnect-max-retries = 0
        "#,
        )
        .expect("config must deserialize");

        assert!(config.reconnect_config().is_none());
    }

    /// With no overrides, the helper yields the sturdy built-in defaults
    /// (not the weak library defaults).
    #[test]
    fn reconnect_config_uses_sturdy_defaults() {
        let config: YellowstoneGrpcConfig = toml::from_str(
            r#"
            endpoint = "https://example.rpcpool.com"
            timeout = 60
            auto-reconnect = true
        "#,
        )
        .expect("config must deserialize");

        let reconnect = config.reconnect_config().expect("auto-reconnect enabled");

        assert_eq!(
            reconnect.backoff.max_retries,
            super::DEFAULT_RECONNECT_MAX_RETRIES
        );
        assert_eq!(
            reconnect.backoff.multiplier,
            super::DEFAULT_RECONNECT_MULTIPLIER
        );
        assert_eq!(
            reconnect.backoff.initial_interval,
            super::DEFAULT_RECONNECT_INITIAL_BACKOFF
        );
    }

    /// Config overrides win over the built-in defaults.
    #[test]
    fn reconnect_config_applies_overrides() {
        let config: YellowstoneGrpcConfig = toml::from_str(
            r#"
            endpoint = "https://example.rpcpool.com"
            timeout = 60
            auto-reconnect = true
            reconnect-max-retries = 25
        "#,
        )
        .expect("config must deserialize");

        let reconnect = config.reconnect_config().expect("auto-reconnect enabled");

        assert_eq!(reconnect.backoff.max_retries, 25);
    }
}
