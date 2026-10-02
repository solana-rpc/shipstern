#![deny(
    clippy::disallowed_methods,
    clippy::suspicious,
    clippy::style,
    clippy::clone_on_ref_ptr,
    missing_debug_implementations,
    missing_copy_implementations
)]
#![warn(clippy::pedantic, missing_docs)]
#![allow(clippy::module_name_repetitions)]

//! Shipstern provides a simple API for requesting, parsing, and consuming data
//! from Yellowstone.

use std::{marker::PhantomData, sync::Arc};

use config::BufferConfig;
use shipstern_core::Filters;
use tokio::sync::{mpsc, oneshot, watch};
use yellowstone_grpc_proto::tonic::Status;

use crate::sources::SourceExitStatus;

#[cfg(feature = "prometheus")]
pub extern crate prometheus;
#[cfg(feature = "prometheus")]
pub mod metrics;
pub extern crate shipstern_core;
pub extern crate thiserror;
pub use shipstern_core::bs58;

mod buffer;
pub mod builder;
pub mod config;
mod handle;
pub mod handler;
pub mod instruction;

pub mod sources;

/// Utility functions for the Shipstern runtime.
pub mod util;

pub mod filter_pipeline;

pub use handle::{FilterUpdateError, RuntimeHandle};
pub use handler::{Handler, HandlerResult, Pipeline};
pub use shipstern_core::CommitmentLevel;
pub use util::*;
use yellowstone_grpc_proto::geyser::SubscribeUpdate;

use crate::{
    builder::RuntimeBuilder,
    sources::{FilterUpdateSource, SourceTrait},
};

