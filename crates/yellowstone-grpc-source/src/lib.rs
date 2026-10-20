mod resume;

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
use yellowstone_grpc_client::{Backoff, GeyserGrpcClient, ReconnectConfig, SubscribeRequestSink};
use yellowstone_grpc_proto::{
    geyser::{SlotStatus, SubscribeRequest, SubscribeUpdate},
    tonic::{codec::CompressionEncoding, transport::ClientTlsConfig, Code, Status},
};

use crate::resume::{add_resume_filter, strip_resume_filter, ResumePoint};

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
/// (3 retries / 10ms base), which gives up in under ~100ms. `SlotResume` doubles
/// from 500ms and caps each wait at 10s, so 10 retries span about 65s
/// (500ms, 1s, 2s, 4s, 8s, then 10s each). Recovery also needs
/// the server to still hold the missed slots, or the run ends with `OutOfRange`.
const DEFAULT_RECONNECT_MAX_RETRIES: u32 = 10;
const DEFAULT_RECONNECT_INITIAL_BACKOFF: Duration = Duration::from_millis(500);
const DEFAULT_RECONNECT_MULTIPLIER: f64 = 2.0;
/// The longest wait between two `SlotResume` attempts. Uncapped, the eighth wait
/// alone is 64s, longer than many servers hold a slot for replay.
const MAX_RECONNECT_DELAY: Duration = Duration::from_secs(10);

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
    /// Reconnect a lost gRPC stream. Defaults to `true`. See [`ReconnectMode`].
    ///
    /// At confirmed or finalized the source subscribes again from the last slot it saw, and
    /// adds a `slots` filter so a slot is on record even before the first matching update. At
    /// processed the stream stops on loss: handlers cannot retract an invalidated bank or
    /// verify complete replacement delivery. Set this to `false` for a server without
    /// stored slots.
    ///
    #[arg(long, env, default_value_t = true)]
    #[serde(default = "default_auto_reconnect")]
    pub auto_reconnect: bool,

    /// Max reconnect attempts before the stream gives up.
    ///
    /// Only applies to confirmed/finalized streams with `auto_reconnect` set.
    /// Defaults to 10 when unset.
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
        self.backoff()
            .map(|backoff| ReconnectConfig::default().with_backoff(backoff))
    }

    /// The retry budget and delays both reconnect modes use. `None` when auto-reconnect
    /// is off or the budget is zero.
    pub fn backoff(&self) -> Option<Backoff> {
        if !self.auto_reconnect {
            return None;
        }

        let max_retries = self
            .reconnect_max_retries
            .unwrap_or(DEFAULT_RECONNECT_MAX_RETRIES);

        if max_retries == 0 {
            return None;
        }

        Some(Backoff::new(
            DEFAULT_RECONNECT_INITIAL_BACKOFF,
            DEFAULT_RECONNECT_MULTIPLIER,
            max_retries,
        ))
    }

    /// How a lost stream is recovered. Processed streams stop on loss. The handle argument
    /// is retained for compatibility.
    pub fn reconnect_mode(&self, _has_handle: bool) -> ReconnectMode {
        if self.backoff().is_none() {
            return ReconnectMode::Off;
        }
        match self.commitment_level {
            Some(CommitmentLevel::Confirmed | CommitmentLevel::Finalized) => {
                ReconnectMode::SlotResume
            },
            _ => ReconnectMode::Off,
        }
    }
}

/// How the source recovers a lost stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReconnectMode {
    /// Retained for compatibility. The source does not select bank replay because handlers
    /// cannot retract invalidated banks or verify complete replacement delivery.
    BankReplay,
    /// The source subscribes again from the last slot it saw and drops that slot's repeats.
    /// A confirmed or finalized slot never changes once delivered, so this is exact. It adds
    /// its own `slots` filter so the last slot is known even when nothing else matched, takes
    /// a `from_slot` and filter updates, and ends the run with `OutOfRange` when the server no
    /// longer holds the slot.
    SlotResume,
    /// The stream stops on loss.
    Off,
}

/// Updates from one connection, whichever subscribe call opened it.
type UpdateStream = BoxStream<'static, Result<SubscribeUpdate, Status>>;

