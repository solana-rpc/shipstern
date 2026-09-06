//! Builder types for the Shipstern runtime and stream server.
use std::sync::Arc;

use shipstern_core::{
    instruction::InstructionUpdate, AccountUpdate, BlockMetaUpdate, BlockUpdate, SlotUpdate,
    TransactionUpdate,
};
use tokio::sync::watch;

use crate::{
    config::{BufferConfig, ShipsternConfig},
    handle::FilterState,
    handler::{BoxPipeline, DynPipeline, PipelineSet, PipelineSets},
    instruction::InstructionPipeline,
    sources::{FromConfig, SourceTrait},
    util, Runtime,
};

/// Helper trait for defining the intended use for a builder.
pub trait BuilderKind: Default {
    /// The type of error returned by the builder.
    type Error: std::error::Error;
}

/// An error thrown by the Shipstern runtime builder.
#[derive(Debug, thiserror::Error)]
pub enum BuilderError {
    /// Two account pipelines were registered with the same parser ID.
    #[error("ID collision detected among account pipelines")]
    AccountPipelineCollision,
    /// Two transaction pipelines were registered with the same parser ID.
    #[error("ID collision detected among transaction pipelines")]
    TransactionPipelineCollision,
    /// Two slot pipelines were registered with the same parser ID.
    #[error("ID collision detected among slot pipelines")]
    SlotPipelineCollision,
    /// Two block meta pipelines were registered with the same parser ID.
    #[error("ID collision detected among block meta pipelines")]
    BlockMetaCollision,
    /// Two block pipelines were registered with the same parser ID.
    #[error("ID collision detected among block pipelines")]
    BlockCollision,
    /// A required field was missing from the builder.
    #[error("Missing field {0:?}")]
    MissingField(&'static str),
    /// A required field or section was missing from the provided configuration.
    #[error("Missing config section {0:?}")]
    MissingConfig(&'static str),
    /// An error occurred while instantiating the metrics backend.
    #[error("Error instantiating metrics backend")]
    Metrics(#[source] Box<dyn std::error::Error>),
}

/// A builder used by both the [`Runtime`] and
/// [`stream::Server`](crate::stream::Server) types.
#[derive(Debug)]
#[must_use = "Consider calling .build() on this builder"]
pub struct Builder<K: BuilderKind, S: SourceTrait> {
    /// The error result of the builder.    
    pub err: Result<(), K::Error>,
    /// The account pipelines.
    pub account: Vec<BoxPipeline<'static, AccountUpdate>>,
    /// The transaction pipelines.
    pub transaction: Vec<BoxPipeline<'static, TransactionUpdate>>,
    /// The instruction pipelines.
    pub instruction: Vec<BoxPipeline<'static, InstructionUpdate>>,
    /// The block meta pipelines.
    pub block_meta: Vec<BoxPipeline<'static, BlockMetaUpdate>>,
    /// The block meta pipelines.
    pub block: Vec<BoxPipeline<'static, BlockUpdate>>,
    /// The slot pipelines.
    pub slot: Vec<BoxPipeline<'static, SlotUpdate>>,
    /// The metrics.
    #[cfg(feature = "prometheus")]
    pub metrics_registry: prometheus::Registry,
    /// The extra builder kind.
    pub extra: K,
    /// The source trait.
    pub _source: std::marker::PhantomData<S>,
}

impl<K: BuilderKind, S: SourceTrait> Default for Builder<K, S> {
    fn default() -> Self {
        Self {
            err: Ok(()),
            account: vec![],
            transaction: vec![],
            instruction: vec![],
            block_meta: vec![],
            block: vec![],
            slot: vec![],
            extra: K::default(),
            _source: std::marker::PhantomData,
            #[cfg(feature = "prometheus")]
            metrics_registry: prometheus::Registry::new(),
        }
    }
}

impl<K: BuilderKind, S: SourceTrait> Builder<K, S> {
    /// Mutate the builder in place.
    #[inline]
    pub fn mutate(self, mutate: impl FnOnce(&mut Self)) -> Self {
        self.try_mutate(|s| {
            mutate(s);
            Ok(())
        })
    }

    /// Try to mutate the builder in place.
    #[inline]
    pub fn try_mutate(mut self, mutate: impl FnOnce(&mut Self) -> Result<(), K::Error>) -> Self {
        if let Ok(()) = self.err {
            self.err = mutate(&mut self);
        }
        self
    }

    #[cfg(feature = "prometheus")]
    /// Sets the metrics registry for the runtime.
    pub fn metrics(self, metrics_registry: prometheus::Registry) -> Builder<K, S> {
        self.mutate(|s| s.metrics_registry = metrics_registry)
    }
}

/// Marker type used for the [`RuntimeBuilder`] type.
#[derive(Debug, Default, Clone, Copy)]
pub struct RuntimeKind;
/// A builder for the [`Runtime`] type.
pub type RuntimeBuilder<S> = Builder<RuntimeKind, S>;

impl BuilderKind for RuntimeKind {
    type Error = BuilderError;
}

impl<S: SourceTrait> RuntimeBuilder<S> {
    /// Add a new account pipeline to the builder.
    pub fn account<A: DynPipeline<AccountUpdate> + Send + Sync + 'static>(
        self,
        account: A,
    ) -> Self {
        self.mutate(|s| s.account.push(Box::new(account)))
    }

    /// Add a new transaction pipeline to the builder.
    pub fn transaction<T: DynPipeline<TransactionUpdate> + Send + Sync + 'static>(
        self,
        transaction: T,
    ) -> Self {
        self.mutate(|s| s.transaction.push(Box::new(transaction)))
    }

    /// Add a new instruction pipeline to the builder.
    ///
    /// **NOTE:** All registered instruction pipelines will be bundled into a
    /// single [`InstructionPipeline`] instance.
    pub fn instruction<I: DynPipeline<InstructionUpdate> + Send + Sync + 'static>(
        self,
        instruction: I,
    ) -> Self {
        self.mutate(|s| s.instruction.push(Box::new(instruction)))
    }

