use std::{str::FromStr, time::Duration};

use async_trait::async_trait;
use shipstern::{
    sources::{FromConfig, SourceContext, SourceExitStatus, SourceTrait},
    CommitmentLevel, Error as ShipsternError,
};
use shipstern_core::{AccountFilter, AccountPrefilter, LamportsCmp, MemcmpData};
use solana_account_decoder_client_types::{UiAccount, UiAccountEncoding};
use solana_client::{
    nonblocking::rpc_client::RpcClient,
    rpc_config::{RpcAccountInfoConfig, RpcProgramAccountsConfig},
    rpc_filter::{Memcmp, MemcmpEncodedBytes, RpcFilterType},
    rpc_request::MAX_MULTIPLE_ACCOUNTS,
};
use solana_commitment_config::CommitmentConfig;
use solana_pubkey::Pubkey;
use tokio::task::JoinSet;
use yellowstone_grpc_proto::geyser::{
    subscribe_update::UpdateOneof, SubscribeUpdate, SubscribeUpdateAccount,
    SubscribeUpdateAccountInfo,
};

/// A `Source` implementation for the Solana Accounts RPC API.
#[derive(Debug)]
pub struct SolanaAccountsRpcSource {
    config: SolanaAccountsRpcConfig,
}

/// The configuration for the Solana Accounts RPC source.
#[derive(Debug, Clone, Default, serde::Deserialize, clap::Args)]
pub struct SolanaAccountsRpcConfig {
    /// The endpoint of the RPC server.
    #[arg(long, env)]
    pub endpoint: String,
    /// The timeout for the connection.
    #[arg(long, env)]
    pub timeout: u64,

    /// The commitment of the RPC calls. Unset gives confirmed.
    #[arg(long, env)]
    pub commitment_level: Option<CommitmentLevel>,
}

impl SolanaAccountsRpcSource {
    fn get_commitment_config(&self) -> CommitmentConfig {
        match self.config.commitment_level {
            Some(CommitmentLevel::Finalized) => CommitmentConfig::finalized(),
            Some(CommitmentLevel::Processed) => CommitmentConfig::processed(),
            _ => CommitmentConfig::confirmed(),
        }
    }
}

impl FromConfig for SolanaAccountsRpcSource {
    type Config = SolanaAccountsRpcConfig;

    fn from_config(config: Self::Config) -> Self { Self { config } }
}

/// The RPC form of `filter`. `Lamports` has none, so the client checks it.
fn rpc_filter(filter: &AccountFilter) -> Option<RpcFilterType> {
    match filter {
        AccountFilter::Memcmp { offset, data } => {
            let bytes = match data {
                MemcmpData::Bytes(bytes) => MemcmpEncodedBytes::Bytes(bytes.clone()),
                MemcmpData::Base58(text) => MemcmpEncodedBytes::Base58(text.clone()),
                MemcmpData::Base64(text) => MemcmpEncodedBytes::Base64(text.clone()),
            };

            Some(RpcFilterType::Memcmp(Memcmp::new(
                usize::try_from(*offset).ok()?,
                bytes,
            )))
        },
        AccountFilter::DataSize(size) => Some(RpcFilterType::DataSize(*size)),
        // Prefilter validation rejects `TokenAccountState(false)`.
        AccountFilter::TokenAccountState(_) => Some(RpcFilterType::TokenAccountState),
        AccountFilter::Lamports(_) => None,
    }
}

/// Whether an account passes every filter, checked in the client. It mirrors the
/// server's checks for accounts fetched by key and for the `Lamports` filter.
fn passes_filters(filters: &[AccountFilter], data: &[u8], lamports: u64) -> bool {
    filters.iter().all(|filter| match filter {
        AccountFilter::Lamports(cmp) => match *cmp {
            LamportsCmp::Eq(value) => lamports == value,
            LamportsCmp::Ne(value) => lamports != value,
            LamportsCmp::Lt(value) => lamports < value,
            LamportsCmp::Gt(value) => lamports > value,
        },
        AccountFilter::TokenAccountState(_) => is_token_account(data),
        other => rpc_filter(other).is_some_and(|f| match f {
            RpcFilterType::DataSize(size) => data.len() as u64 == size,
            RpcFilterType::Memcmp(memcmp) => memcmp.bytes_match(data),
            RpcFilterType::TokenAccountState => is_token_account(data),
        }),
    })
}