/// How the stream of one connection stopped.
enum StreamStop {
    /// The run is over.
    Exit(SourceExitStatus),
    /// The stream was lost, and a resubscribe can recover it.
    Lost(Option<Status>),
}

/// A loss the network or the server caused, which a new subscription can recover from.
/// `OutOfRange` is not one: the server no longer holds the checkpoint slot. Nor is the
/// `Internal` a server without replay answers `from_slot` with: every retry sends it again.
fn is_recoverable(status: &Status) -> bool {
    if status.code() == Code::Internal && status.message().contains("from_slot is not supported") {
        return false;
    }
    matches!(
        status.code(),
        Code::Unavailable
            | Code::Internal
            | Code::Unknown
            | Code::DeadlineExceeded
            | Code::Cancelled
            | Code::Aborted
            | Code::ResourceExhausted
    )
}

/// The wait before reconnect attempt `attempt`, counted from 1, capped at `MAX_RECONNECT_DELAY`.
fn retry_delay(backoff: &Backoff, attempt: u32) -> Duration {
    let exponent = attempt.saturating_sub(1).min(i32::MAX as u32) as i32;
    backoff
        .initial_interval
        .mul_f64(backoff.multiplier.powi(exponent))
        .min(MAX_RECONNECT_DELAY)
}

/// Count a loss and wait out its backoff. False once the budget is spent.
async fn wait_before_retry(backoff: &Backoff, attempts: &mut u32, from_slot: Option<u64>) -> bool {
    *attempts += 1;
    if *attempts > backoff.max_retries {
        tracing::error!(
            attempts = backoff.max_retries,
            "Stream lost and the reconnect budget is spent; stopping"
        );
        return false;
    }

    let delay = retry_delay(backoff, *attempts);
    tracing::warn!(
        attempt = *attempts,
        max_retries = backoff.max_retries,
        ?delay,
        ?from_slot,
        "Stream lost; subscribing again from the last seen slot"
    );
    tokio::time::sleep(delay).await;
    true
}

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

