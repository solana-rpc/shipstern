//! From the runtime's prefilters to the feed's subscribe request.

use std::collections::{BTreeSet, HashMap};

use shipstern_core::{Filters, Pubkey, TransactionPrefilter};
use triton_preconfs_proto::preconfs::{
    subscribe_request::Region, ExecutionResult, SubscribeRequest, TransactionFilter,
};

/// Server limits from the Triton docs. A request over them is refused at subscribe.
const MAX_FILTERS: usize = 64;
const MAX_ACCOUNTS: usize = 10_000;
const MAX_FILTER_NAME_BYTES: usize = 64;

/// A prefilter the feed cannot express, named by its parser.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum FilterError {
    /// The feed carries transactions only.
    #[error("no parser has a transaction prefilter, and the feed carries only transactions")]
    NoTransactionFilters,
    /// The feed matches account keys only.
    #[error("parser {parser}: the feed cannot expand token-account owners")]
    TokenAccounts { parser: String },
    /// The feed refuses a filter that selects nothing on its own.
    #[error("parser {parser}: the feed needs an include, required or signature list")]
    NoPositiveSelector { parser: String },
    /// A parser ID is the filter name, and the feed caps its length.
    #[error(
        "parser {parser}: {len} bytes, the feed allows {MAX_FILTER_NAME_BYTES} per filter name"
    )]
    NameTooLong { parser: String, len: usize },
    /// Over the per-stream filter limit.
    #[error("{count} transaction filters, the feed allows {MAX_FILTERS}")]
    TooManyFilters { count: usize },
    /// Over the per-list account limit.
    #[error("parser {parser}: {count} accounts in one list, the feed allows {MAX_ACCOUNTS}")]
    TooManyAccounts { parser: String, count: usize },
}

impl From<FilterError> for shipstern::Error {
    fn from(err: FilterError) -> Self { Self::Other(Box::new(err)) }
}

/// The feed filters, keyed by parser ID so the echoed names route each update to its parser.
pub(crate) fn transactions(
    filters: &Filters,
) -> Result<HashMap<String, TransactionFilter>, FilterError> {
    let transactions = filters
        .parsers_filters
        .iter()
        .filter_map(|(parser, prefilter)| {
            let prefilter = prefilter.transaction.as_ref()?;
            Some(transaction_filter(parser, prefilter).map(|filter| (parser.clone(), filter)))
        })
        .collect::<Result<HashMap<_, _>, _>>()?;

    if transactions.is_empty() {
        return Err(FilterError::NoTransactionFilters);
    }
    if transactions.len() > MAX_FILTERS {
        return Err(FilterError::TooManyFilters {
            count: transactions.len(),
        });
    }

    Ok(transactions)
}

/// One request per feed and region, all with the same filters.
pub(crate) fn request(
    transactions: &HashMap<String, TransactionFilter>,
    region: Region,
) -> SubscribeRequest {
    SubscribeRequest {
        transactions: transactions.clone(),
        region: Some(region),
    }
}

/// The parsers that get nothing from this source, because they have no transaction prefilter.
pub(crate) fn unserved(filters: &Filters) -> impl Iterator<Item = &str> {
    filters
        .parsers_filters
        .iter()
        .filter(|(_, prefilter)| prefilter.transaction.is_none())
        .map(|(parser, _)| parser.as_str())
}

/// The parsers with an explicit `vote` wish. The feed has no vote field, so the stream
/// applies it after decoding.
pub(crate) fn vote_rules(filters: &Filters) -> HashMap<String, bool> {
    filters
        .parsers_filters
        .iter()
        .filter_map(|(parser, prefilter)| {
            Some((parser.clone(), prefilter.transaction.as_ref()?.vote?))
        })
        .collect()
}

