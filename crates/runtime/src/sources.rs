//! Sources for Shipstern.
//!
//! A source streams [`SubscribeUpdate`]s into the runtime. Implement
//! [`SourceTrait`] for a custom one, [`FromConfig`] to build it from a config
//! document, and [`FilterUpdateSource`] if it can change a live subscription.

use async_trait::async_trait;
use shipstern_core::Filters;
use tokio::sync::{mpsc::Sender, watch};
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
    /// The server ended a running stream with a gRPC status.
    StreamError {
        /// gRPC status code.
        code: tonic::Code,
        /// Server error message.
        message: String,
    },
    /// A running stream stopped on a failure that is only a message.
    ///
    /// Use it from a stream loop that `break`s with how it ended. Anything
    /// else, a failure to connect in particular, returns `Err` from
    /// [`SourceTrait::connect`], which keeps the error's type. The runtime
    /// stops with an error either way.
    Error(String),
}

/// Everything the runtime hands a source when it connects.
///
/// New fields can be added without breaking sources, so destructure it with a
/// trailing `..`:
///
/// ```rust, ignore
/// let SourceContext { filters, tx, .. } = ctx;
/// ```
#[derive(Debug)]
#[non_exhaustive]
pub struct SourceContext {
    /// The filter set to subscribe with, derived from the registered pipelines
    /// and any update published before the runtime started.
    pub filters: Filters,
    /// Where to send updates. A failed send means the runtime has stopped, so
    /// return [`SourceExitStatus::ReceiverDropped`].
    pub tx: Sender<Result<SubscribeUpdate, tonic::Status>>,
    /// Each filter set published through a
    /// [`RuntimeHandle`](crate::RuntimeHandle). The slot holds only the newest
    /// set and closes once the last handle is dropped. Only a
    /// [`FilterUpdateSource`] has handles, so other sources can ignore it.
    pub filter_updates: watch::Receiver<Filters>,
}

impl SourceContext {
    /// A context with no filter updates, for driving a source directly, in a
    /// test for instance. The update slot starts closed.
    #[must_use]
    pub fn new(filters: Filters, tx: Sender<Result<SubscribeUpdate, tonic::Status>>) -> Self {
        let (_, filter_updates) = watch::channel(filters.clone());

        Self {
            filters,
            tx,
            filter_updates,
        }
    }
}

/// Data source that streams updates to the runtime.
///
/// A source is a value the runtime is handed, so it can carry whatever state
/// it needs: a config, a shared client, a fixture. Everything the runtime
/// provides arrives in the [`SourceContext`] passed to [`Self::connect`].
///
/// ```rust, ignore
/// #[derive(Debug)]
/// struct MySource { config: MyConfig }
///
/// #[async_trait]
/// impl SourceTrait for MySource {
///     async fn connect(&self, ctx: SourceContext) -> Result<SourceExitStatus, shipstern::Error> {
///         let SourceContext { filters, tx, .. } = ctx;
///
///         // stream updates matching `filters` into `tx`, then say how it ended
///         Ok(SourceExitStatus::Completed)
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
///
/// The trait is object safe, so `Box<dyn SourceTrait>` is a source too, for
/// picking one at startup.
#[async_trait]
pub trait SourceTrait: std::fmt::Debug + Send + Sync + 'static {
    /// Connect and stream updates until the stream ends, then return how it
    /// ended. A failure returns `Err`, which stops the runtime with that
    /// error; see [`SourceExitStatus::Error`] for the one exception.
    ///
    /// # Errors
    /// Returns an error if the source cannot connect or fails mid-stream.
    async fn connect(&self, ctx: SourceContext) -> Result<SourceExitStatus, crate::Error>;
}

#[async_trait]
impl<S: SourceTrait + ?Sized> SourceTrait for Box<S> {
    async fn connect(&self, ctx: SourceContext) -> Result<SourceExitStatus, crate::Error> {
        (**self).connect(ctx).await
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
/// Implementing it promises that [`SourceTrait::connect`] reads
/// [`SourceContext::filter_updates`] and sends each set to the server. Nothing
/// else ties the two together, so a source that implements this and ignores
/// the slot accepts every update and applies none.
///
/// ```rust, ignore
/// impl FilterUpdateSource for YellowstoneGrpcSource {}
/// ```
pub trait FilterUpdateSource: SourceTrait {}

impl<S: FilterUpdateSource + ?Sized> FilterUpdateSource for Box<S> {}
