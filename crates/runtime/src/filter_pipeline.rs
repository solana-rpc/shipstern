//! `Pipeline` equivalent that allows for transaction custom filters

use std::{borrow::Cow, fmt::Debug};

use futures_util::{Future, StreamExt};
use shipstern_core::{GetPrefilter, ParseError, Parser, ParserId, Prefilter, PrefilterBuilder};
use smallvec::SmallVec;

use crate::{
    handler::{DynPipeline, PipelineErrors},
    Handler,
};

/// A wrapper around a parser that allows for custom prefilters to be applied to the transaction.
#[derive(Debug, Clone)]
pub struct FilterPipeline<P: Parser, H> {
    parser: P,
    handlers: H,
    additional_filters: Prefilter,
}

impl<P: Parser, H> GetPrefilter for FilterPipeline<P, H> {
    #[inline]
    fn prefilter(&self) -> Prefilter {
        let mut prefilter = self.parser.prefilter();
        prefilter.merge(self.additional_filters.clone());

        prefilter
    }
}

impl<P: Parser, H> ParserId for FilterPipeline<P, H> {
    #[inline]
    fn id(&self) -> Cow<'static, str> { self.parser.id() }
}

impl<P: Parser, H> FilterPipeline<P, H> {
    /// Create a new `FilterPipeline` with the given parser and handlers.
    ///  This allows to set custom transaction filters.
    ///
    /// # Example
    ///
    /// ```rust, ignore
    ///
    ///    shipstern::Runtime::builder()
    ///        .account(Pipeline::new(RaydiumAmmV4AccParser, [Logger]))
    ///        .instruction(FilterPipeline::new(RaydiumAmmV4IxParser, [RaydiumAmmV4IxLogger], Prefilter::builder()
    ///            .transaction_accounts_include([
    ///                KeyBytes::<32>::from_str("GH8Ers4yzKR3UKDvgVu8cqJfGzU4cU62mTeg9bcJ7ug6").unwrap(),
    ///                KeyBytes::<32>::from_str("4xxM4cdb6MEsCxM52xvYqkNbzvdeWWsPDZrBcTqVGUar").unwrap()
    ///            ])
    ///            .transaction_accounts([
    ///                KeyBytes::<32>::from_str("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA").unwrap()
    ///            ]),
    ///        ))
    ///        .build::<YellowstoneGrpcSource>(config)
    ///        .run();
    /// ```
    ///
    /// # Panics
    /// If the additional filters are invalid, this function will panic.
    pub fn new(parser: P, handlers: H, additional_filters: PrefilterBuilder) -> Self {
        Self {
            parser,
            handlers,
            additional_filters: additional_filters.build().unwrap(),
        }
    }
}

impl<P, H> FilterPipeline<P, H>
where
    P: Parser + Send + Sync,
    P::Input: Send + Sync,
    H: Debug + Sync,
    for<'i> &'i H: IntoIterator,
    for<'i> <&'i H as IntoIterator>::Item: Handler<P::Output, P::Input>,
{
    /// Handle fn for `FilterPipeline`
    ///
    /// # Errors
    /// If any of the related handlers executions errors, returns those errors
    pub async fn handle_value(&self, value: &P::Input) -> Result<(), PipelineErrors> {
        let parsed = match self.parser.parse(value).await {
            Ok(p) => p,
            Err(ParseError::Filtered) => return Ok(()),
            Err(ParseError::DiscriminatorNotFound(msg)) => {
                return Err(PipelineErrors::Parse(msg.into()));
            },
            Err(ParseError::Other(e)) => return Err(PipelineErrors::Parse(e)),
        };
        let parsed = &parsed;

        let errs = self
            .handlers
            .into_iter()
            .map(|h| async move { h.handle(parsed, value).await })
            .collect::<futures_util::stream::FuturesUnordered<_>>()
            .filter_map(|r| async move { r.err() })
            .collect::<SmallVec<[_; 1]>>()
            .await;

        if !errs.is_empty() {
            return Err(PipelineErrors::Handlers(errs));
        }

        Ok(())
    }
}

impl<P, H> DynPipeline<P::Input> for FilterPipeline<P, H>
where
    P: Parser + Debug + Send + Sync,
    P::Input: Send + Sync,
    P::Output: Send + Sync,
    H: Debug + Sync,
    for<'i> &'i H: IntoIterator,
    for<'i> <&'i H as IntoIterator>::Item: Handler<P::Output, P::Input> + Send,
{
    fn handle<'h>(
        &'h self,
        value: &'h P::Input,
    ) -> std::pin::Pin<
        Box<dyn Future<Output = Result<(), crate::handler::PipelineErrors>> + Send + 'h>,
    > {
        Box::pin(FilterPipeline::handle_value(self, value))
    }
}