/// An error thrown by the Shipstern runtime.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A system I/O error.
    #[error("I/O error")]
    Io(#[from] std::io::Error),
    /// An error returned by a Yellowstone server.
    #[error("Yellowstone client builder error")]
    YellowstoneBuilder(#[from] yellowstone_grpc_client::GeyserGrpcBuilderError),
    /// An error returned by a Yellowstone client.
    #[error("Yellowstone client error")]
    YellowstoneClient(#[from] yellowstone_grpc_client::GeyserGrpcClientError),
    /// An error occurring when the Yellowstone client stops early.
    #[error("Yellowstone client crashed")]
    ClientHangup,
    /// An error occurring when the Yellowstone server closes the connection.
    #[error("Yellowstone stream hung up unexpectedly")]
    ServerHangup,
    /// A gRPC error returned by the Yellowstone server.
    #[error("Yellowstone stream returned an error")]
    YellowstoneStatus(#[from] yellowstone_grpc_proto::tonic::Status),
    /// An error occurring when a datasource is not configured correctly.
    #[error("Yellowstone stream config error")]
    ConfigError,
    /// An error occurring when a runtime error occurs.
    #[error("Other error")]
    Other(#[from] Box<dyn std::error::Error + Send + Sync>),
}

/// The main runtime for Shipstern.
#[derive(Debug)]
pub struct Runtime<S: SourceTrait> {
    buffer: BufferConfig,
    source: S::Config,
    pipelines: handler::PipelineSets,
    filter_updates_rx: watch::Receiver<Filters>,
    filter_state: Arc<handle::FilterState>,
    #[cfg(feature = "prometheus")]
    metrics_registry: prometheus::Registry,
    _source: PhantomData<S>,
}

impl<S: SourceTrait> Runtime<S> {
    /// Create a new runtime builder.
    pub fn builder() -> RuntimeBuilder<S> { RuntimeBuilder::<S>::default() }
}

impl<S: FilterUpdateSource> Runtime<S> {
    /// Create a handle for changing this runtime's subscription once it runs.
    /// Only sources implementing [`FilterUpdateSource`] have one. Take it before
    /// running, since the run methods consume the runtime.
    ///
    /// ```rust, ignore
    /// let runtime = Runtime::<YellowstoneGrpcSource>::builder()
    ///     .account(Pipeline::new(TokenProgramAccParser, [Handler]))
    ///     .try_build(config)?;
    ///
    /// let handle = runtime.handle();
    /// tokio::spawn(runtime.run_async());
    ///
    /// handle.update_filters(|filters| filters.merge(TokenProgramAccParser.id(), extra_owner))?;
    /// ```
    ///
    #[must_use]
    pub fn handle(&self) -> RuntimeHandle { RuntimeHandle::new(Arc::clone(&self.filter_state)) }
}

impl<S: SourceTrait> Runtime<S> {
    /// Create a Tokio runtime and run the Shipstern runtime in it, exiting the
    /// process on error. See [`Self::try_run`] to handle errors and
    /// [`Self::run_async`] to bring your own Tokio runtime.
    ///
    /// # Example
    ///
    /// ```ignore
    /// use shipstern::Pipeline;
    /// use shipstern_spl_token_parser::{AccountParser, InstructionParser};
    ///
    /// // MyHandler is a handler that implements the Handler trait
    /// // NOTE: The main function is not async
    /// fn main() {
    ///     Runtime::builder::<YellowstoneGrpcSource>()
    ///         .account(Pipeline::new(AccountParser, [MyHandler]))
    ///         .instruction(Pipeline::new(InstructionParser, [MyHandler]))
    ///         .build(config)
    ///         .run(); // Process will exit if an error occurs
    /// }
    /// ```
    #[inline]
    pub fn run(self) { util::handle_fatal(self.try_run()); }

    /// Error returning variant of [`Self::run`].
    ///
    /// # Errors
    /// This function returns an error if the runtime crashes.
    #[inline]
    pub fn try_run(self) -> Result<(), Box<Error>> {
        tokio::runtime::Runtime::new()
            .map_err(|e| Box::new(e.into()))?
            .block_on(self.try_run_async())
    }

    /// Run the Shipstern runtime on the current Tokio runtime, exiting the process
    /// on error. See [`Self::try_run_async`] to handle errors and [`Self::run`] to
    /// have a Tokio runtime created for you.
    ///
    /// # Example
    ///
    /// ```ignore
    /// use shipstern_parser::{
    ///     token_extension_program::{
    ///         AccountParser as TokenExtensionProgramAccParser,
    ///         InstructionParser as TokenExtensionProgramIxParser,
    ///     },
    ///     token_program::{
    ///         AccountParser as TokenProgramAccParser, InstructionParser as TokenProgramIxParser,
    ///     },
    /// };
    ///
    /// // MyHandler is a handler that implements the Handler trait
    ///
    /// #[tokio::main]
    /// async fn main() {
    ///     Runtime::builder::<YellowstoneGrpcSource>()
    ///         .account(Pipeline::new(TokenProgramAccParser, [MyHandler]))
    ///         .account(Pipeline::new(TokenExtensionProgramAccParser, [MyHandler]))
    ///         .instruction(Pipeline::new(TokenExtensionProgramIxParser, [MyHandler]))
    ///         .instruction(Pipeline::new(TokenProgramIxParser, [MyHandler]))
    ///         .build(config)
    ///         .run_async()
    ///         .await;
    /// }
    /// ```
    #[inline]
    pub async fn run_async(self) { util::handle_fatal(self.try_run_async().await); }

    /// Error returning variant of [`Self::run_async`].
    ///
    /// # Errors
    /// This function returns an error if the runtime crashes.
    ///
    /// # Panics
    /// Only panics if the rustls crypto provider fails to install.
    ///
    /// # Shutdown Flows
    ///
    /// ```text
    /// ┌─────────────────────────────────────────────────────────────────────┐
    /// │                         RUNTIME SELECT!                             │
    /// │                                                                     │
    /// │   Signal ─────────────────┐                                         │
    /// │   (Ctrl+C, SIGTERM)       │                                         │
    /// │                           ▼                                         │
    /// │                    ┌─────────────┐     ┌─────────────┐              │
    /// │                    │Signal wins  │────▶│stop_buffer()│              │
    /// │                    │select!      │     │drops rx     │              │
    /// │                    └─────────────┘     └──────┬──────┘              │
    /// │                           │                   │                     │
    /// │                           ▼                   ▼                     │
    /// │                      Ok(()) exit      Source sees send              │
    /// │                                       fail, but select!             │
    /// │                                       already done                  │
    /// │                                                                     │
    /// ├─────────────────────────────────────────────────────────────────────┤
    /// │                                                                     │
    /// │   Buffer ─────────────────┐                                         │
    /// │   (rx recv error/close)   │                                         │
    /// │                           ▼                                         │
    /// │                    ┌─────────────┐                                  │
    /// │                    │Buffer wins  │────▶ Err(YellowstoneStatus)      │
    /// │                    │select!      │      or Ok(StopCode)             │
    /// │                    └─────────────┘                                  │
    /// │                                                                     │
    /// ├─────────────────────────────────────────────────────────────────────┤
    /// │                                                                     │
    /// │   SourceExit ─────────────┐                                         │
    /// │   (source task ended)     │                                         │
    /// │                           ▼                                         │
    /// │                    ┌─────────────┐                                  │
    /// │                    │SourceExit   │                                  │
    /// │                    │wins select! │                                  │
    /// │                    └──────┬──────┘                                  │
    /// │                           │                                         │
    /// │        ┌──────────────┬───┴──------───┬──────────────┐              │
    /// │        ▼              ▼               ▼              ▼              │
    /// │   Completed      StreamEnded      StreamError     Error             │
    /// │   (finite src)  (unexpected)        (gRPC)        (other)           │
    /// │        │              │               │              │              │
    /// │        ▼              ▼               ▼              ▼              │
    /// │   drain -> Ok    ServerHangup     ServerHangup     Other            │
    /// │                                                                     │
    /// │    ┌──────────────────────────────────────────────────────────┐     │
    /// │    │ ReceiverDropped: defensive only - normally unreachable   │     │
    /// │    │ because Signal/Buffer branch wins first when rx drops    │     │
    /// │    └──────────────────────────────────────────────────────────┘     │
    /// └─────────────────────────────────────────────────────────────────────┘
    /// ```
    #[tracing::instrument("Runtime::run", skip(self))]
    #[allow(clippy::too_many_lines)]
    pub async fn try_run_async(self) -> Result<(), Box<Error>> {
        enum StopType<S> {
            Signal(S),
            Buffer(Result<(), Error>),
            SourceExit(Result<SourceExitStatus, oneshot::error::RecvError>),
        }

        let (tx, updates_rx) =
            mpsc::channel::<Result<SubscribeUpdate, Status>>(self.buffer.sources_channel_size);

        let (status_tx, status_rx) = oneshot::channel::<SourceExitStatus>();

        #[cfg(feature = "prometheus")]
        metrics::register_metrics(&self.metrics_registry);

        let mut filter_updates_rx = self.filter_updates_rx;

        // Subscribe with the latest set, so an update sent before running is in the
        // first request. Marking it seen stops the source resending it.
        let filters = filter_updates_rx.borrow_and_update().clone();

        let source = S::new(self.source, filters);

        // Release the runtime's own reference so the slot closes once every
        // handle is gone, and a source that waits on updates is not left
        // waiting on a sender that can never produce one.
        drop(self.filter_state);

        tokio::spawn(async move {
            let _ = source
                .connect_with_filter_updates(tx, status_tx, filter_updates_rx)
                .await;
        });

        let signal;

        #[cfg(unix)]
        {
            use futures_util::stream::{FuturesUnordered, StreamExt};
            use tokio::signal::unix::SignalKind;

            let mut stream = [
                SignalKind::hangup(),
                SignalKind::interrupt(),
                SignalKind::quit(),
                SignalKind::terminate(),
            ]
            .into_iter()
            .map(|k| {
                tokio::signal::unix::signal(k).map(|mut s| async move {
                    s.recv().await;
                    Ok(k)
                })
            })
            .collect::<Result<FuturesUnordered<_>, _>>()
            .map_err(|e| Box::new(e.into()))?;

            signal = async move { stream.next().await.transpose() }
        }

        #[cfg(not(unix))]
        {
            use std::fmt;

            use futures_util::TryFutureExt;

            struct CtrlC;

            impl fmt::Debug for CtrlC {
                fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result { f.write_str("^C") }
            }

            signal = tokio::signal::ctrl_c()
                .map_ok(|()| Some(CtrlC))
                .map_err(Into::into);
        }

        let mut buffer = buffer::Buffer::run_yellowstone(self.buffer, updates_rx, self.pipelines);

        let stop_ty = tokio::select! {
            s = signal => StopType::Signal(s),
            b = buffer.wait_for_stop() => StopType::Buffer(b),
            status = status_rx => StopType::SourceExit(status),
        };

        match stop_ty {
            StopType::Signal(Ok(Some(s))) => {
                tracing::warn!("{s:?} received, shutting down...");
                Self::force_stop_buffer(buffer).await;
                Ok(())
            },
            StopType::Signal(Ok(None)) => Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "Signal handler returned None",
            )
            .into()),
            StopType::Buffer(result) => result,
            StopType::Signal(Err(e)) => Err(e),
            StopType::SourceExit(Ok(status)) => match status {
                SourceExitStatus::ReceiverDropped => {
                    tracing::info!("Source stopped: receiver dropped (shutdown)");
                    Self::force_stop_buffer(buffer).await;
                    Ok(())
                },
                SourceExitStatus::Completed => {
                    tracing::info!("Source completed successfully; draining runtime buffer");
                    buffer.wait_for_stop().await
                },
                SourceExitStatus::StreamEnded => {
                    tracing::warn!("Source stopped: stream ended unexpectedly");
                    Err(Error::ServerHangup)
                },
                SourceExitStatus::StreamError { code, message } => {
                    tracing::error!(?code, %message, "Source stopped: stream error");
                    Err(Error::YellowstoneStatus(Status::new(code, message)))
                },
                SourceExitStatus::Error(msg) => {
                    tracing::error!(%msg, "Source stopped: error");
                    Err(Error::Other(msg.into()))
                },
            },
            StopType::SourceExit(Err(_)) => {
                tracing::warn!("Source exit status channel closed unexpectedly");
                Err(Error::ClientHangup)
            },
        }?;

        Ok(())
    }

    async fn force_stop_buffer(buffer: buffer::Buffer) {
        match buffer.join().await {
            Err(e) => tracing::warn!(err = %Chain(&e), "Error stopping runtime buffer"),
            Ok(c) => c.as_unit(),
        }
    }
}

#[cfg(test)]
mod load_tests;
#[cfg(test)]
mod runtime_tests;
