//! Helper types for bundling [Shipstern parsers](crate::shipstern_core::Parser) and
//! handler callbacks.

use std::{borrow::Cow, collections::HashMap, pin::Pin};

use futures_util::{Future, FutureExt, StreamExt};
use shipstern_core::{
    AccountUpdate, BlockFooterUpdate, BlockMetaUpdate, BlockUpdate, Filters, GetPrefilter,
    ParseError, Parser, ParserId, Prefilter, SlotUpdate, TransactionUpdate,
};
use smallvec::SmallVec;
use tracing::{trace, Instrument, Span};

#[cfg(feature = "prometheus")]
use crate::metrics;

type BoxedError = Box<dyn std::error::Error + Send + Sync + 'static>;
/// The result returned by a handler.
pub type HandlerResult<T> = Result<T, BoxedError>;

/// A handler callback for a parsed value and its corresponding raw event.
pub trait Handler<T, R>
where R: Sync
{
    /// Consume the parsed value together with the raw event.
    fn handle(&self, value: &T, raw_event: &R) -> impl Future<Output = HandlerResult<()>> + Send;
}

impl<T: Handler<U, R>, U, R> Handler<U, R> for &T
where R: Sync
{
    #[inline]
    fn handle(&self, value: &U, raw_event: &R) -> impl Future<Output = HandlerResult<()>> + Send {
        <T as Handler<U, R>>::handle(self, value, raw_event)
    }
}

pub(crate) use pipeline_error::Errors as PipelineErrors;

mod pipeline_error {
    use smallvec::SmallVec;

    use super::BoxedError;

    #[derive(Debug, Clone, Copy)]
    #[must_use]
    pub struct Handled(());

    impl Handled {
        #[inline]
        pub fn as_unit(self) { let Self(()) = self; }
    }

    #[derive(Debug)]
    pub enum Errors {
        Parse(BoxedError),
        Handlers(SmallVec<[BoxedError; 1]>),
        AlreadyHandled(Handled),
    }

    impl Errors {
        #[inline]
        #[must_use]
        pub fn parse<E: std::error::Error + Send + Sync + 'static>(e: E) -> Self {
            Self::Parse(Box::new(e))
        }

