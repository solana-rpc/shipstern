use std::{collections::HashMap, fmt::Debug};

use shipstern::{
    builder::{Builder, BuilderKind, RuntimeBuilder, RuntimeKind},
    config::{BufferConfig, ShipsternConfig},
    handler::{BoxPipeline, Pipeline},
    sources::{FromConfig, SourceTrait},
    util,
};
use shipstern_core::{
    instruction::InstructionUpdate, AccountUpdate, BlockMetaUpdate, Parser, ProgramParser, Pubkey,
    TransactionUpdate,
};
use shipstern_proto::{
    prost::{Message, Name},
    prost_types::Any,
};
use tokio::sync::broadcast;

use super::{
    config::{GrpcConfig, StreamConfig},
    grpc::{Channels, GrpcHandler, Receiver},
    Server,
};

/// An error thrown by the Shipstern stream server builder.
#[derive(Debug, thiserror::Error)]
pub enum BuilderError {
    /// Two program parsers were registered with the same program ID.
    #[error("Parser with duplicate ID {1:?} and duplicate program ID {0} registered")]
    DuplicateId(Pubkey, String),
    /// An error occurred while building the underlying Shipstern runtime.
    #[error("Error building Shipstern runtime")]
    Runtime(#[from] shipstern::builder::BuilderError),
}

/// Marker type for the [`StreamBuilder`] type.
#[derive(Debug, Default)]
pub struct StreamKind<'a>(Vec<&'a [u8]>, Channels<HashMap<String, Receiver>>);
/// A builder for the [`Server`] type.
pub struct StreamBuilder<'a, S: SourceTrait>(Builder<StreamKind<'a>, S>);

impl BuilderKind for StreamKind<'_> {
    type Error = BuilderError;
}

impl<S: SourceTrait> Default for StreamBuilder<'_, S> {
    fn default() -> Self { Self(Builder::default()) }
}

impl<'a, S: SourceTrait> StreamBuilder<'a, S> {
    pub fn new(builder: Builder<StreamKind<'a>, S>) -> Self { Self(builder) }
}

fn wrap_parser<P: Debug + Parser + Send + Sync + 'static>(
    parser: P,
    tx: broadcast::Sender<Any>,
) -> BoxPipeline<'static, P::Input>
where
    P::Input: Sync,
    P::Output: Message + Name + Send + Sync,
{
    Box::new(Pipeline::new(parser, [GrpcHandler(tx)]))
}