/// An initialized SPL Token or Token-2022 account, as the RPC `tokenAccountState` filter
/// tests it.
fn is_token_account(data: &[u8]) -> bool {
    const ACCOUNT_LEN: usize = 165;
    const MULTISIG_LEN: usize = 355;
    // `AccountState` byte; 0 is uninitialized.
    const STATE_OFFSET: usize = 108;
    // Token-2022 `AccountType::Account`, right after the base layout.
    const ACCOUNT_TYPE_ACCOUNT: u8 = 2;

    let initialized = data.get(STATE_OFFSET).is_some_and(|&state| state != 0);

    match data.len() {
        ACCOUNT_LEN => initialized,
        MULTISIG_LEN => false,
        len if len > ACCOUNT_LEN => initialized && data[ACCOUNT_LEN] == ACCOUNT_TYPE_ACCOUNT,
        _ => false,
    }
}

/// The update for `account`, or `None` when it fails one of `filters`.
fn account_update(
    filter_id: &str,
    filters: &[AccountFilter],
    slot: u64,
    pubkey: &Pubkey,
    account: UiAccount,
) -> Result<Option<SubscribeUpdate>, String> {
    let owner = Pubkey::from_str(&account.owner).map_err(|e| {
        format!(
            "Failed to parse owner {} for source: solana-rpc, filter: {filter_id}: {e}",
            account.owner
        )
    })?;

    let Some(data) = account.data.decode() else {
        return Err(format!(
            "Failed to decode account data for {pubkey} from source: solana-rpc, filter: \
             {filter_id}"
        ));
    };

    if !passes_filters(filters, &data, account.lamports) {
        return Ok(None);
    }

    Ok(Some(SubscribeUpdate {
        filters: vec![filter_id.to_owned()],
        created_at: None,
        update_oneof: Some(UpdateOneof::Account(SubscribeUpdateAccount {
            account: Some(SubscribeUpdateAccountInfo {
                pubkey: pubkey.as_array().to_vec(),
                lamports: account.lamports,
                owner: owner.as_array().to_vec(),
                executable: account.executable,
                rent_epoch: account.rent_epoch,
                data,
                write_version: 0,
                txn_signature: None,
            }),
            slot,
            is_startup: true,
            bank_id: None,
        })),
    }))
}

/// Sends `accounts` that pass `filters` as updates for `filter_id`.
async fn send_accounts<E>(
    tx: &tokio::sync::mpsc::Sender<Result<SubscribeUpdate, E>>,
    filter_id: &str,
    filters: &[AccountFilter],
    slot: u64,
    accounts: impl IntoIterator<Item = (Pubkey, UiAccount)>,
) -> Result<(), String> {
    for (pubkey, account) in accounts {
        let Some(update) = account_update(filter_id, filters, slot, &pubkey, account)? else {
            continue;
        };

        if tx.send(Ok(update)).await.is_err() {
            return Err(format!(
                "Failed to send update to buffer for source: solana-rpc, filter: {filter_id}"
            ));
        }
    }

    Ok(())
}

/// How the source fetches the accounts of one prefilter. The gRPC server ANDs the fields of a
/// filter: with keys set, only those keys can match, and the owners narrow them further.
#[derive(Debug, PartialEq)]
enum AccountQuery {
    /// `getProgramAccounts` for each owner.
    Owners(Vec<Pubkey>),
    /// `getMultipleAccounts` for the keys. When `owners` is not empty, only accounts with a
    /// listed owner are kept.
    Keys {
        keys: Vec<Pubkey>,
        owners: Vec<Pubkey>,
    },
}

impl AccountQuery {
    /// `None` when the prefilter has neither keys nor owners. Such a prefilter asks for every
    /// account that passes its filters, which the RPC API cannot fetch.
    fn new(prefilter: &AccountPrefilter) -> Option<Self> {
        let keys = sorted_keys(
            prefilter
                .accounts
                .iter()
                .chain(prefilter.cuckoo_accounts.iter()),
        );
        let owners = sorted_keys(prefilter.owners.iter());

        if !keys.is_empty() {
            return Some(Self::Keys { keys, owners });
        }

        if owners.is_empty() {
            return None;
        }

        Some(Self::Owners(owners))
    }
}