    /// Add a new block meta pipeline to the builder.
    pub fn block_meta<T: DynPipeline<BlockMetaUpdate> + Send + Sync + 'static>(
        self,
        block_meta: T,
    ) -> Self {
        self.mutate(|s| s.block_meta.push(Box::new(block_meta)))
    }

    /// Add a new block pipeline to the builder.
    pub fn block<T: DynPipeline<BlockUpdate> + Send + Sync + 'static>(self, block: T) -> Self {
        self.mutate(|s| s.block.push(Box::new(block)))
    }

    /// Add a new slot pipeline to the builder.
    pub fn slot<T: DynPipeline<SlotUpdate> + Send + Sync + 'static>(self, slot: T) -> Self {
        self.mutate(|s| s.slot.push(Box::new(slot)))
    }

    /// Attempt to build a new [`Runtime`] around `source`, using the given
    /// buffer configuration.
    ///
    /// This is the entry point for a source built by the caller, which is any
    /// source not implementing [`FromConfig`], or one carrying state that a
    /// config section cannot express.
    ///
    /// ```rust, ignore
    /// let ShipsternConfig { source, buffer } = toml::from_str(&text)?;
    ///
    /// Runtime::builder()
    ///     .account(Pipeline::new(AccountParser, [Logger]))
    ///     .try_build_with(YellowstoneGrpcSource::from_config(source), buffer)?;
    /// ```
    ///
    /// # Errors
    /// This function returns an error if the builder is invalid.
    /// # Panics
    /// Only panics if the prometheus metrics registry is not set.
    pub fn try_build_with(
        self,
        source: S,
        buffer: BufferConfig,
    ) -> Result<Runtime<S>, BuilderError> {
        let Self {
            err,
            account,
            transaction,
            instruction,
            block_meta,
            block,
            slot,
            extra: RuntimeKind,
            _source,
            #[cfg(feature = "prometheus")]
            metrics_registry,
        } = self;
        let () = err?;

        // Bundle every instruction parser into a single InstructionPipeline so
        // the instruction tree is built once per transaction (one
        // `build_from_txn`, which clones the transaction and rebuilds the CPI
        // tree) and then fanned out to all parsers, instead of each parser
        // rebuilding the whole tree independently (N clones + N rebuilds).
        //
        // Bundling collapses per-parser ids into one entry, so the collision
        // checks below no longer see instruction ids. Warn on duplicates here to
        // preserve the previous behavior; unlike the other pipeline kinds a
        // duplicate instruction id is not fatal -- both parsers run.
        {
            let mut seen = std::collections::HashSet::new();
            for id in instruction.iter().map(|ix| ix.id()) {
                if !seen.insert(id.clone()) {
                    tracing::warn!(parser = %id, "Duplicate instruction parser ID; both will run");
                }
            }
        }

        let mut ixs = PipelineSet::new();

        if let Some(pipeline) = InstructionPipeline::new(instruction) {
            let pipeline = Box::new(pipeline) as BoxPipeline<'static, TransactionUpdate>;
            ixs.insert(pipeline.id().into_owned(), pipeline);
        }

        let account_len = account.len();
        let transaction_len = transaction.len();
        let block_meta_len = block_meta.len();
        let block_len = block.len();
        let slot_len = slot.len();

        let pipelines = PipelineSets {
            account: account.into_iter().collect(),
            transaction: transaction.into_iter().collect(),
            instruction: ixs,
            block_meta: block_meta.into_iter().collect(),
            block: block.into_iter().collect(),
            slot: slot.into_iter().collect(),
        };

        if pipelines.account.len() != account_len {
            return Err(BuilderError::AccountPipelineCollision);
        }

        if pipelines.transaction.len() != transaction_len {
            return Err(BuilderError::TransactionPipelineCollision);
        }

        if pipelines.block_meta.len() != block_meta_len {
            return Err(BuilderError::BlockMetaCollision);
        }

        if pipelines.block.len() != block_len {
            return Err(BuilderError::BlockCollision);
        }

        if pipelines.slot.len() != slot_len {
            return Err(BuilderError::SlotPipelineCollision);
        }

        let (filter_updates_tx, filter_updates_rx) = watch::channel(pipelines.filters());
        let filter_state = Arc::new(FilterState::new(filter_updates_tx));

        Ok(Runtime {
            buffer,
            source,
            pipelines,
            filter_updates_rx,
            filter_state,
            #[cfg(feature = "prometheus")]
            metrics_registry,
        })
    }

    /// Build a new [`Runtime`] around `source`, terminating the current
    /// process if an error occurs.
    #[inline]
    #[must_use]
    pub fn build_with(self, source: S, buffer: BufferConfig) -> Runtime<S> {
        util::handle_fatal_msg(
            self.try_build_with(source, buffer),
            "Error building Shipstern runtime",
        )
    }
}

impl<S: FromConfig> RuntimeBuilder<S> {
    /// Attempt to build a new [`Runtime`] instance from the current builder
    /// state and the provided configuration, constructing the source from its
    /// section of the config.
    ///
    /// ```rust, ignore
    /// let config: ShipsternConfig<YellowstoneGrpcConfig> = toml::from_str(&text)?;
    ///
    /// Runtime::<YellowstoneGrpcSource>::builder()
    ///     .account(Pipeline::new(AccountParser, [Logger]))
    ///     .try_build(config)?;
    /// ```
    ///
    /// # Errors
    /// This function returns an error if the builder or configuration are
    /// invalid.
    pub fn try_build(self, config: ShipsternConfig<S::Config>) -> Result<Runtime<S>, BuilderError> {
        let ShipsternConfig { source, buffer } = config;

        self.try_build_with(S::from_config(source), buffer)
    }

    /// Build a new [`Runtime`] instance from the current builder state and the
    /// provided configuration, terminating the current process if an error
    /// occurs.
    #[inline]
    #[must_use]
    pub fn build(self, config: ShipsternConfig<S::Config>) -> Runtime<S> {
        util::handle_fatal_msg(self.try_build(config), "Error building Shipstern runtime")
    }
}
