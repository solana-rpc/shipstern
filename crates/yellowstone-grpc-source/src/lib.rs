use std::{
    collections::{HashMap, HashSet},
    time::Duration,
};

use async_trait::async_trait;
use clap::ValueEnum;
use futures_util::{SinkExt, StreamExt};
use shipstern::{
    sources::{FilterUpdateSource, SourceExitStatus, SourceTrait},
    CommitmentLevel, Error as ShipsternError,
};
use shipstern_core::{AccountsDataSlice, Filters, PrefilterError, Pubkey};
use tokio::sync::{mpsc::Sender, oneshot, watch};
use yellowstone_grpc_client::{Backoff, GeyserGrpcClient, ReconnectConfig};
use yellowstone_grpc_proto::{
    geyser::{subscribe_update::UpdateOneof, SubscribeRequest, SubscribeUpdate},
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

    /// Receive only a window of each account's data instead of all of it.
    ///
    /// Applies to the whole subscription rather than to one parser, which is
    /// why it lives here and not on a `Prefilter`. Each entry is an
    /// `{ offset, length }` pair; the server returns those bytes for every
    /// account update on this connection. The windows must be given in
    /// ascending order of offset and must not overlap, which is checked on
    /// connect.
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
    /// When enabled, the stream reconnects with exponential backoff after a
    /// transient failure, resumes from the last seen slot, and deduplicates
    /// replayed events. The server must have `replay_stored_slots` configured
    /// for gap-free recovery.
    ///
    /// Defaults to `true`: a config file omitting this key (including ones that
    /// predate the field) gets auto-reconnect. Set to `false` to opt out.
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

    /// Send a parser's accounts as a cuckoo filter once it names at least this
    /// many, on yellowstone-grpc-geyser 13.1.0+ only. False positives are dropped
    /// before handlers, so they see the same updates. Unset, nothing is compressed.
    #[arg(long, env)]
    pub compress_account_filters_from: Option<usize>,
}

impl YellowstoneGrpcConfig {
    /// Check the settings the server would refuse the whole subscription over.
    ///
    /// Called before dialing so a bad data-slice window surfaces as a startup
    /// error naming the offending window, rather than as a stream that dies on
    /// subscribe. An embedder can call it earlier, as soon as the config is
    /// loaded.
    ///
    /// # Errors
    ///
    /// Returns an error if [`Self::accounts_data_slice`] is not in ascending
    /// order of offset, overlaps, or carries a zero-length window.
    ///
    pub fn validate(&self) -> Result<(), PrefilterError> {
        AccountsDataSlice::validate_all(&self.accounts_data_slice)
    }