fn sorted_keys<'a>(keys: impl Iterator<Item = &'a shipstern_core::Pubkey>) -> Vec<Pubkey> {
    keys.map(|key| Pubkey::new_from_array(key.0))
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// Whether an account fetched by key has an owner in `owners`; an empty list allows any owner.
fn owner_listed(owners: &[Pubkey], owner: &str) -> bool {
    owners.is_empty() || Pubkey::from_str(owner).is_ok_and(|owner| owners.contains(&owner))
}

fn account_config() -> RpcAccountInfoConfig {
    RpcAccountInfoConfig {
        encoding: Some(UiAccountEncoding::Base64),
        data_slice: None,
        commitment: None, // Already set in the client
        min_context_slot: None,
    }
}

#[async_trait]
impl SourceTrait for SolanaAccountsRpcSource {
    #[allow(deprecated)] // get_program_accounts_with_config is deprecated but replacement not yet stable
    async fn connect(&self, ctx: SourceContext) -> Result<SourceExitStatus, ShipsternError> {
        let SourceContext { filters, tx, .. } = ctx;
        let config = &self.config;

        let mut tasks_set = JoinSet::new();

        for (filter_id, prefilter) in &filters.parsers_filters {
            let Some(account_prefilter) = &prefilter.account else {
                continue;
            };

            let client = || {
                RpcClient::new_with_timeout_and_commitment(
                    config.endpoint.clone(),
                    Duration::from_secs(config.timeout),
                    self.get_commitment_config(),
                )
            };

            // The RPC API has no call that scans every account, so a prefilter without keys or
            // owners cannot be served. On gRPC it receives every account that passes its filters.
            let Some(query) = AccountQuery::new(account_prefilter) else {
                tracing::warn!(
                    %filter_id,
                    "The solana-rpc source fetches accounts by key or owner only, and this \
                     account prefilter sets neither, so it receives no accounts"
                );
                continue;
            };

            match query {
                AccountQuery::Owners(owners) => {
                    for program_id in owners {
                        let tx = tx.clone();
                        let filter_id = filter_id.clone();
                        let filters = account_prefilter.filters.clone();
                        let client = client();

                        tasks_set.spawn(async move {
                            let slot = client.get_slot().await.map_err(|e| {
                                format!(
                                    "Failed to get slot for source: solana-rpc, filter: \
                                     {filter_id}: {e}"
                                )
                            })?;

                            // solana-client 4.x dropped `get_program_accounts_with_config`;
                            // the config-taking call now yields RPC-encoded `UiAccount`s.
                            let accounts = client
                                .get_program_ui_accounts_with_config(
                                    &program_id,
                                    RpcProgramAccountsConfig {
                                        filters: Some(
                                            filters.iter().filter_map(rpc_filter).collect(),
                                        ),
                                        account_config: account_config(),
                                        with_context: Some(true),
                                        sort_results: Some(false),
                                    },
                                )
                                .await
                                .map_err(|e| {
                                    format!(
                                        "Failed to get program accounts for source: solana-rpc, \
                                         filter: {filter_id}: {e}"
                                    )
                                })?;

                            send_accounts(&tx, &filter_id, &filters, slot, accounts).await
                        });
                    }
                },
                AccountQuery::Keys { keys, owners } => {
                    let tx = tx.clone();
                    let filter_id = filter_id.clone();
                    let filters = account_prefilter.filters.clone();
                    let client = client();

                    tasks_set.spawn(async move {
                        for chunk in keys.chunks(MAX_MULTIPLE_ACCOUNTS) {
                            let response = client
                                .get_multiple_ui_accounts_with_config(chunk, account_config())
                                .await
                                .map_err(|e| {
                                    format!(
                                        "Failed to get accounts for source: solana-rpc, filter: \
                                         {filter_id}: {e}"
                                    )
                                })?;

                            let accounts = chunk
                                .iter()
                                .copied()
                                .zip(response.value)
                                .filter_map(|(key, account)| Some((key, account?)))
                                .filter(|(_, account)| owner_listed(&owners, &account.owner));

                            send_accounts(
                                &tx,
                                &filter_id,
                                &filters,
                                response.context.slot,
                                accounts,
                            )
                            .await?;
                        }

                        Ok::<(), String>(())
                    });
                },
            }
        }

        // One task runs per owner of a filter without keys and per filter with keys, and the
        // runtime treats a source `Err` as fatal, so the first failure wins: keeping later
        // errors adds nothing, and updates already sent by sibling tasks stay in the buffer.
        let mut first_error: Option<String> = None;

        while let Some(task_result) = tasks_set.join_next().await {
            let msg = match task_result {
                Ok(Ok(())) => continue,
                Ok(Err(msg)) => {
                    tracing::error!(%msg, "Solana RPC source task failed");
                    msg
                },
                Err(e) => {
                    tracing::error!(err = %e, "Solana RPC source task panicked or was cancelled");
                    e.to_string()
                },
            };

            first_error.get_or_insert(msg);
        }

        if let Some(msg) = first_error {
            return Err(ShipsternError::Other(msg.into()));
        }

        Ok(SourceExitStatus::Completed)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use shipstern::{
        sources::{FromConfig, SourceContext, SourceTrait},
        CommitmentLevel,
    };
    use shipstern_core::{Filters, Prefilter};
    use tokio::sync::mpsc;

    use super::{SolanaAccountsRpcConfig, SolanaAccountsRpcSource};

    #[test]
    fn client_filters_match_the_server_rules() {
        use shipstern_core::{AccountFilter, LamportsCmp, MemcmpData};

        use super::passes_filters;

        let data = [7_u8, 8, 9, 10];
        let memcmp = |offset, bytes: &[u8]| AccountFilter::Memcmp {
            offset,
            data: MemcmpData::Bytes(bytes.to_vec()),
        };

        assert!(passes_filters(&[], &data, 0));
        assert!(passes_filters(&[AccountFilter::DataSize(4)], &data, 0));
        assert!(!passes_filters(&[AccountFilter::DataSize(5)], &data, 0));
        assert!(passes_filters(&[memcmp(1, &[8, 9])], &data, 0));
        assert!(!passes_filters(&[memcmp(1, &[9])], &data, 0));
        assert!(!passes_filters(&[memcmp(3, &[10, 11])], &data, 0));
        assert!(passes_filters(
            &[AccountFilter::Memcmp {
                offset: 0,
                data: MemcmpData::Base58(shipstern_core::bs58::encode([7_u8, 8]).into_string()),
            }],
            &data,
            0
        ));
        assert!(passes_filters(
            &[AccountFilter::Lamports(LamportsCmp::Gt(5))],
            &data,
            6
        ));
        assert!(!passes_filters(
            &[AccountFilter::Lamports(LamportsCmp::Lt(5))],
            &data,
            6
        ));

        // Filters are ANDed.
        assert!(!passes_filters(
            &[AccountFilter::DataSize(4), memcmp(0, &[1])],
            &data,
            0
        ));
    }

    #[test]
    fn token_account_state_matches_initialized_token_accounts() {
        use super::is_token_account;

        let mut account = vec![0_u8; 165];
        assert!(!is_token_account(&account), "uninitialized");

        account[108] = 1;
        assert!(is_token_account(&account), "SPL Token account");

        let mut extended = account.clone();
        extended.extend_from_slice(&[2, 0, 0]);
        assert!(
            is_token_account(&extended),
            "Token-2022 account with extensions"
        );

        extended[165] = 1;
        assert!(!is_token_account(&extended), "Token-2022 mint");

        let mut multisig = vec![1_u8; 355];
        multisig[165] = 2;
        assert!(!is_token_account(&multisig), "multisig");
    }

    #[test]
    fn keys_with_owners_fetch_only_the_keys_and_keep_listed_owners() {
        use super::{owner_listed, AccountQuery};

        let key = [1_u8; 32];
        let owner = [2_u8; 32];
        let other = solana_pubkey::Pubkey::new_from_array([3; 32]);
        let account = |prefilter: Prefilter| prefilter.account.expect("account prefilter");

        let both = account(
            Prefilter::builder()
                .accounts([key])
                .account_owners([owner])
                .build()
                .unwrap(),
        );
        let Some(AccountQuery::Keys { keys, owners }) = AccountQuery::new(&both) else {
            panic!("keys and owners should fetch only the keys");
        };
        assert_eq!(keys, vec![solana_pubkey::Pubkey::new_from_array(key)]);
        assert!(owner_listed(
            &owners,
            &solana_pubkey::Pubkey::new_from_array(owner).to_string()
        ));
        assert!(!owner_listed(&owners, &other.to_string()));

        let only_keys = account(Prefilter::builder().accounts([key]).build().unwrap());
        let Some(AccountQuery::Keys { owners, .. }) = AccountQuery::new(&only_keys) else {
            panic!("keys should fetch the keys");
        };
        assert!(owner_listed(&owners, &other.to_string()), "any owner");

        let only_owners = account(
            Prefilter::builder()
                .account_owners([owner])
                .build()
                .unwrap(),
        );
        assert_eq!(
            AccountQuery::new(&only_owners),
            Some(AccountQuery::Owners(vec![
                solana_pubkey::Pubkey::new_from_array(owner)
            ]))
        );
    }

    #[test]
    fn prefilters_without_keys_or_owners_have_no_query() {
        use shipstern_core::AccountFilter;

        use super::AccountQuery;

        let only_filters = Prefilter::builder()
            .account_filters([AccountFilter::DataSize(165)])
            .build()
            .unwrap();
        let every_account = Prefilter::builder().accounts_include_all().build().unwrap();

        for prefilter in [only_filters, every_account] {
            let account = prefilter.account.expect("account prefilter");
            assert_eq!(AccountQuery::new(&account), None);
        }
    }

    #[test]
    fn lamports_has_no_rpc_filter() {
        use shipstern_core::{AccountFilter, LamportsCmp};

        assert!(super::rpc_filter(&AccountFilter::Lamports(LamportsCmp::Eq(1))).is_none());
        assert!(super::rpc_filter(&AccountFilter::DataSize(165)).is_some());
    }

    #[test]
    fn connect_reports_rpc_errors_instead_of_panicking() {
        let runtime = tokio::runtime::Runtime::new().expect("runtime should build");

        runtime.block_on(async {
            let filters = Filters::new(HashMap::from([(
                "test-filter".to_string(),
                Prefilter::builder()
                    .account_owners([[1_u8; 32]])
                    .build()
                    .expect("account owner filter should build"),
            )]));
            let source = SolanaAccountsRpcSource::from_config(SolanaAccountsRpcConfig {
                endpoint: "http://127.0.0.1:9".to_string(),
                timeout: 1,
                commitment_level: Some(CommitmentLevel::Confirmed),
            });
            let (tx, mut rx) = mpsc::channel(1);

            let result = source.connect(SourceContext::new(filters, tx)).await;

            let Err(shipstern::Error::Other(err)) = result else {
                panic!("expected a connect error, got {result:?}");
            };
            assert!(err
                .to_string()
                .contains("Failed to get slot for source: solana-rpc"));
            assert!(rx.try_recv().is_err());
        });
    }

    #[test]
    fn connect_with_multiple_filters_reports_one_error() {
        let runtime = tokio::runtime::Runtime::new().expect("runtime should build");

        runtime.block_on(async {
            let filters = Filters::new(HashMap::from([
                (
                    "filter-a".to_string(),
                    Prefilter::builder()
                        .account_owners([[1_u8; 32]])
                        .build()
                        .expect("account owner filter should build"),
                ),
                (
                    "filter-b".to_string(),
                    Prefilter::builder()
                        .account_owners([[2_u8; 32]])
                        .build()
                        .expect("account owner filter should build"),
                ),
            ]));
            let source = SolanaAccountsRpcSource::from_config(SolanaAccountsRpcConfig {
                endpoint: "http://127.0.0.1:9".to_string(),
                timeout: 1,
                commitment_level: Some(CommitmentLevel::Confirmed),
            });
            let (tx, mut rx) = mpsc::channel(1);

            let result = source.connect(SourceContext::new(filters, tx)).await;

            let Err(shipstern::Error::Other(err)) = result else {
                panic!("expected a connect error, got {result:?}");
            };
            assert!(err
                .to_string()
                .contains("Failed to get slot for source: solana-rpc"));
            assert!(rx.try_recv().is_err());
        });
    }
}
