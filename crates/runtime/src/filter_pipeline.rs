//! `Pipeline` equivalent that allows for transaction custom filters

use std::{borrow::Cow, fmt::Debug};

use futures_util::{Future, StreamExt};
use shipstern_core::{
    CuckooAccounts, GetPrefilter, ParseError, Parser, ParserId, Prefilter, PrefilterBuilder,
    Pubkey, TransactionPrefilter, TransactionUpdate,
};
use smallvec::SmallVec;

use crate::{
    handler::{DynPipeline, PipelineErrors},
    Handler,
};

/// A wrapper around a parser that allows for custom prefilters to be applied to the transaction.
///
/// A plain transaction include replaces the parser's own include, so it narrows the parser. A
/// cuckoo-only include keeps the program as a required key, because a server without cuckoo
/// support ignores the cuckoo set. The vote and signature values always apply. An exclude set
/// applies only in its own token mode. Other fields merge with [`Prefilter::merge`].
///
/// All instruction parsers of a runtime share one server filter. So a required key here also
/// narrows the other parsers. Their keys can bring transactions that this filter rejects, so a
/// bundle checks the transaction filter again in the client and skips those.
///
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
        let user = self.additional_filters.clone();
        let user_tx = user.transaction.clone();
        let mut own_narrowing = None;

        if let Some(own) = prefilter.transaction.as_mut()
            && let Some(user_tx) = user_tx.as_ref()
        {
            if !user_tx.accounts_include.is_empty() {
                // A union with the program key would match every transaction of the program again.
                own.accounts_include.clear();
                own.cuckoo_accounts_include = CuckooAccounts::default();
            } else if !user_tx.cuckoo_accounts_include.is_empty() {
                // A server without cuckoo support ignores the user set, so the program stays required.
                let program = std::mem::take(&mut own.accounts_include);
                own.accounts_required.extend(program);
            }

            own_narrowing = Some((
                own.accounts_exclude.clone(),
                own.vote,
                own.signature.clone(),
                own.token_accounts,
            ));
        }

        prefilter.merge(user);

        // `merge` widens these fields, so apply them again. A user vote or signature wins.
        if let Some(tx) = prefilter.transaction.as_mut()
            && let Some(user_tx) = user_tx
        {
            let (own_exclude, own_vote, own_signature, own_token) =
                own_narrowing.unwrap_or_default();
            // In a token mode, exclude also matches token-balance owners. So an exclude set
            // applies again only in the mode it was made for.
            let own_exclude = (own_token == tx.token_accounts).then_some(own_exclude);
            let user_exclude =
                (user_tx.token_accounts == tx.token_accounts).then_some(user_tx.accounts_exclude);

            tx.accounts_exclude = own_exclude
                .into_iter()
                .chain(user_exclude)
                .flatten()
                .collect();
            tx.vote = user_tx.vote.or(own_vote);
            tx.signature = user_tx.signature.or(own_signature);
        }

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

    fn client_filter(&self) -> Option<&TransactionPrefilter> {
        self.additional_filters.transaction.as_ref()
    }
}

/// Whether `txn` matches `filter`. A field that the client cannot judge passes. Cuckoo keys
/// are checked exactly, so a false cuckoo match is dropped.
pub(crate) fn transaction_matches(filter: &TransactionPrefilter, txn: &TransactionUpdate) -> bool {
    let Some(info) = txn.transaction.as_ref() else {
        return true;
    };

    if filter.vote.is_some_and(|vote| vote != info.is_vote) {
        return false;
    }

    if let Some(signature) = &filter.signature
        && shipstern_core::bs58::encode(&info.signature).into_string() != *signature
    {
        return false;
    }

    // Token-balance owners can also match the account sets, and the client cannot see them.
    if filter.token_accounts.is_some() {
        return true;
    }

    let (Some(tx), Some(meta)) = (info.transaction.as_ref(), info.meta.as_ref()) else {
        return true;
    };
    let Some(message) = tx.message.as_ref() else {
        return true;
    };

    let has = |key: &Pubkey| {
        let key: &[u8] = key.as_ref();

        message
            .account_keys
            .iter()
            .chain(&meta.loaded_writable_addresses)
            .chain(&meta.loaded_readonly_addresses)
            .any(|k| k.as_slice() == key)
    };

    let no_include =
        filter.accounts_include.is_empty() && filter.cuckoo_accounts_include.is_empty();
    let included = no_include
        || filter
            .accounts_include
            .iter()
            .chain(filter.cuckoo_accounts_include.iter())
            .any(has);

    included && filter.accounts_required.iter().all(has) && !filter.accounts_exclude.iter().any(has)
}

