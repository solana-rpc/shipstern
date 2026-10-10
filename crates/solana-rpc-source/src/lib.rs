use std::{str::FromStr, time::Duration};

use async_trait::async_trait;
use shipstern::{
    sources::{FromConfig, SourceContext, SourceExitStatus, SourceTrait},
    CommitmentLevel, Error as ShipsternError,
};
use solana_account_decoder_client_types::UiAccountEncoding;
use solana_client::{
    nonblocking::rpc_client::RpcClient,
    rpc_config::{RpcAccountInfoConfig, RpcProgramAccountsConfig},
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

#[async_trait]
impl SourceTrait for SolanaAccountsRpcSource {
    #[allow(deprecated)] // get_program_accounts_with_config is deprecated but replacement not yet stable
    async fn connect(&self, ctx: SourceContext) -> Result<SourceExitStatus, ShipsternError> {
        let SourceContext { filters, tx, .. } = ctx;
        let config = &self.config;

        let mut tasks_set = JoinSet::new();

        for (filter_id, prefilter) in &filters.parsers_filters {
            if let Some(account_prefilter) = &prefilter.account {
                for program in &account_prefilter.owners {
                    let program_id = Pubkey::new_from_array(program.0);
                    let config = config.clone();
                    let tx = tx.clone();
                    let filter_id = filter_id.clone();

                    let client = RpcClient::new_with_timeout_and_commitment(
                        config.endpoint.clone(),
                        Duration::from_secs(config.timeout),
                        self.get_commitment_config(),
                    );

                    tasks_set.spawn(async move {
                        let slot = client.get_slot().await.map_err(|e| {
                            format!(
                                "Failed to get slot for source: solana-rpc, filter: {filter_id}: \
                                 {e}"
                            )
                        })?;

                        // solana-client 4.x dropped `get_program_accounts_with_config`;
                        // the config-taking call now yields RPC-encoded `UiAccount`s.
                        let accounts = client
                            .get_program_ui_accounts_with_config(
                                &program_id,
                                RpcProgramAccountsConfig {
                                    filters: None,
                                    account_config: RpcAccountInfoConfig {
                                        encoding: Some(UiAccountEncoding::Base64),
                                        data_slice: None,
                                        commitment: None, // Already set in the client
                                        min_context_slot: None,
                                    },
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

                        for (acc_pubkey, account) in accounts {
                            let owner = Pubkey::from_str(&account.owner).map_err(|e| {
                                format!(
                                    "Failed to parse owner {} for source: solana-rpc, filter: \
                                     {filter_id}: {e}",
                                    account.owner
                                )
                            })?;

                            let Some(data) = account.data.decode() else {
                                return Err(format!(
                                    "Failed to decode account data for {acc_pubkey} from source: \
                                     solana-rpc, filter: {filter_id}"
                                ));
                            };

                            let update = SubscribeUpdate {
                                filters: vec![filter_id.clone()],
                                created_at: None,
                                update_oneof: Some(UpdateOneof::Account(SubscribeUpdateAccount {
                                    account: Some(SubscribeUpdateAccountInfo {
                                        pubkey: acc_pubkey.as_array().to_vec(),
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
                                })),
                            };

                            let res = tx.send(Ok(update)).await;

                            if res.is_err() {
                                return Err(format!(
                                    "Failed to send update to buffer for source: solana-rpc, \
                                     filter: {filter_id}"
                                ));
                            }
                        }

                        Ok::<(), String>(())
                    });
                }
            }
        }

        // One task runs per (filter, owner) pair and the runtime treats a source
        // `Err` as fatal, so the first failure wins: keeping later errors adds
        // nothing, and updates already sent by sibling tasks stay in the buffer.
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