fn transaction_filter(
    parser: &str,
    prefilter: &TransactionPrefilter,
) -> Result<TransactionFilter, FilterError> {
    if parser.len() > MAX_FILTER_NAME_BYTES {
        return Err(FilterError::NameTooLong {
            parser: parser.to_string(),
            len: parser.len(),
        });
    }
    if prefilter.token_accounts.is_some() {
        return Err(FilterError::TokenAccounts {
            parser: parser.to_string(),
        });
    }

    // The server reads each list as a set, so a sorted set gives one stable request.
    let list = |keys: &mut dyn Iterator<Item = &Pubkey>| -> Result<Vec<String>, FilterError> {
        let list: BTreeSet<String> = keys.map(ToString::to_string).collect();
        if list.len() > MAX_ACCOUNTS {
            return Err(FilterError::TooManyAccounts {
                parser: parser.to_string(),
                count: list.len(),
            });
        }
        Ok(list.into_iter().collect())
    };
    let account_include = list(
        &mut prefilter
            .accounts_include
            .iter()
            .chain(prefilter.cuckoo_accounts_include.iter()),
    )?;
    let account_required = list(&mut prefilter.accounts_required.iter())?;
    let account_exclude = list(&mut prefilter.accounts_exclude.iter())?;
    let signature = prefilter.signature.iter().cloned().collect::<Vec<_>>();

    if account_include.is_empty() && account_required.is_empty() && signature.is_empty() {
        return Err(FilterError::NoPositiveSelector {
            parser: parser.to_string(),
        });
    }

    // Unset means unknown on the feed, so only an explicit list excludes it server-side.
    let execution_results = match prefilter.failed {
        None => vec![],
        Some(false) => vec![ExecutionResult::Success.into()],
        Some(true) => vec![
            ExecutionResult::ExecutionFailure.into(),
            ExecutionResult::FeesOnly.into(),
        ],
    };

    Ok(TransactionFilter {
        account_include,
        account_exclude,
        account_required,
        signature,
        execution_results,
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};

    use shipstern_core::{
        AccountPrefilter, CuckooAccounts, Filters, Prefilter, Pubkey, TokenAccounts,
        TransactionPrefilter,
    };
    use triton_preconfs_proto::preconfs::{
        subscribe_request::Region, ExecutionResult, HarmonicRegion,
    };

    use super::{request, transactions, unserved, FilterError};

    fn key(n: u8) -> Pubkey { Pubkey::new([n; 32]) }

    fn transaction(prefilter: TransactionPrefilter) -> Prefilter {
        Prefilter {
            transaction: Some(prefilter),
            ..Default::default()
        }
    }

    fn filters(parsers: impl IntoIterator<Item = (&'static str, Prefilter)>) -> Filters {
        Filters::new(
            parsers
                .into_iter()
                .map(|(id, prefilter)| (id.to_string(), prefilter))
                .collect(),
        )
    }

    fn one_parser(id: &'static str, prefilter: TransactionPrefilter) -> Filters {
        filters([(id, transaction(prefilter))])
    }

    /// Each prefilter list lands in its feed list, and the cuckoo set joins the include list.
    #[test]
    fn lists_and_signature_map_to_the_feed_filter() {
        let filters = one_parser("p", TransactionPrefilter {
            accounts_include: HashSet::from([key(1)]),
            cuckoo_accounts_include: CuckooAccounts::from_iter([key(2)]),
            accounts_required: HashSet::from([key(3)]),
            accounts_exclude: HashSet::from([key(4)]),
            signature: Some("sig".to_string()),
            ..Default::default()
        });

        let region = Region::HarmonicRegion(HarmonicRegion::Fra as i32);
        let request = request(&transactions(&filters).unwrap(), region);

        let filter = &request.transactions["p"];
        let mut include = filter.account_include.clone();
        include.sort();
        assert_eq!(include, [key(1).to_string(), key(2).to_string()]);
        assert_eq!(filter.account_required, [key(3).to_string()]);
        assert_eq!(filter.account_exclude, [key(4).to_string()]);
        assert_eq!(filter.signature, ["sig"]);
        assert_eq!(request.region, Some(region));
    }

    /// `failed` becomes the server's result filter, so an unknown result never reads as success.
    #[test]
    fn failed_maps_to_execution_results() {
        let results = |failed| {
            let filters = one_parser("p", TransactionPrefilter {
                accounts_include: HashSet::from([key(1)]),
                failed,
                ..Default::default()
            });
            transactions(&filters).unwrap()["p"]
                .execution_results
                .clone()
        };

        assert_eq!(results(None), Vec::<i32>::new());
        assert_eq!(results(Some(false)), [ExecutionResult::Success as i32]);
        assert_eq!(results(Some(true)), [
            ExecutionResult::ExecutionFailure as i32,
            ExecutionResult::FeesOnly as i32
        ]);
    }

    /// An account parser gets no filter, because an empty filter is refused by the feed,
    /// and it is named as unserved so the operator learns it gets nothing.
    #[test]
    fn account_only_parsers_are_skipped_and_named() {
        let filters = filters([
            ("accounts", Prefilter {
                account: Some(AccountPrefilter::default()),
                ..Default::default()
            }),
            (
                "txs",
                transaction(TransactionPrefilter {
                    accounts_include: HashSet::from([key(1)]),
                    ..Default::default()
                }),
            ),
        ]);

        let transactions = transactions(&filters).unwrap();

        assert_eq!(transactions.keys().collect::<Vec<_>>(), ["txs"]);
        assert_eq!(unserved(&filters).collect::<Vec<_>>(), ["accounts"]);
    }

    /// A runtime with no transaction parser would subscribe to nothing.
    #[test]
    fn no_transaction_filter_is_refused() {
        let filters = filters([("accounts", Prefilter {
            account: Some(AccountPrefilter::default()),
            ..Default::default()
        })]);

        let result = transactions(&filters);

        assert!(matches!(result, Err(FilterError::NoTransactionFilters)));
    }

    /// The feed refuses an exclude-only filter, so name the parser before the dial.
    #[test]
    fn a_filter_with_only_excludes_is_refused() {
        let filters = one_parser("p", TransactionPrefilter {
            accounts_exclude: HashSet::from([key(1)]),
            ..Default::default()
        });

        let result = transactions(&filters);

        assert!(matches!(result, Err(FilterError::NoPositiveSelector { parser }) if parser == "p"));
    }

    /// Token-account expansion has no feed field, so dropping it silently would widen nothing
    /// and narrow everything.
    #[test]
    fn token_accounts_is_refused() {
        let filters = one_parser("p", TransactionPrefilter {
            accounts_include: HashSet::from([key(1)]),
            token_accounts: Some(TokenAccounts::All),
            ..Default::default()
        });

        let result = transactions(&filters);

        assert!(matches!(result, Err(FilterError::TokenAccounts { parser }) if parser == "p"));
    }

    /// The feed caps a filter name at 64 bytes, and a parser ID is the name.
    #[test]
    fn a_long_parser_id_is_refused() {
        let id: &'static str = "p".repeat(65).leak();
        let filters = one_parser(id, TransactionPrefilter {
            accounts_include: HashSet::from([key(1)]),
            ..Default::default()
        });

        let result = transactions(&filters);

        assert!(matches!(
            result,
            Err(FilterError::NameTooLong { len: 65, .. })
        ));
    }

    /// The feed allows 64 filters per stream.
    #[test]
    fn more_than_64_filters_is_refused() {
        let parsers: Vec<(String, Prefilter)> = (0..65)
            .map(|i| {
                (
                    format!("p{i}"),
                    transaction(TransactionPrefilter {
                        accounts_include: HashSet::from([key(1)]),
                        ..Default::default()
                    }),
                )
            })
            .collect();
        let filters = Filters::new(parsers.into_iter().collect::<HashMap<_, _>>());

        let result = transactions(&filters);

        assert!(matches!(
            result,
            Err(FilterError::TooManyFilters { count: 65 })
        ));
    }

    /// The feed allows 10000 accounts per list, counted after the cuckoo set joins it and
    /// after a key in both sets collapses to one.
    #[test]
    fn more_than_10000_accounts_is_refused() {
        let generated = |i: u32| Pubkey::new(i.to_le_bytes().repeat(8).try_into().unwrap());
        let filters = one_parser("p", TransactionPrefilter {
            accounts_include: (0..10_000).map(generated).collect(),
            cuckoo_accounts_include: CuckooAccounts::from_iter([generated(0), key(255)]),
            ..Default::default()
        });

        let result = transactions(&filters);

        assert!(matches!(
            result,
            Err(FilterError::TooManyAccounts { parser, count: 10_001 }) if parser == "p"
        ));
    }
}
