use std::time::Duration;

use async_trait::async_trait;
use clap::ValueEnum;
use futures_util::{SinkExt, StreamExt};
use shipstern::{
    sources::{FilterUpdateSource, SourceExitStatus, SourceTrait},
    CommitmentLevel, Error as ShipsternError,
};
use shipstern_core::{AccountsDataSlice, Filters, PrefilterError};
use tokio::sync::{mpsc::Sender, oneshot, watch};
use yellowstone_grpc_client::{Backoff, GeyserGrpcClient, ReconnectConfig};
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

    /// Enable the client's built-in auto-reconnect on the gRPC stream.
    ///
    /// The stream reconnects with backoff, resumes from the last seen slot, and
    /// dedups replayed events; gap-free recovery needs `replay_stored_slots` on the
    /// server. Defaults to `true`, including for configs that omit the key.
    #[arg(long, env, default_value_t = true)]
    #[serde(default = "default_auto_reconnect")]
    pub auto_reconnect: bool,

    /// Max reconnect attempts before the stream gives up.
    ///
    /// Only applies when `auto_reconnect` is set. Falls back to the client
    /// library default when unset.
    #[arg(long, env)]
    pub reconnect_max_retries: Option<u32>,

    /// Number of recent slots retained for dedup during the replay window.
    ///
    /// Only applies when `auto_reconnect` is set. Falls back to the client
    /// library default when unset.
    #[arg(long, env)]
    pub reconnect_slot_retention: Option<usize>,
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
    /// the client treats the same way. Callers use this to decide whether a
    /// rejected request can be held for recovery.
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

        // Start from the library default for fields we keep (slot_retention),
        // then swap in the sturdier backoff.
        let mut config = ReconnectConfig::default().with_backoff(backoff);

        if let Some(slot_retention) = self.reconnect_slot_retention {
            config.slot_retention = slot_retention;
        }

        Some(config)
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

/// Whether a send is the first attempt at a set or a retry of one already
/// held, which is the only thing separating a loud rejection from a quiet one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SendAttempt {
    /// A set the caller just published.
    First,
    /// A set the retry timer is re-offering.
    Retry,
}

/// How often a filter set held after a sink rejection is retried.
const RETRY_HELD_FILTERS_EVERY: Duration = Duration::from_secs(5);

