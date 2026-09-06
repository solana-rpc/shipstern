//! Sources for Shipstern.
//!
//! A `SourceTrait` is a trait that defines the behavior for data sources that can be used to connect to it and
//! send updates to a channel. This trait is implemented by various modules, including the `yellowstone_grpc` module.

use async_trait::async_trait;
use shipstern_core::Filters;
use tokio::sync::{mpsc::Sender, oneshot, watch};
use yellowstone_grpc_proto::{geyser::SubscribeUpdate, tonic};

/// How a source exited.
#[derive(Debug)]
pub enum SourceExitStatus {
    /// Update channel receiver was dropped.
    ReceiverDropped,
    /// Source finished successfully (finite sources like snapshot/RPC).
    Completed,
    /// Server closed connection unexpectedly (streaming sources).
    StreamEnded,
    /// gRPC error.
    StreamError {
        /// gRPC status code.
        code: tonic::Code,
        /// Server error message.
        message: String,
    },
    /// Other errors.
    Error(String),
}

/// Data source that streams updates to the runtime.
///
/// A source is a value the runtime is handed, so it can carry whatever state
/// it needs: a config, a shared client, a fixture. The filter set is computed
/// from the registered pipelines and arrives at [`Self::connect`].
///
/// ```rust, ignore
/// #[derive(Debug)]
/// struct MySource { config: MyConfig }
///
/// #[async_trait]
/// impl SourceTrait for MySource {
///     async fn connect(
///         &self,
///         filters: Filters,
///         tx: Sender<Result<SubscribeUpdate, Status>>,
///         status_tx: oneshot::Sender<SourceExitStatus>,
///     ) -> Result<(), shipstern::Error> {
///         // stream updates into `tx`, then report how the stream ended
///         let _ = status_tx.send(SourceExitStatus::Completed);
///         Ok(())
///     }
/// }
///
/// Runtime::builder()
///     .account(Pipeline::new(AccountParser, [Handler]))
///     .try_build_with(MySource { config }, buffer_config)?;
/// ```
///
/// Implement [`FromConfig`] as well to build the runtime from a
/// [`ShipsternConfig`](crate::config::ShipsternConfig) document with
/// [`RuntimeBuilder::try_build`](crate::builder::RuntimeBuilder::try_build).
#[async_trait]
pub trait SourceTrait: std::fmt::Debug + Send + Sync + 'static {
    /// Connect and stream updates matching `filters`. Send exit status via
    /// `status_tx` before returning.
    async fn connect(
        &self,
        filters: Filters,
        tx: Sender<Result<SubscribeUpdate, tonic::Status>>,
        status_tx: oneshot::Sender<SourceExitStatus>,
    ) -> Result<(), crate::Error>;

    /// Connect and stream updates, applying each filter set published on
    /// `filter_updates_rx` to the live subscription. The slot holds only the
    /// newest set.
    ///
    /// The default ignores the receiver and calls [`Self::connect`]. Override it
    /// together with implementing [`FilterUpdateSource`].
    ///
    async fn connect_with_filter_updates(
        &self,
        filters: Filters,
        tx: Sender<Result<SubscribeUpdate, tonic::Status>>,
        status_tx: oneshot::Sender<SourceExitStatus>,
        filter_updates_rx: watch::Receiver<Filters>,
    ) -> Result<(), crate::Error> {
        drop(filter_updates_rx);

        self.connect(filters, tx, status_tx).await
    }
}

/// A source the runtime can construct from its section of a
/// [`ShipsternConfig`](crate::config::ShipsternConfig).
///
/// Implementing this unlocks
/// [`RuntimeBuilder::try_build`](crate::builder::RuntimeBuilder::try_build),
/// which takes the whole config document. Sources built some other way, test
/// doubles for instance, skip it and are handed to
/// [`RuntimeBuilder::try_build_with`](crate::builder::RuntimeBuilder::try_build_with)
/// directly.
///
/// ```rust, ignore
/// impl FromConfig for YellowstoneGrpcSource {
///     type Config = YellowstoneGrpcConfig;
///
///     fn from_config(config: Self::Config) -> Self { Self { config } }
/// }
/// ```
pub trait FromConfig: SourceTrait {
    /// Source-specific configuration, one section of the config document.
    type Config: serde::de::DeserializeOwned + clap::Args + std::fmt::Debug;

    /// Build the source from its configuration.
    fn from_config(config: Self::Config) -> Self;
}

/// A source that applies filter updates to its live subscription, which unlocks
/// [`Runtime::handle`](crate::Runtime::handle).
///
/// You must also override [`SourceTrait::connect_with_filter_updates`]: its
/// default discards the receiver, so every update returns `Ok(())` and reaches
/// nothing.
///
/// ```rust, ignore
/// impl FilterUpdateSource for YellowstoneGrpcSource {}
/// ```
pub trait FilterUpdateSource: SourceTrait {}