impl<'a, S: SourceTrait> StreamBuilder<'a, S> {
    fn insert<
        P: Debug + ProgramParser + Send + Sync + 'static,
        F: for<'b> FnOnce(
            &'b mut Builder<StreamKind<'a>, S>,
        ) -> &'b mut Vec<BoxPipeline<'static, P::Input>>,
    >(
        self,
        parser: P,
        f: F,
    ) -> Self
    where
        P::Input: Sync,
        P::Output: Message + Name + Send + Sync,
    {
        let res = self.0.try_mutate(|s| {
            use std::collections::hash_map::Entry;

            // TODO: configure channel size
            let (tx, rx) = broadcast::channel(64);

            match s.extra.1.entry(parser.program_id()) {
                Entry::Vacant(v) => {
                    v.insert([(parser.id().into_owned(), rx)].into_iter().collect());
                },
                Entry::Occupied(o) => match o.into_mut().entry(parser.id().into_owned()) {
                    Entry::Vacant(v) => {
                        v.insert(rx);
                    },
                    Entry::Occupied(_) => {
                        return Err(BuilderError::DuplicateId(
                            parser.program_id(),
                            parser.id().into_owned(),
                        ));
                    },
                },
            }
            f(s).push(wrap_parser(parser, tx));
            Ok(())
        });

        Self(res)
    }

    /// Add a new descriptor set to the builder.
    pub fn descriptor_set(self, desc: &'a [u8]) -> Self {
        Self(self.0.mutate(|s| s.extra.0.push(desc)))
    }

    /// Sets the metrics registry for the runtime.
    #[cfg(feature = "prometheus")]
    pub fn metrics(self, metrics_registry: prometheus::Registry) -> Self {
        Self(self.0.metrics(metrics_registry))
    }

    /// Add a new account parser to the builder.
    pub fn account<A: Debug + ProgramParser<Input = AccountUpdate> + Send + Sync + 'static>(
        self,
        account: A,
    ) -> Self
    where
        A::Input: Sync,
        A::Output: Message + Name + Send + Sync,
    {
        self.insert(account, |s| &mut s.account)
    }

    /// Add a new block metadata parser to the builder.
    pub fn block_meta<A: Debug + ProgramParser<Input = BlockMetaUpdate> + Send + Sync + 'static>(
        self,
        block_meta: A,
    ) -> Self
    where
        A::Input: Sync,
        A::Output: Message + Name + Send + Sync,
    {
        self.insert(block_meta, |s| &mut s.block_meta)
    }

    /// Add a new transaction parser to the builder.
    pub fn transaction<
        T: Debug + ProgramParser<Input = TransactionUpdate> + Send + Sync + 'static,
    >(
        self,
        transaction: T,
    ) -> Self
    where
        T::Input: Sync,
        T::Output: Message + Name + Send + Sync,
    {
        self.insert(transaction, |s| &mut s.transaction)
    }

    /// Add a new instruction parser to the builder.
    pub fn instruction<
        T: Debug + ProgramParser<Input = InstructionUpdate> + Send + Sync + 'static,
    >(
        self,
        instruction: T,
    ) -> Self
    where
        T::Input: Sync,
        T::Output: Message + Name + Send + Sync,
    {
        self.insert(instruction, |s| &mut s.instruction)
    }

    /// Attempt to build a new [`Server`] around `source`, using the given
    /// gRPC and buffer configuration.
    ///
    /// # Errors
    /// This function returns an error if the builder is invalid.
    pub fn try_build_with(
        self,
        source: S,
        grpc_cfg: GrpcConfig,
        buffer: BufferConfig,
    ) -> Result<Server<'a, S>, BuilderError> {
        let Builder {
            err,
            account,
            transaction,
            instruction,
            block_meta,
            block,
            extra: StreamKind(desc_sets, channels),
            slot,
            _source,
            #[cfg(feature = "prometheus")]
            metrics_registry,
            ..
        } = self.0;
        let () = err?;

        let channels = channels
            .into_iter()
            .map(|(k, v)| (k, v.into_values().collect()))
            .collect();

        let runtime_builder = RuntimeBuilder {
            err: Ok(()),
            account,
            transaction,
            instruction,
            block_meta,
            block,
            extra: RuntimeKind,
            slot,
            _source,
            #[cfg(feature = "prometheus")]
            metrics_registry,
            ..Default::default()
        };

        let runtime = runtime_builder.try_build_with(source, buffer)?;

        Ok(Server {
            grpc_cfg,
            desc_sets,
            channels,
            runtime,
        })
    }

    /// Build a new [`Server`] around `source`, terminating the current process
    /// if an error occurs.
    #[inline]
    pub fn build_with(
        self,
        source: S,
        grpc_cfg: GrpcConfig,
        buffer: BufferConfig,
    ) -> Server<'a, S> {
        util::handle_fatal_msg(
            self.try_build_with(source, grpc_cfg, buffer),
            "Error building Shipstern stream server",
        )
    }
}

impl<'a, S: FromConfig> StreamBuilder<'a, S> {
    /// Attempt to build a new [`Server`] instance from the current builder
    /// state and the provided configuration, constructing the source from its
    /// section of the config.
    ///
    /// # Errors
    /// This function returns an error if the builder or configuration are
    /// invalid.
    pub fn try_build(self, config: StreamConfig<S::Config>) -> Result<Server<'a, S>, BuilderError> {
        let StreamConfig {
            grpc,
            runtime: ShipsternConfig { source, buffer },
        } = config;

        self.try_build_with(S::from_config(source), grpc, buffer)
    }

    /// Build a new [`Server`] instance from the current builder state and the
    /// provided configuration, terminating the current process if an error
    /// occurs.
    #[inline]
    pub fn build(self, config: StreamConfig<S::Config>) -> Server<'a, S> {
        util::handle_fatal_msg(
            self.try_build(config),
            "Error building Shipstern stream server",
        )
    }
}