#[cfg(test)]
mod tests {
    use shipstern_core::{ParseResult, Pubkey, TokenAccounts, TransactionPrefilter};

    use super::*;

    const PROGRAM: [u8; 32] = [1; 32];
    const USER: [u8; 32] = [2; 32];

    #[derive(Debug, Clone, Copy)]
    struct ProgramParser {
        cuckoo: bool,
        token: Option<TokenAccounts>,
    }

    impl Parser for ProgramParser {
        type Input = ();
        type Output = ();

        fn id(&self) -> Cow<'static, str> { "test::Program".into() }

        fn prefilter(&self) -> Prefilter {
            let builder = Prefilter::builder();
            let builder = if self.cuckoo {
                builder.transaction_cuckoo_accounts_include([PROGRAM])
            } else {
                builder.transaction_accounts_include([PROGRAM])
            };
            let builder = match self.token {
                Some(mode) => builder.transaction_token_accounts(mode),
                None => builder,
            };

            builder.build().expect("valid prefilter")
        }

        async fn parse(&self, _value: &()) -> ParseResult<()> { Ok(()) }
    }

    fn sent(cuckoo: bool, user: PrefilterBuilder) -> TransactionPrefilter {
        FilterPipeline::new(
            ProgramParser {
                cuckoo,
                token: None,
            },
            (),
            user,
        )
        .prefilter()
        .transaction
        .expect("transaction prefilter")
    }

    fn keys(keys: &[[u8; 32]]) -> std::collections::HashSet<Pubkey> {
        keys.iter().copied().map(Pubkey::new).collect()
    }

    #[test]
    fn user_filters_narrow_the_parser() {
        let include = sent(
            false,
            Prefilter::builder().transaction_accounts_include([USER]),
        );
        assert_eq!(include.accounts_include, keys(&[USER]));
        assert!(include.accounts_required.is_empty());

        let cuckoo_only = sent(
            false,
            Prefilter::builder().transaction_cuckoo_accounts_include([USER]),
        );
        assert!(cuckoo_only.accounts_include.is_empty());
        assert_eq!(cuckoo_only.accounts_required, keys(&[PROGRAM]));

        let required_only = sent(false, Prefilter::builder().transaction_accounts([USER]));
        assert_eq!(required_only.accounts_include, keys(&[PROGRAM]));
        assert_eq!(required_only.accounts_required, keys(&[USER]));

        let own_cuckoo = sent(
            true,
            Prefilter::builder().transaction_accounts_include([USER]),
        );
        assert!(own_cuckoo.cuckoo_accounts_include.is_empty());

        let narrowing = sent(
            false,
            Prefilter::builder()
                .transaction_accounts_include([USER])
                .transaction_accounts_exclude([[3; 32]])
                .transaction_vote(false)
                .transaction_signature("sig"),
        );
        assert_eq!(narrowing.accounts_exclude, keys(&[[3; 32]]));
        assert_eq!(narrowing.vote, Some(false));
        assert_eq!(narrowing.signature.as_deref(), Some("sig"));

        // In a token mode, a keys-only exclude would also drop a key that is a token owner.
        let token_parser = FilterPipeline::new(
            ProgramParser {
                cuckoo: false,
                token: Some(TokenAccounts::All),
            },
            (),
            Prefilter::builder().transaction_accounts_exclude([[3; 32]]),
        )
        .prefilter()
        .transaction
        .expect("transaction prefilter");
        assert!(token_parser.accounts_exclude.is_empty());

        let token_user = sent(
            false,
            Prefilter::builder()
                .transaction_accounts_exclude([[3; 32]])
                .transaction_token_accounts(TokenAccounts::All),
        );
        assert_eq!(token_user.accounts_exclude, keys(&[[3; 32]]));
    }
}