/// Dial the server and subscribe. Recovery, when enabled, is managed by the source.
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
/// richat rejects the request outright if the set contains blocks. The client
/// library agrees, overwriting the field with the live checkpoint on
/// reconnect rather than reusing the configured value. Only the initial
/// subscribe sets it.
///
/// `SlotResume` adds its own `slots` filter on every request, so a resubscribe
/// has a slot to start from even when no update matched the user's filters yet.
///
fn build_subscribe_request(
    filters: Filters,
    config: &YellowstoneGrpcConfig,
    mode: ReconnectMode,
) -> SubscribeRequest {
    let mut request: SubscribeRequest = filters.into();

    if mode == ReconnectMode::SlotResume {
        add_resume_filter(&mut request);
    }

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
    mode: ReconnectMode,
    filters: Filters,
    filter_updates_sent: &mut u64,
) where
    S: SinkExt<SubscribeRequest> + Unpin,
    S::Error: std::fmt::Display,
{
    let request = build_subscribe_request(filters, config, mode);

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
    /// Open the subscription and pump updates until the stream ends, sending
    /// each filter set published on the context's update slot to the server so
    /// it replaces the live subscription. In `SlotResume` mode a lost stream is
    /// opened again from the last slot seen, with the newest filter set.
    ///
    async fn connect(&self, ctx: SourceContext) -> Result<SourceExitStatus, ShipsternError> {
        let SourceContext {
            filters,
            tx,
            mut filter_updates,
            ..
        } = ctx;

        let config = self.config.clone();

        // The server refuses out-of-order or overlapping windows and answers
        // with an error instead of a stream, so check before dialing and say
        // which window is at fault.
        config
            .validate()
            .map_err(|err| ShipsternError::Other(Box::new(err)))?;

        // A closed filter channel means no RuntimeHandle can publish more updates.
        let has_handle = filter_updates.has_changed().is_ok();

        // A closed slot can still hold one set that the runtime did not read.
        let mut filters = if has_handle {
            filters
        } else {
            filter_updates.borrow_and_update().clone()
        };
        let mut filter_updates_rx = Some(filter_updates);

        let mode = config.reconnect_mode(has_handle);
        match mode {
            ReconnectMode::Off
                if config.auto_reconnect
                    && matches!(
                        config.commitment_level,
                        None | Some(CommitmentLevel::Processed)
                    ) =>
            {
                tracing::info!(
                    commitment = ?config.commitment_level,
                    from_slot = ?config.from_slot,
                    has_handle,
                    "Processed streams stop on loss; bank replay requires handler invalidation \
                     and complete replacement delivery"
                )
            },
            mode => tracing::debug!(?mode, "Auto-reconnect mode"),
        }
        let backoff = (mode == ReconnectMode::SlotResume)
            .then(|| config.backoff())
            .flatten();

        let checkpoint_status = match config.commitment_level {
            Some(CommitmentLevel::Finalized) => SlotStatus::SlotFinalized,
            _ => SlotStatus::SlotConfirmed,
        };
        let mut resume = ResumePoint::new(checkpoint_status);
        // Only the first subscribe takes the configured start. A resubscribe takes the
        // checkpoint, and until there is one, the same start.
        let mut from_slot = config.from_slot;
        let mut attempts: u32 = 0;
        let mut filter_updates_sent: u64 = 0;

        let exit_status = loop {
            let mut request = build_subscribe_request(filters.clone(), &config, mode);
            request.from_slot = from_slot;

            let (mut sink, mut stream) = match open_stream(&config, request).await {
                Ok(opened) => opened,
                Err(err) => {
                    // A failed reopen counts like a loss. A failed first connect is the caller's.
                    let retry = match &backoff {
                        Some(backoff) if attempts > 0 => {
                            wait_before_retry(backoff, &mut attempts, from_slot).await
                        },
                        _ => false,
                    };
                    if retry {
                        continue;
                    }
                    return Err(err);
                },
            };

            tracing::debug!("gRPC stream started");

            let stop = loop {
                tokio::select! {
                    update = stream.next() => match update {
                        Some(Ok(mut update)) => {
                            // A delivered update shows the connection works, so the budget
                            // starts over.
                            attempts = 0;

                            // A repeat of the checkpoint slot, or a slot status only the
                            // resume filter asked for, stays here.
                            if mode == ReconnectMode::SlotResume
                                && (!resume.deliver(&update) || !strip_resume_filter(&mut update))
                            {
                                continue;
                            }

                            if tx.send(Ok(update)).await.is_err() {
                                tracing::info!("Receiver dropped, stopping source");
                                // Defensive only - normally unreachable because Signal/Buffer
                                // branch wins first when receiver drops.
                                break StreamStop::Exit(SourceExitStatus::ReceiverDropped);
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

                            if mode == ReconnectMode::SlotResume && is_recoverable(&status) {
                                break StreamStop::Lost(Some(status));
                            }
                            break StreamStop::Exit(stream_error(&tx, status).await);
                        },
                        None if mode == ReconnectMode::SlotResume => break StreamStop::Lost(None),
                        None => break StreamStop::Exit(SourceExitStatus::StreamEnded),
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

                        // A resubscribe must carry the newest set too.
                        filters = new_filters.clone();
                        send_filter_update(
                            &mut sink,
                            &config,
                            mode,
                            new_filters,
                            &mut filter_updates_sent,
                        )
                        .await;
                    },
                }
            };

            let lost = match stop {
                StreamStop::Exit(status) => break status,
                StreamStop::Lost(lost) => lost,
            };
            from_slot = resume.checkpoint().or(from_slot);
            let retry = match &backoff {
                Some(backoff) => wait_before_retry(backoff, &mut attempts, from_slot).await,
                None => false,
            };
            if !retry {
                break match lost {
                    Some(status) => stream_error(&tx, status).await,
                    None => SourceExitStatus::StreamEnded,
                };
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
    use std::{
        collections::{HashMap, HashSet},
        time::Duration,
    };

    use shipstern_core::{AccountPrefilter, Filters, Prefilter, Pubkey};
    use yellowstone_grpc_client::Backoff;
    use yellowstone_grpc_proto::tonic::Status;

    use super::{
        build_subscribe_request, is_recoverable, retry_delay, send_filter_update, CommitmentLevel,
        ReconnectMode, SubscribeRequest, YellowstoneGrpcConfig,
    };
    use crate::resume::RESUME_FILTER_KEY;

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

        send_filter_update(
            &mut sink,
            &config,
            ReconnectMode::Off,
            filters_owned_by(1),
            &mut sent,
        )
        .await;

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

        send_filter_update(
            &mut sink,
            &config,
            ReconnectMode::Off,
            filters_owned_by(1),
            &mut sent,
        )
        .await;

        assert_eq!(sent, 0);
        assert!(sink.sent.is_empty());
    }

    /// Processed streams stop on loss for every request; committed streams can resume.
    #[test]
    fn reconnect_mode_follows_commitment_and_request() {
        let config = |extra: &str| {
            config_from(&format!(
                "endpoint = \"https://example.rpcpool.com\"\ntimeout = 60\n{extra}"
            ))
        };

        assert_eq!(config("").reconnect_mode(false), ReconnectMode::Off);
        assert_eq!(config("").reconnect_mode(true), ReconnectMode::Off);
        assert_eq!(
            config("from-slot = 10").reconnect_mode(false),
            ReconnectMode::Off
        );
        assert_eq!(
            config("commitment-level = \"confirmed\"").reconnect_mode(false),
            ReconnectMode::SlotResume
        );
        assert_eq!(
            config("commitment-level = \"finalized\"\nfrom-slot = 10").reconnect_mode(true),
            ReconnectMode::SlotResume
        );
        assert_eq!(
            config("commitment-level = \"finalized\"\nauto-reconnect = false")
                .reconnect_mode(false),
            ReconnectMode::Off
        );
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

        let request =
            build_subscribe_request(Filters::new(HashMap::new()), &config, ReconnectMode::Off);

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

        let request =
            build_subscribe_request(Filters::new(HashMap::new()), &config, ReconnectMode::Off);

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

        let request =
            build_subscribe_request(Filters::new(HashMap::new()), &config, ReconnectMode::Off);

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

        let request =
            build_subscribe_request(Filters::new(HashMap::new()), &config, ReconnectMode::Off);
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

        let request =
            build_subscribe_request(Filters::new(HashMap::new()), &config, ReconnectMode::Off);

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

    /// The wait doubles until the cap, so a stream cut for a minute is retried while the
    /// server still holds the slot, instead of once at two minutes.
    #[test]
    fn retry_delay_doubles_then_caps() {
        let backoff = Backoff::new(Duration::from_millis(500), 2.0, 10);

        assert_eq!(retry_delay(&backoff, 1), Duration::from_millis(500));
        assert_eq!(retry_delay(&backoff, 5), Duration::from_secs(8));
        assert_eq!(retry_delay(&backoff, 6), Duration::from_secs(10));
        assert_eq!(retry_delay(&backoff, 10), Duration::from_secs(10));
    }

    /// A server without replay rejects every `from_slot` the same way, so retrying it only
    /// spends the budget.
    #[test]
    fn unsupported_from_slot_is_not_retried() {
        assert!(is_recoverable(&Status::internal("connection reset")));
        assert!(!is_recoverable(&Status::internal(
            "from_slot is not supported"
        )));
    }

    /// Only `SlotResume` asks for slot statuses on its own, and it does so on every request,
    /// so a filter update does not drop the subscription that moves the checkpoint.
    #[test]
    fn slot_resume_requests_carry_the_resume_filter() {
        let config = config_from(
            r#"
            endpoint = "https://example.rpcpool.com"
            timeout = 60
            commitment-level = "confirmed"
        "#,
        );

        let request = build_subscribe_request(
            Filters::new(HashMap::new()),
            &config,
            ReconnectMode::SlotResume,
        );
        assert!(request.slots.contains_key(RESUME_FILTER_KEY));

        let request =
            build_subscribe_request(Filters::new(HashMap::new()), &config, ReconnectMode::Off);
        assert!(!request.slots.contains_key(RESUME_FILTER_KEY));
    }
}