    /// Build the auto-reconnect config from the user-facing flags.
    ///
    /// Uses sturdier backoff defaults than the client library (see
    /// [`DEFAULT_RECONNECT_MAX_RETRIES`] and friends); `reconnect_max_retries`
    /// and `reconnect_slot_retention` override the retry count and dedup window.
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
    /// Returns `None` when auto-reconnect is disabled, and when it is on with
    /// a retry budget of zero, which the client library treats the same way:
    /// it stops on the first stream error instead of reconnecting. Callers
    /// decide whether a rejected request can be recovered from this returning
    /// `Some`, so the two have to agree or a set is held for a recovery that
    /// cannot arrive.
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

/// The first release that matches `cuckoo_accounts_filter`. An older server
/// reads a cuckoo with no account list as "every account".
const COMPRESSED_ACCOUNTS_SINCE: (u64, u64, u64) = (13, 1, 0);
const COMPRESSED_ACCOUNTS_PACKAGE: &str = "yellowstone-grpc-geyser";

/// The part of the `GetVersion` JSON this reads.
#[derive(serde::Deserialize)]
struct ServerVersion {
    version: ServerVersionFields,
}

#[derive(serde::Deserialize)]
struct ServerVersionFields {
    package: String,
    version: String,
}

/// Whether a server reporting `reported` matches `cuckoo_accounts_filter`.
/// Anything unclear is a no, since a wrong yes subscribes to the whole cluster.
fn matches_compressed_accounts(reported: &str) -> bool {
    let Ok(ServerVersion { version }) = serde_json::from_str::<ServerVersion>(reported) else {
        return false;
    };

    if version.package != COMPRESSED_ACCOUNTS_PACKAGE {
        return false;
    }

    let Ok(reported) = semver::Version::parse(&version.version) else {
        return false;
    };

    let (major, minor, patch) = COMPRESSED_ACCOUNTS_SINCE;

    reported >= semver::Version::new(major, minor, patch)
}

/// Returns `threshold` only if the server says it matches cuckoo filters.
async fn negotiate_compressed_accounts(
    client: &mut GeyserGrpcClient,
    threshold: usize,
) -> Option<usize> {
    let reported = match client.get_version().await {
        Ok(response) => response.version,
        Err(err) => {
            tracing::warn!(
                %err,
                "Could not read the server version, so account filters stay uncompressed"
            );

            return None;
        },
    };

    if !matches_compressed_accounts(&reported) {
        tracing::warn!(
            server = %reported,
            "Server does not match cuckoo account filters, so they stay uncompressed"
        );

        return None;
    }

    tracing::debug!(
        server = %reported,
        threshold,
        "Compressing an account set naming at least this many keys"
    );

    Some(threshold)
}

/// Drops per window past which the server cannot be reading the cuckoo: its
/// false-positive rate is about 1 in 8k. Auto-reconnect can land on an older
/// pool member without asking the version again, and this catches that.
const DROP_BUDGET: usize = 10_000;
const DROP_BUDGET_WINDOW: Duration = Duration::from_secs(10);

/// Exact account sets behind the cuckoo filters on the live subscription,
/// keyed by parser id. Empty when nothing went out compressed.
#[derive(Debug, Default)]
struct CompressedAccounts {
    exact: HashMap<String, HashSet<Pubkey>>,
    dropped: usize,
    window_opened: Option<std::time::Instant>,
}

impl CompressedAccounts {
    fn new(filters: &Filters, compressed_ids: &HashSet<String>) -> Self {
        Self {
            exact: compressed_ids
                .iter()
                .filter_map(|parser_id| {
                    let account = filters.get(parser_id)?.account.as_ref()?;

                    Some((parser_id.clone(), account.accounts.clone()))
                })
                .collect(),
            dropped: 0,
            window_opened: None,
        }
    }

    /// Counted per window, since a long-lived connection accumulates honest
    /// false positives without bound.
    fn over_drop_budget(&mut self) -> bool {
        let now = std::time::Instant::now();

        match self.window_opened {
            Some(opened) if now.duration_since(opened) < DROP_BUDGET_WINDOW => {},
            _ => {
                self.window_opened = Some(now);
                self.dropped = 0;
            },
        }

        self.dropped += 1;

        self.dropped > DROP_BUDGET
    }

