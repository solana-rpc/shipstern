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

/// Why one fetch task stopped the source.
enum TaskStop {
    ReceiverDropped,
    Failed(String),
}

impl From<String> for TaskStop {
    fn from(msg: String) -> Self { Self::Failed(msg) }
}

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
                                return Err(TaskStop::Failed(format!(
                                    "Failed to decode account data for {acc_pubkey} from source: \
                                     solana-rpc, filter: {filter_id}"
                                )));
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

                            if tx.send(Ok(update)).await.is_err() {
                                return Err(TaskStop::ReceiverDropped);
                            }
                        }

                        Ok::<(), TaskStop>(())
                    });
                }
            }
        }

        // One task runs per (filter, owner) pair and the runtime treats a source
        // `Err` as fatal, so the first stop wins and aborts the siblings. Updates
        // already sent stay in the buffer.
        let mut stop: Option<TaskStop> = None;

        while let Some(task_result) = tasks_set.join_next().await {
            if stop.is_some() {
                continue;
            }

            let reason = match task_result {
                Ok(Ok(())) => continue,
                Ok(Err(TaskStop::ReceiverDropped)) => TaskStop::ReceiverDropped,
                Ok(Err(TaskStop::Failed(msg))) => {
                    tracing::error!(%msg, "Solana RPC source task failed");
                    TaskStop::Failed(msg)
                },
                Err(e) => {
                    tracing::error!(err = %e, "Solana RPC source task panicked or was cancelled");
                    TaskStop::Failed(e.to_string())
                },
            };

            tasks_set.abort_all();
            stop = Some(reason);
        }

        match stop {
            Some(TaskStop::ReceiverDropped) => Ok(SourceExitStatus::ReceiverDropped),
            Some(TaskStop::Failed(msg)) => Err(ShipsternError::Other(msg.into())),
            None => Ok(SourceExitStatus::Completed),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, time::Duration};

    use shipstern::{
        sources::{FromConfig, SourceContext, SourceExitStatus, SourceTrait},
        CommitmentLevel,
    };
    use shipstern_core::{Filters, Prefilter};
    use solana_pubkey::Pubkey;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::{TcpListener, TcpStream},
        sync::mpsc,
    };

    use super::{SolanaAccountsRpcConfig, SolanaAccountsRpcSource};

    const OWNER_A: [u8; 32] = [1; 32];
    const OWNER_B: [u8; 32] = [2; 32];
    const CONTEXT_SLOT: u64 = 777;

    async fn read_request_body(stream: &mut TcpStream) -> String {
        let mut buf = Vec::new();
        let mut chunk = [0_u8; 1024];

        loop {
            let read = stream.read(&mut chunk).await.expect("read the request");
            assert_ne!(read, 0, "the client closed before the body");
            buf.extend_from_slice(&chunk[..read]);

            let text = String::from_utf8_lossy(&buf).into_owned();
            let Some((headers, body)) = text.split_once("\r\n\r\n") else {
                continue;
            };
            let length = headers
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .map(|value| value.trim().parse::<usize>().expect("content length"))
                })
                .expect("the request has a content length");

            if body.len() >= length {
                return body.to_string();
            }
        }
    }

    /// Serves JSON-RPC on localhost. `respond` takes the request body and returns the
    /// response body, or `None` to leave the request unanswered.
    async fn serve(respond: fn(&str) -> Option<String>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let endpoint = format!("http://{}", listener.local_addr().expect("local addr"));

        tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.expect("accept");

                tokio::spawn(async move {
                    let body = read_request_body(&mut stream).await;
                    let Some(response) = respond(&body) else {
                        std::future::pending::<()>().await;
                        return;
                    };
                    let reply = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: \
                         {}\r\nConnection: close\r\n\r\n{response}",
                        response.len()
                    );

                    stream.write_all(reply.as_bytes()).await.expect("write");
                });
            }
        });

        endpoint
    }

    fn program_accounts_reply() -> String {
        let account = Pubkey::new_from_array([3; 32]);
        let owner = Pubkey::new_from_array(OWNER_A);

        format!(
            r#"{{"jsonrpc":"2.0","id":1,"result":{{"context":{{"slot":{CONTEXT_SLOT}}},"value":[{{"pubkey":"{account}","account":{{"lamports":5,"data":["aGk=","base64"],"owner":"{owner}","executable":false,"rentEpoch":0,"space":2}}}}]}}}}"#
        )
    }

    fn answer_one_account(body: &str) -> Option<String> {
        if body.contains("getSlot") {
            return Some(r#"{"jsonrpc":"2.0","id":1,"result":5}"#.to_string());
        }

        Some(program_accounts_reply())
    }

    fn filters_for(owners: &[[u8; 32]]) -> Filters {
        Filters::new(
            owners
                .iter()
                .enumerate()
                .map(|(i, owner)| {
                    (
                        format!("filter-{i}"),
                        Prefilter::builder()
                            .account_owners([*owner])
                            .build()
                            .expect("account owner filter should build"),
                    )
                })
                .collect::<HashMap<_, _>>(),
        )
    }

    fn source_at(endpoint: String, timeout: u64) -> SolanaAccountsRpcSource {
        SolanaAccountsRpcSource::from_config(SolanaAccountsRpcConfig {
            endpoint,
            timeout,
            commitment_level: Some(CommitmentLevel::Confirmed),
        })
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

    #[tokio::test]
    async fn a_closed_receiver_is_receiver_dropped() {
        let endpoint = serve(answer_one_account).await;
        let (tx, rx) = mpsc::channel(4);
        drop(rx);

        let result = source_at(endpoint, 5)
            .connect(SourceContext::new(filters_for(&[OWNER_A]), tx))
            .await;

        assert!(
            matches!(result, Ok(SourceExitStatus::ReceiverDropped)),
            "got {result:?}"
        );
    }

    /// Owner A fails at once while owner B's request never gets an answer, so the run ends
    /// in time only if the first failure aborts B.
    #[tokio::test]
    async fn the_first_failure_aborts_the_other_owners() {
        fn fail_a_hang_b(body: &str) -> Option<String> {
            if body.contains("getSlot") {
                return Some(r#"{"jsonrpc":"2.0","id":1,"result":5}"#.to_string());
            }
            if body.contains(&Pubkey::new_from_array(OWNER_A).to_string()) {
                return Some(
                    r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32000,"message":"boom"}}"#
                        .to_string(),
                );
            }

            None
        }

        let endpoint = serve(fail_a_hang_b).await;
        let (tx, _rx) = mpsc::channel(4);
        let source = source_at(endpoint, 60);

        let result = tokio::time::timeout(
            Duration::from_secs(10),
            source.connect(SourceContext::new(filters_for(&[OWNER_A, OWNER_B]), tx)),
        )
        .await
        .expect("the failing owner aborts the one that waits on the server");

        let Err(shipstern::Error::Other(err)) = result else {
            panic!("expected a connect error, got {result:?}");
        };
        assert!(err.to_string().contains("boom"), "got {err}");
    }
}
