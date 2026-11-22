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
    rpc_request::RpcRequest,
    rpc_response::{OptionalContext, RpcKeyedAccount},
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

#[async_trait]
impl SourceTrait for SolanaAccountsRpcSource {
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
                        // `get_program_ui_accounts_with_config` drops the response context,
                        // so the request goes through `send` to keep the slot the accounts
                        // were read at.
                        let account_config = RpcAccountInfoConfig {
                            encoding: Some(UiAccountEncoding::Base64),
                            data_slice: None,
                            commitment: Some(client.commitment()),
                            min_context_slot: None,
                        };
                        let params =
                            serde_json::json!([program_id.to_string(), RpcProgramAccountsConfig {
                                filters: None,
                                account_config,
                                with_context: Some(true),
                                sort_results: Some(false),
                            }]);
                        let response = client
                            .send::<OptionalContext<Vec<RpcKeyedAccount>>>(
                                RpcRequest::GetProgramAccounts,
                                params,
                            )
                            .await
                            .map_err(|e| {
                                format!(
                                    "Failed to get program accounts for source: solana-rpc, \
                                     filter: {filter_id}: {e}"
                                )
                            })?;
                        let OptionalContext::Context(response) = response else {
                            return Err(format!(
                                "The RPC server sent no context slot for source: solana-rpc, \
                                 filter: {filter_id}"
                            ));
                        };
                        let slot = response.context.slot;

                        for RpcKeyedAccount {
                            pubkey: acc_pubkey,
                            account,
                        } in response.value
                        {
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
                            let acc_pubkey = Pubkey::from_str(&acc_pubkey).map_err(|e| {
                                format!(
                                    "Failed to parse pubkey {acc_pubkey} for source: solana-rpc, \
                                     filter: {filter_id}: {e}"
                                )
                            })?;

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
    use yellowstone_grpc_proto::geyser::subscribe_update::UpdateOneof;

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

    /// The slot `getSlot` answers is far from `CONTEXT_SLOT`, so a stamp from it shows.
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
            let source = source_at("http://127.0.0.1:9".to_string(), 1);
            let (tx, mut rx) = mpsc::channel(1);

            let result = source
                .connect(SourceContext::new(filters_for(&[OWNER_A]), tx))
                .await;

            let Err(shipstern::Error::Other(err)) = result else {
                panic!("expected a connect error, got {result:?}");
            };
            assert!(err
                .to_string()
                .contains("Failed to get program accounts for source: solana-rpc"));
            assert!(rx.try_recv().is_err());
        });
    }

    #[test]
    fn connect_with_multiple_filters_reports_one_error() {
        let runtime = tokio::runtime::Runtime::new().expect("runtime should build");

        runtime.block_on(async {
            let source = source_at("http://127.0.0.1:9".to_string(), 1);
            let (tx, mut rx) = mpsc::channel(1);

            let result = source
                .connect(SourceContext::new(filters_for(&[OWNER_A, OWNER_B]), tx))
                .await;

            let Err(shipstern::Error::Other(err)) = result else {
                panic!("expected a connect error, got {result:?}");
            };
            assert!(err
                .to_string()
                .contains("Failed to get program accounts for source: solana-rpc"));
            assert!(rx.try_recv().is_err());
        });
    }

    #[tokio::test]
    async fn accounts_carry_the_slot_of_the_response_context() {
        let endpoint = serve(answer_one_account).await;
        let (tx, mut rx) = mpsc::channel(4);

        let result = source_at(endpoint, 5)
            .connect(SourceContext::new(filters_for(&[OWNER_A]), tx))
            .await;

        assert!(
            matches!(result, Ok(SourceExitStatus::Completed)),
            "got {result:?}"
        );
        let update = rx
            .try_recv()
            .expect("one account update")
            .expect("no status");
        let Some(UpdateOneof::Account(account)) = update.update_oneof else {
            panic!("expected an account update, got {:?}", update.update_oneof);
        };
        assert_eq!(account.slot, CONTEXT_SLOT);
        assert_eq!(account.account.expect("account info").data, b"hi");
    }
}