    /// Drop from `update` every compressed parser that did not name the
    /// account, and report whether any parser still matches. Other axes are
    /// still matched exactly server-side, so the account check is enough.
    fn retain_exact_matches(&self, update: &mut SubscribeUpdate) -> bool {
        if self.exact.is_empty() {
            return true;
        }

        let Some(UpdateOneof::Account(account)) = &update.update_oneof else {
            return true;
        };

        // Not a valid pubkey, so there is nothing to check it against.
        let Some(Ok(pubkey)) = account
            .account
            .as_ref()
            .map(|info| Pubkey::try_from_ref(&info.pubkey))
        else {
            return true;
        };

        update.filters.retain(|parser_id| {
            self.exact
                .get(parser_id)
                .is_none_or(|accounts| accounts.contains(&pubkey))
        });

        !update.filters.is_empty()
    }
}

/// Build the wire subscription for `filters`, layering on the commitment the
/// `From<Filters>` conversion leaves unset.
///
/// `compress_accounts_from` is the negotiated threshold, not the configured
/// one. Also returns the ids of the parsers that went out compressed.
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
fn build_subscribe_request(
    filters: Filters,
    config: &YellowstoneGrpcConfig,
    compress_accounts_from: Option<usize>,
) -> (SubscribeRequest, HashSet<String>) {
    let (mut request, compressed_ids) = match compress_accounts_from {
        Some(threshold) => filters.to_compressed_subscribe_request(threshold),
        None => (filters.into(), HashSet::new()),
    };

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

    (request, compressed_ids)
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

/// What became of a filter set handed to the sink.
#[derive(Debug)]
enum SendOutcome {
    /// The server has it. This is now the live set.
    Sent {
        filters: Filters,
        /// Parsers whose account list went out as a cuckoo filter.
        compressed_ids: HashSet<String>,
    },
    /// The sink refused it and a reconnect can still land it.
    Held(Filters),
    /// The sink refused it and nothing can land it, so it is gone.
    Dropped,
}

/// Record an accepted set as the live one and return whatever is still owed to
/// the server. A held set leaves the compression state alone, since the server
/// is still matching the previous one.
fn adopt(
    outcome: SendOutcome,
    live_filters: &mut Filters,
    compressed: &mut CompressedAccounts,
) -> Option<Filters> {
    match outcome {
        SendOutcome::Sent {
            filters,
            mut compressed_ids,
        } => {
            // Updates matched under the previous cuckoo can still be in flight,
            // so a parser once compressed stays checked.
            compressed_ids.extend(compressed.exact.keys().cloned());

            *compressed = CompressedAccounts::new(&filters, &compressed_ids);
            *live_filters = filters;

            None
        },
        SendOutcome::Held(filters) => Some(filters),
        SendOutcome::Dropped => None,
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

/// Send `filters` to the server, handing it back unsent when the sink rejects
/// it and a later retry can still land. A `Some` return is a set still owed to
/// the server, not a failure.
///
/// A rejection means the request channel is disconnected. With auto-reconnect
/// enabled, which is the default, that is a transient reconnect window rather
/// than a fatal error: the stream yields nothing during it and the client
/// library swaps a fresh sender into this sink once it recovers. Dropping the
/// set here would lose it silently, because the sink only records a request
/// into its reconnect state after a successful send, so the reconnect would
/// resubscribe with the previous filters.
///
/// That swap is the only thing that revives a rejected sink, and it belongs to
/// the reconnect connector. Without one the sink stays disconnected for the
/// rest of the run, so the set is dropped rather than held for a recovery that
/// cannot arrive.
///
async fn send_or_hold<S>(
    sink: &mut S,
    config: &YellowstoneGrpcConfig,
    filters: Filters,
    filter_updates_sent: &mut u64,
    attempt: SendAttempt,
    compress_accounts_from: Option<usize>,
) -> SendOutcome
where
    S: SinkExt<SubscribeRequest> + Unpin,
    S::Error: std::fmt::Display,
{
    let (request, compressed_ids) =
        build_subscribe_request(filters.clone(), config, compress_accounts_from);

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

            return SendOutcome::Dropped;
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

        return SendOutcome::Held(filters);
    }

    *filter_updates_sent += 1;

    SendOutcome::Sent {
        filters,
        compressed_ids,
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

        if let Some(reconnect_config) = config.reconnect_config() {
            tracing::debug!(?reconnect_config, "Auto-reconnect enabled");
            builder = builder.set_reconnect_config(reconnect_config);
        }

        let mut client = builder.connect().await?;

        let mut compress_accounts_from = match config.compress_account_filters_from {
            None => None,
            Some(threshold) => negotiate_compressed_accounts(&mut client, threshold).await,
        };

        let (mut subscribe_request, compressed_ids) =
            build_subscribe_request(filters.clone(), &config, compress_accounts_from);
        subscribe_request.from_slot = config.from_slot;

        let mut compressed = CompressedAccounts::new(&filters, &compressed_ids);

        // What a downgrade rebuilds from.
        let mut live_filters = filters;

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
                    Some(Ok(mut update)) => {
                        if !compressed.retain_exact_matches(&mut update) {
                            if compress_accounts_from.is_some() && compressed.over_drop_budget() {
                                tracing::warn!(
                                    dropped = DROP_BUDGET,
                                    window_secs = DROP_BUDGET_WINDOW.as_secs(),
                                    "Dropping far more accounts than a cuckoo filter can \
                                     explain, so this server is not matching one; resending \
                                     the subscription with explicit account lists"
                                );

                                compress_accounts_from = None;

                                // Held like any rejected set, so a failed resend retries
                                // on the timer instead of on every dropped update.
                                let filters =
                                    pending_filters.take().unwrap_or_else(|| live_filters.clone());

                                pending_filters = adopt(
                                    send_or_hold(
                                        &mut sink,
                                        &config,
                                        filters,
                                        &mut filter_updates_sent,
                                        SendAttempt::First,
                                        None,
                                    )
                                    .await,
                                    &mut live_filters,
                                    &mut compressed,
                                );

                                if pending_filters.is_some() {
                                    retry.reset();
                                }
                            }

                            continue;
                        }

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

                    pending_filters = adopt(
                        send_or_hold(
                            &mut sink,
                            &config,
                            filters,
                            &mut filter_updates_sent,
                            SendAttempt::Retry,
                            compress_accounts_from,
                        )
                            .await,
                        &mut live_filters,
                        &mut compressed,
                    );
                },

                update = next_filter_update(&mut filter_updates_rx) => {
                    let Some(filters) = update else {
                        // Every handle is gone, so retire the branch for the
                        // rest of the connection. `Runtime::handle` borrows the
                        // runtime and every `run` consumes it, so no further
                        // handle can be taken: this is permanent for the run.
                        tracing::debug!(
                            "Last runtime handle dropped, filter updates are off for this run"
                        );
                        filter_updates_rx = None;
                        continue;
                    };

                    // A newer set supersedes anything still held, because every
                    // set is complete rather than a delta.
                    pending_filters = adopt(
                        send_or_hold(
                            &mut sink,
                            &config,
                            filters,
                            &mut filter_updates_sent,
                            SendAttempt::First,
                            compress_accounts_from,
                        )
                            .await,
                        &mut live_filters,
                        &mut compressed,
                    );

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
        adopt, build_subscribe_request, matches_compressed_accounts, send_or_hold, CommitmentLevel,
        CompressedAccounts, SendAttempt, SendOutcome, SubscribeRequest, SubscribeUpdate,
        UpdateOneof, YellowstoneGrpcConfig,
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

    /// Only asks whether a set was held, with compression off.
    async fn send(
        sink: &mut TestSink,
        config: &YellowstoneGrpcConfig,
        filters: Filters,
        sent: &mut u64,
        attempt: SendAttempt,
    ) -> Option<Filters> {
        let mut live = Filters::new(HashMap::new());
        let mut compressed = CompressedAccounts::default();

        adopt(
            send_or_hold(sink, config, filters, sent, attempt, None).await,
            &mut live,
            &mut compressed,
        )
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

        let held = send(
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

        let held = send(
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

        let held = send(
            &mut sink,
            &reconnecting_config(),
            filters_owned_by(3),
            &mut sent,
            SendAttempt::First,
        )
        .await;

        let held = held.expect("set must be held while the sink is down");

        sink.recover();

        let still_held = send(
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

        let held = send(
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

        let request = build_subscribe_request(Filters::new(HashMap::new()), &config, None).0;

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

        let request = build_subscribe_request(Filters::new(HashMap::new()), &config, None).0;

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

        let request = build_subscribe_request(Filters::new(HashMap::new()), &config, None).0;

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

        let request = build_subscribe_request(Filters::new(HashMap::new()), &config, None).0;
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

        let request = build_subscribe_request(Filters::new(HashMap::new()), &config, None).0;

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

    /// richat is built against a proto with the field but is not geyser, so a
    /// version check alone would let it through.
    #[test]
    fn only_geyser_13_1_or_newer_gets_a_cuckoo() {
        let geyser = |version| {
            format!(
                r#"{{"version":{{"package":"yellowstone-grpc-geyser","version":"{version}"}}}}"#
            )
        };

        assert!(matches_compressed_accounts(&geyser("13.1.0")));
        assert!(matches_compressed_accounts(&geyser(
            "16.0.0-rc9+solana.4.3.0.rc.1"
        )));
        assert!(!matches_compressed_accounts(&geyser("13.1.0-rc1")));
        assert!(!matches_compressed_accounts(&geyser("13.0.9")));
        assert!(!matches_compressed_accounts(
            r#"{"version":{"package":"richat","version":"13.0.2"}}"#
        ));
        assert!(!matches_compressed_accounts("not json"));
    }

    fn account_update(pubkey: Pubkey, filters: &[&str]) -> SubscribeUpdate {
        SubscribeUpdate {
            filters: filters.iter().map(|f| (*f).to_owned()).collect(),
            created_at: None,
            update_oneof: Some(UpdateOneof::Account(
                yellowstone_grpc_proto::geyser::SubscribeUpdateAccount {
                    account: Some(yellowstone_grpc_proto::geyser::SubscribeUpdateAccountInfo {
                        pubkey: pubkey.to_vec(),
                        ..Default::default()
                    }),
                    slot: 1,
                    is_startup: false,
                },
            )),
        }
    }

    fn compressed_for(parser_id: &str, accounts: &[Pubkey]) -> CompressedAccounts {
        let filters = Filters::new(HashMap::from([(parser_id.to_owned(), Prefilter {
            account: Some(AccountPrefilter {
                accounts: accounts.iter().copied().collect(),
                ..Default::default()
            }),
            ..Default::default()
        })]));

        CompressedAccounts::new(&filters, &HashSet::from([parser_id.to_owned()]))
    }

    #[test]
    fn false_positives_are_dropped_before_the_handler() {
        let watched = Pubkey::new([1; 32]);
        let compressed = compressed_for("compressed", &[watched]);

        let mut hit = account_update(watched, &["compressed", "exact"]);
        assert!(compressed.retain_exact_matches(&mut hit));
        assert_eq!(hit.filters, ["compressed", "exact"]);

        let mut shared = account_update(Pubkey::new([2; 32]), &["compressed", "exact"]);
        assert!(compressed.retain_exact_matches(&mut shared));
        assert_eq!(shared.filters, ["exact"]);

        let mut miss = account_update(Pubkey::new([2; 32]), &["compressed"]);
        assert!(!compressed.retain_exact_matches(&mut miss));
    }

    /// Updates the old server matched under the cuckoo are still in flight
    /// after the explicit resend lands, so they still have to be dropped.
    #[test]
    fn an_explicit_resend_keeps_dropping_accounts_outside_the_set() {
        let watched = Pubkey::new([1; 32]);
        let mut compressed = compressed_for("compressed", &[watched]);

        let filters = Filters::new(HashMap::from([("compressed".to_owned(), Prefilter {
            account: Some(AccountPrefilter {
                accounts: HashSet::from([watched]),
                ..Default::default()
            }),
            ..Default::default()
        })]));

        let mut live = Filters::new(HashMap::new());

        let resent = SendOutcome::Sent {
            filters,
            compressed_ids: HashSet::new(),
        };

        assert!(adopt(resent, &mut live, &mut compressed).is_none());

        let mut in_flight = account_update(Pubkey::new([2; 32]), &["compressed"]);
        assert!(!compressed.retain_exact_matches(&mut in_flight));
    }
}
