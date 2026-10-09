use std::{str::FromStr, time::Duration};

use async_trait::async_trait;
use shipstern::{
    sources::{SourceExitStatus, SourceTrait},
    CommitmentLevel, Error as ShipsternError,
};
use shipstern_core::Filters;
use solana_account_decoder_client_types::UiAccountEncoding;
use solana_client::{
    nonblocking::rpc_client::RpcClient,
    rpc_config::{RpcAccountInfoConfig, RpcProgramAccountsConfig},
};
use solana_commitment_config::CommitmentConfig;
use solana_pubkey::Pubkey;
use tokio::{
    sync::{mpsc::Sender, oneshot},
    task::JoinSet,
};
use yellowstone_grpc_proto::{
    geyser::{
        subscribe_update::UpdateOneof, SubscribeUpdate, SubscribeUpdateAccount,
        SubscribeUpdateAccountInfo,
    },
    tonic::Status,
};

/// A `Source` implementation for the Solana Accounts RPC API.
#[derive(Debug)]
pub struct SolanaAccountsRpcSource {
    filters: Filters,
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
    /// Create a new `SolanaAccountsRpcSource`.
    #[must_use]
    pub fn new(config: SolanaAccountsRpcConfig, filters: Filters) -> Self {
        Self { config, filters }
    }

    fn get_commitment_config(&self) -> CommitmentConfig {
        match self.config.commitment_level {
            Some(CommitmentLevel::Finalized) => CommitmentConfig::finalized(),
            Some(CommitmentLevel::Processed) => CommitmentConfig::processed(),
            _ => CommitmentConfig::confirmed(),
        }
    }
}

#[async_trait]
impl SourceTrait for SolanaAccountsRpcSource {
    type Config = SolanaAccountsRpcConfig;

    fn new(config: Self::Config, filters: Filters) -> Self { Self { config, filters } }

    #[allow(deprecated)] // get_program_accounts_with_config is deprecated but replacement not yet stable
    async fn connect(
        &self,
        tx: Sender<Result<SubscribeUpdate, Status>>,
        status_tx: oneshot::Sender<SourceExitStatus>,
    ) -> Result<(), ShipsternError> {
        let filters = &self.filters;
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
                                    bank_id: None,
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

        let mut exit_status = SourceExitStatus::Completed;

        // One task runs per (filter, owner) pair and the runtime treats a source
        // `Error` as fatal, so the first failure wins: keeping later errors adds
        // nothing, and updates already sent by sibling tasks stay in the buffer.
        while let Some(task_result) = tasks_set.join_next().await {
            match task_result {
                Ok(Ok(())) => {},
                Ok(Err(msg)) => {
                    tracing::error!(%msg, "Solana RPC source task failed");

                    if matches!(exit_status, SourceExitStatus::Completed) {
                        exit_status = SourceExitStatus::Error(msg);
                    }
                },
                Err(e) => {
                    tracing::error!(err = %e, "Solana RPC source task panicked or was cancelled");

                    if matches!(exit_status, SourceExitStatus::Completed) {
                        exit_status = SourceExitStatus::Error(e.to_string());
                    }
                },
            }
        }

        let _ = status_tx.send(exit_status);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use shipstern::{sources::SourceTrait, CommitmentLevel};
    use shipstern_core::{Filters, Prefilter};
    use tokio::sync::{mpsc, oneshot};

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
            let source = SolanaAccountsRpcSource::new(
                SolanaAccountsRpcConfig {
                    endpoint: "http://127.0.0.1:9".to_string(),
                    timeout: 1,
                    commitment_level: Some(CommitmentLevel::Confirmed),
                },
                filters,
            );
            let (tx, mut rx) = mpsc::channel(1);
            let (status_tx, status_rx) = oneshot::channel();

            source
                .connect(tx, status_tx)
                .await
                .expect("connect should report task failure through source status");

            let status = status_rx.await.expect("source status should be sent");
            let shipstern::sources::SourceExitStatus::Error(msg) = status else {
                panic!("expected source error, got {status:?}");
            };
            assert!(msg.contains("Failed to get slot for source: solana-rpc"));
            assert!(rx.try_recv().is_err());
        });
    }

    #[test]
    fn connect_with_multiple_filters_reports_single_error_status() {
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
            let source = SolanaAccountsRpcSource::new(
                SolanaAccountsRpcConfig {
                    endpoint: "http://127.0.0.1:9".to_string(),
                    timeout: 1,
                    commitment_level: Some(CommitmentLevel::Confirmed),
                },
                filters,
            );
            let (tx, mut rx) = mpsc::channel(1);
            let (status_tx, status_rx) = oneshot::channel();

            source
                .connect(tx, status_tx)
                .await
                .expect("connect should report task failures through source status");

            let status = status_rx
                .await
                .expect("source status should be sent exactly once");
            let shipstern::sources::SourceExitStatus::Error(msg) = status else {
                panic!("expected source error, got {status:?}");
            };
            assert!(msg.contains("Failed to get slot for source: solana-rpc"));
            assert!(rx.try_recv().is_err());
        });
    }
}