/// Send `filters` to the server, handing it back unsent when the sink rejects it
/// and a retry can still land. `Some` means a set still owed, not a failure.
///
/// A rejection means the channel is disconnected. With auto-reconnect the client
/// later swaps in a fresh sender, so the set is held; without it nothing can
/// recover, so the set is dropped.
///
async fn send_or_hold<S>(
    sink: &mut S,
    config: &YellowstoneGrpcConfig,
    filters: Filters,
    filter_updates_sent: &mut u64,
    attempt: SendAttempt,
) -> Option<Filters>
where
    S: SinkExt<SubscribeRequest> + Unpin,
    S::Error: std::fmt::Display,
{
    let request = build_subscribe_request(filters.clone(), config);

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
        // Whether the set can be held at all depends on the connection, not on
        // which attempt this is, so decide that first. Holding on a sink that
        // cannot recover would retry every 5s for the rest of the run.
        if config.reconnect_config().is_none() {
            tracing::warn!(
                %err,
                "Filter update rejected by the sink and dropped, since auto-reconnect is off \
                 and the subscription cannot recover; it keeps its previous filters"
            );

            return None;
        }

        // A retry re-offers a set already reported, so it stays quiet. The
        // sturdy reconnect defaults span minutes, and one line per 5s tick
        // would bury the original rejection.
        match attempt {
            SendAttempt::First => tracing::warn!(
                %err,
                "Filter update rejected by the sink, holding it until the stream recovers"
            ),
            SendAttempt::Retry => {
                tracing::debug!(%err, "Filter update still rejected, holding it");
            },
        }

        return Some(filters);
    }

    *filter_updates_sent += 1;

    None
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

        if let Some(reconnect_config) = config.reconnect_config() {
            tracing::debug!(?reconnect_config, "Auto-reconnect enabled");
            builder = builder.set_reconnect_config(reconnect_config);
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

        let (mut sink, stream) = client
            .subscribe_with_request(Some(subscribe_request))
            .await?;

        let mut stream = std::pin::pin!(stream);

        tracing::debug!("gRPC stream started");

        let mut pending_filters: Option<Filters> = None;
        let mut filter_updates_sent: u64 = 0;

        // A held set cannot wait on the stream to produce again. The filter it
        // is replacing may match nothing, and the client swallows its own
        // keepalive messages rather than yielding them, so there are live
        // connections on which no update ever arrives to retry from.
        let mut retry = tokio::time::interval(RETRY_HELD_FILTERS_EVERY);
        retry.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

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
                        //
                        // Only for a code the client treats as terminal, though.
                        // The sink records a request into the reconnect state as
                        // soon as the local channel takes it, before the server has
                        // seen it, so a set refused with a recoverable code is
                        // resubscribed on reconnect and refused again without ever
                        // reaching this arm.
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

                _ = retry.tick(), if pending_filters.is_some() => {
                    let Some(filters) = pending_filters.take() else { continue };

                    pending_filters =
                        send_or_hold(
                            &mut sink,
                            &config,
                            filters,
                            &mut filter_updates_sent,
                            SendAttempt::Retry,
                        )
                            .await;
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

                    // A newer set supersedes anything still held, because every
                    // set is complete rather than a delta.
                    pending_filters =
                        send_or_hold(
                            &mut sink,
                            &config,
                            filters,
                            &mut filter_updates_sent,
                            SendAttempt::First,
                        )
                            .await;

                    // An interval's first tick is immediate, so without this a
                    // rejected set retries at once, inside the same reconnect
                    // window that just rejected it.
                    if pending_filters.is_some() {
                        retry.reset();
                    }
                },
            }
        };

        if pending_filters.is_some() {
            tracing::warn!("Connection ended with a filter update still unsent");
        }

        let _ = status_tx.send(exit_status);

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};

    use shipstern_core::{AccountPrefilter, Filters, Prefilter, Pubkey};

    use super::{
        build_subscribe_request, send_or_hold, CommitmentLevel, SendAttempt, SubscribeRequest,
        YellowstoneGrpcConfig,
    };

    fn config_from(toml_src: &str) -> YellowstoneGrpcConfig {
        toml::from_str(toml_src).expect("config must deserialize")
    }

    /// Stands in for the client's sink, which has private fields and no
    /// constructor. `disconnected` reproduces a request channel that has gone
    /// away, which is the only way a real send fails.
    #[derive(Default)]
    struct TestSink {
        disconnected: bool,
        sent: Vec<SubscribeRequest>,
    }

    impl TestSink {
        fn disconnected() -> Self {
            Self {
                disconnected: true,
                sent: Vec::new(),
            }
        }

        /// Stands in for the reconnect connector swapping a fresh sender in.
        fn recover(&mut self) { self.disconnected = false; }
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

    #[derive(Debug)]
    struct Disconnected;

    impl std::fmt::Display for Disconnected {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("send failed because the receiver is gone")
        }
    }

    impl futures_util::Sink<SubscribeRequest> for TestSink {
        type Error = Disconnected;

        fn poll_ready(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<(), Self::Error>> {
            if self.disconnected {
                return std::task::Poll::Ready(Err(Disconnected));
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

    fn reconnecting_config() -> YellowstoneGrpcConfig {
        config_from(
            r#"
            endpoint = "https://example.rpcpool.com"
            timeout = 60
        "#,
        )
    }

    fn non_reconnecting_config() -> YellowstoneGrpcConfig {
        config_from(
            r#"
            endpoint = "https://example.rpcpool.com"
            timeout = 60
            auto-reconnect = false
        "#,
        )
    }

    /// A set that reaches the sink is not held, arrives as the subscription the
    /// caller asked for rather than an empty one, and advances the counter that
    /// tells an operator a stream error followed an update.
    #[tokio::test]
    async fn accepted_filter_update_reaches_the_sink_intact() {
        let mut sink = TestSink::default();
        let mut sent = 0;

        let held = send_or_hold(
            &mut sink,
            &reconnecting_config(),
            filters_owned_by(1),
            &mut sent,
            SendAttempt::First,
        )
        .await;

        assert!(held.is_none());
        assert_eq!(sent, 1);
        assert_eq!(sink.sent.len(), 1);
        assert_eq!(received_owners(&sink.sent[0]), [
            Pubkey::new([1; 32]).to_string()
        ]);
    }

    /// A rejected send means the request channel is disconnected, which with
    /// auto-reconnect on is a reconnect window rather than a fatal error. The
    /// set has to come back so the retry timer can land it once the client
    /// swaps a fresh sender in; dropping it would resubscribe with the old
    /// filters.
    #[tokio::test]
    async fn rejected_filter_update_is_held_when_reconnect_can_recover() {
        let mut sink = TestSink::disconnected();
        let mut sent = 0;

        let held = send_or_hold(
            &mut sink,
            &reconnecting_config(),
            filters_owned_by(2),
            &mut sent,
            SendAttempt::First,
        )
        .await;

        assert!(held.is_some(), "set must be held for the retry timer");
        assert_eq!(sent, 0);
        assert!(sink.sent.is_empty());
    }

    /// The whole reason a rejected set is held rather than dropped: once the
    /// connector swaps a working sender in, the retry has to land the set the
    /// caller asked for, not the one the subscription already had.
    #[tokio::test]
    async fn held_filter_update_lands_once_the_sink_recovers() {
        let mut sink = TestSink::disconnected();
        let mut sent = 0;

        let held = send_or_hold(
            &mut sink,
            &reconnecting_config(),
            filters_owned_by(3),
            &mut sent,
            SendAttempt::First,
        )
        .await;

        let held = held.expect("set must be held while the sink is down");

        sink.recover();

        let still_held = send_or_hold(
            &mut sink,
            &reconnecting_config(),
            held,
            &mut sent,
            SendAttempt::Retry,
        )
        .await;

        assert!(still_held.is_none(), "retry must land the held set");
        assert_eq!(sent, 1);
        assert_eq!(sink.sent.len(), 1);
        assert_eq!(received_owners(&sink.sent[0]), [
            Pubkey::new([3; 32]).to_string()
        ]);
    }

    /// Nothing revives a rejected sink without the reconnect connector, so
    /// holding the set would wait for a recovery that cannot arrive.
    #[tokio::test]
    async fn rejected_filter_update_is_dropped_when_reconnect_is_off() {
        let mut sink = TestSink::disconnected();
        let mut sent = 0;

        let held = send_or_hold(
            &mut sink,
            &non_reconnecting_config(),
            filters_owned_by(4),
            &mut sent,
            SendAttempt::First,
        )
        .await;

        assert!(held.is_none(), "set must be dropped, not held forever");
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
    /// has to read as "no reconnect" here too. `send_or_hold` decides whether
    /// a rejected set can be recovered from this, and holding one for a
    /// recovery that cannot arrive retries it every 5s for the rest of the run.
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

        assert!(
            config.reconnect_config().is_none(),
            "a zero retry budget cannot recover a rejected sink"
        );
    }

    /// With no overrides, the helper yields the sturdy built-in defaults
    /// (not the weak library defaults), and keeps the library slot_retention.
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
        // slot_retention is not overridden, so it keeps the library default.
        assert_eq!(
            reconnect.slot_retention,
            yellowstone_grpc_client::ReconnectConfig::default().slot_retention
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
            reconnect-slot-retention = 300
        "#,
        )
        .expect("config must deserialize");

        let reconnect = config.reconnect_config().expect("auto-reconnect enabled");

        assert_eq!(reconnect.backoff.max_retries, 25);
        assert_eq!(reconnect.slot_retention, 300);
    }
}