        pub fn handle<T>(self, handler: &str) -> Handled {
            for e in self {
                tracing::error!(
                    err = %crate::Chain(&e),
                    handler,
                    r#type = std::any::type_name::<T>(),
                    "Handler failed",
                );
            }

            Handled(())
        }
    }

    impl IntoIterator for Errors {
        type IntoIter = IntoIter;
        type Item = Error;

        fn into_iter(self) -> Self::IntoIter {
            match self {
                Errors::Parse(e) => IntoIter::Parse([e].into_iter()),
                Errors::Handlers(v) => IntoIter::Handlers(v.into_iter()),
                Errors::AlreadyHandled(Handled(())) => IntoIter::AlreadyHandled,
            }
        }
    }

    #[derive(Debug, thiserror::Error)]
    pub enum Error {
        #[error("Error parsing input value: ({0})")]
        Parser(#[source] BoxedError),
        #[error("Handler returned an error on parsed value: ({0})")]
        Handler(#[source] BoxedError),
    }

    #[derive(Debug)]
    pub enum IntoIter {
        Parse(std::array::IntoIter<BoxedError, 1>),
        Handlers(smallvec::IntoIter<[BoxedError; 1]>),
        AlreadyHandled,
    }

    impl Iterator for IntoIter {
        type Item = Error;

        fn next(&mut self) -> Option<Self::Item> {
            match self {
                Self::Parse(o) => o.next().map(Error::Parser),
                Self::Handlers(v) => v.next().map(Error::Handler),
                Self::AlreadyHandled => None,
            }
        }
    }
}

/// A parser and a set of handlers its output is passed to.
#[derive(Debug)]
pub struct Pipeline<P, H>(P, H);

impl<P, H> Pipeline<P, H> {
    /// Create a new pipeline from a parser and a list of handlers.
    #[inline]
    #[must_use]
    pub fn new(parser: P, handlers: H) -> Self { Self(parser, handlers) }
}

impl<P: ParserId, H> ParserId for Pipeline<P, H> {
    #[inline]
    fn id(&self) -> Cow<'static, str> { self.0.id() }
}

impl<P: GetPrefilter, H> GetPrefilter for Pipeline<P, H> {
    #[inline]
    fn prefilter(&self) -> Prefilter { self.0.prefilter() }
}

/// A boxed pipeline.
pub type BoxPipeline<'h, T> = Box<dyn DynPipeline<T> + Send + Sync + 'h>;

impl<P, I> Pipeline<P, I>
where
    for<'i> &'i I: IntoIterator,
    P: Parser,
    P::Input: Sync,
    for<'i> <&'i I as IntoIterator>::Item: Handler<P::Output, P::Input>,
{
    /// Handle fn for `Pipeline`
    ///
    /// # Errors
    /// If any of the related handlers executions errors, returns those errors
    pub async fn handle(&self, value: &P::Input) -> Result<(), PipelineErrors> {
        let parsed = match self
            .0
            .parse(value)
            .instrument(tracing::debug_span!("shipstern.parse",))
            .await
        {
            Ok(p) => p,
            Err(ParseError::Filtered) => return Ok(()),
            Err(ParseError::DiscriminatorNotFound(msg)) => {
                return Err(PipelineErrors::Parse(msg.into()));
            },
            Err(ParseError::Other(e)) => return Err(PipelineErrors::Parse(e)),
        };
        let parsed = &parsed;

        let errs = (&self.1)
            .into_iter()
            .map(|h| async move {
                h.handle(parsed, value)
                    .instrument(tracing::debug_span!("shipstern.handle",))
                    .await
            })
            .collect::<futures_util::stream::FuturesUnordered<_>>()
            .filter_map(|r| async move { r.err() })
            .collect::<SmallVec<[_; 1]>>()
            .await;

        if errs.is_empty() {
            Ok(())
        } else {
            Err(PipelineErrors::Handlers(errs))
        }
    }
}

/// Object-safe trait for parsing and handling values.
pub trait DynPipeline<T>: std::fmt::Debug + ParserId + GetPrefilter {
    /// Pass the provided value to the parser and handlers comprising this
    /// pipeline.
    fn handle<'h>(
        &'h self,
        value: &'h T,
    ) -> Pin<Box<dyn Future<Output = Result<(), PipelineErrors>> + Send + 'h>>;
}

impl<T> DynPipeline<T> for std::convert::Infallible {
    fn handle<'h>(
        &'h self,
        _: &'h T,
    ) -> Pin<Box<dyn Future<Output = Result<(), PipelineErrors>> + Send + 'h>> {
        match *self {}
    }
}

impl<P: std::fmt::Debug + Parser + Sync, I: std::fmt::Debug + Sync> DynPipeline<P::Input>
    for Pipeline<P, I>
where
    for<'i> &'i I: IntoIterator,
    P::Input: Sync,
    P::Output: Send + Sync,
    for<'i> <&'i I as IntoIterator>::Item: Handler<P::Output, P::Input> + Send,
{
    fn handle<'h>(
        &'h self,
        value: &'h P::Input,
    ) -> Pin<Box<dyn Future<Output = Result<(), PipelineErrors>> + Send + 'h>> {
        Box::pin(Pipeline::handle(self, value))
    }
}

impl<T> ParserId for BoxPipeline<'_, T> {
    fn id(&self) -> Cow<'static, str> { <dyn DynPipeline<T>>::id(&**self) }
}

impl<T> GetPrefilter for BoxPipeline<'_, T> {
    #[inline]
    fn prefilter(&self) -> Prefilter { <dyn DynPipeline<T>>::prefilter(&**self) }
}

impl<T> DynPipeline<T> for BoxPipeline<'_, T> {
    #[inline]
    fn handle<'h>(
        &'h self,
        value: &'h T,
    ) -> Pin<Box<dyn Future<Output = Result<(), PipelineErrors>> + Send + 'h>> {
        <dyn DynPipeline<T>>::handle(&**self, value)
    }
}

#[derive(Debug)]
pub(crate) struct PipelineSets {
    pub account: PipelineSet<BoxPipeline<'static, AccountUpdate>>,
    pub transaction: PipelineSet<BoxPipeline<'static, TransactionUpdate>>,
    pub instruction: PipelineSet<BoxPipeline<'static, TransactionUpdate>>,
    pub block_meta: PipelineSet<BoxPipeline<'static, BlockMetaUpdate>>,
    pub block_footer: PipelineSet<BoxPipeline<'static, BlockFooterUpdate>>,
    pub block: PipelineSet<BoxPipeline<'static, BlockUpdate>>,
    pub slot: PipelineSet<BoxPipeline<'static, SlotUpdate>>,
}

impl PipelineSets {
    #[must_use]
    pub fn filters(&self) -> Filters {
        Filters::new(
            self.account
                .filters()
                .chain(self.transaction.filters())
                .chain(self.instruction.filters())
                .chain(self.block_meta.filters())
                .chain(self.block_footer.filters())
                .chain(self.block.filters())
                .chain(self.slot.filters())
                .collect(),
        )
    }
}

#[derive(Debug)]
pub(crate) struct PipelineSet<P>(HashMap<String, P>);

impl<P> PipelineSet<P> {
    #[inline]
    #[must_use]
    pub fn len(&self) -> usize { self.0.len() }

    #[inline]
    #[must_use]
    pub fn new() -> Self { Self(HashMap::new()) }

    #[inline]
    pub fn insert(&mut self, key: String, value: P) -> Option<P> { self.0.insert(key, value) }
}

impl<P: GetPrefilter> PipelineSet<P> {
    #[inline]
    fn filters(&self) -> impl Iterator<Item = (String, Prefilter)> {
        // # Each filter key is going to be the parser::id()
        self.0
            .iter()
            .map(|(k, v)| (k.clone(), v.prefilter()))
            .collect::<Vec<_>>()
            .into_iter()
    }
}

impl<P> PipelineSet<P> {
    pub(crate) fn get_handlers<I>(&'_ self, it: I) -> Pipelines<'_, P, I> { Pipelines(self, it) }
}

impl<P: ParserId> FromIterator<P> for PipelineSet<P> {
    fn from_iter<I: IntoIterator<Item = P>>(iter: I) -> Self {
        Self(iter.into_iter().map(|i| (i.id().into_owned(), i)).collect())
    }
}

#[derive(Debug)]
pub(crate) struct Pipelines<'m, H, I>(&'m PipelineSet<H>, I);

impl<'m, H, I: IntoIterator> Pipelines<'m, H, I>
where I::Item: AsRef<str> + Send + 'm
{
    fn get_pipelines(self) -> impl Iterator<Item = (I::Item, &'m H)> {
        let Self(pipelines, it) = self;
        it.into_iter().filter_map(|f| {
            let filter = f.as_ref();
            let pipeline = pipelines.0.get(filter);

            if pipeline.is_none() {
                trace!(filter, "No pipeline matched filter on incoming update");
            }

            pipeline.map(|p| (f, p))
        })
    }

    pub fn run<'h, T>(
        self,
        span: Span,
        value: &'h T,
        #[cfg(feature = "prometheus")] update_type: metrics::UpdateType,
    ) -> impl Future<Output = ()> + Send + 'h
    where
        H: DynPipeline<T>,
        'm: 'h,
    {
        let _span = span.entered();
        futures_util::future::join_all(self.get_pipelines().map(move |(f, h)| {
            h.handle(value)
                .map(move |r| {
                    #[cfg(feature = "prometheus")]
                    metrics::increment_processed_updates(&r, update_type);

                    match r {
                        Ok(()) => (),
                        Err(v) => v.handle::<T>(f.as_ref()).as_unit(),
                    }
                })
                .in_current_span()
        }))
        .map(move |v| v.into_iter().collect())
    }
}
