use std::{
    collections::{HashMap, HashSet},
    pin::Pin,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

use futures_util::{stream, Stream};
use shipstern::sources::{FromConfig, SourceContext, SourceExitStatus, SourceTrait};
use shipstern_core::{AccountPrefilter, Filters, Prefilter, Pubkey};
use shipstern_yellowstone_grpc_source::{YellowstoneGrpcConfig, YellowstoneGrpcSource};
use tokio::sync::mpsc;
use yellowstone_grpc_proto::{
    geyser::{self, geyser_server::Geyser, subscribe_update::UpdateOneof, *},
    tonic::{self, Request, Response, Status, Streaming},
};

type Responses<T> = Pin<Box<dyn Stream<Item = Result<T, Status>> + Send>>;

#[derive(Clone, Copy, Debug)]
enum Loss {
    Status,
    Eof,
}

#[derive(Clone)]
struct Server {
    connections: Arc<AtomicUsize>,
    loss: Loss,
}

fn account(key: u8) -> SubscribeUpdate {
    SubscribeUpdate {
        filters: vec!["accounts".into()],
        update_oneof: Some(UpdateOneof::Account(SubscribeUpdateAccount {
            slot: 42,
            bank_id: Some(7),
            account: Some(SubscribeUpdateAccountInfo {
                pubkey: vec![key; 32],
                owner: vec![0; 32],
                lamports: 1,
                data: vec![key],
                ..Default::default()
            }),
            ..Default::default()
        })),
        ..Default::default()
    }
}

#[tonic::async_trait]
impl Geyser for Server {
    type SubscribeDeshredStream = Responses<SubscribeUpdateDeshred>;
    type SubscribeGossipStream = Responses<SubscribeUpdateGossip>;
    type SubscribeStream = Responses<SubscribeUpdate>;

    async fn subscribe(
        &self,
        request: Request<Streaming<SubscribeRequest>>,
    ) -> Result<Response<Self::SubscribeStream>, Status> {
        let connection = self.connections.fetch_add(1, Ordering::SeqCst);
        let request = request.into_inner().message().await?.unwrap();
        assert_eq!(
            request.commitment.unwrap_or_default(),
            CommitmentLevel::Processed as i32
        );
        let (tx, rx) = mpsc::channel(4);
        let loss = self.loss;
        tokio::spawn(async move {
            if connection == 0 {
                tx.send(Ok(account(1))).await.unwrap();
                if matches!(loss, Loss::Status) {
                    let _ = tx
                        .send(Err(Status::unavailable("reconstruction has not caught up")))
                        .await;
                }
            } else {
                // Reconstruction had not seen bank 7 when replay selected its held banks.
                // The replacement only sees the later live account, not account 1.
                let _ = tx.send(Ok(account(2))).await;
                let _ = tx
                    .send(Ok(SubscribeUpdate {
                        filters: vec!["__autoreconnect".into()],
                        update_oneof: Some(UpdateOneof::BlockMeta(SubscribeUpdateBlockMeta {
                            slot: 42,
                            parent_slot: 41,
                            bank_id: 7,
                            blockhash: "3qbR1eZRqXUWroWKKYhbDmR3FfqTHfqSU8zZSxtANzYh".into(),
                            parent_blockhash: "3ntrLnnU8tH1YMDro3SNgqbcTf5C7inbKpFbv7SN8wTp".into(),
                            ..Default::default()
                        })),
                        ..Default::default()
                    }))
                    .await;
                let _ = tx
                    .send(Ok(SubscribeUpdate {
                        filters: vec!["__autoreconnect".into()],
                        update_oneof: Some(UpdateOneof::Slot(SubscribeUpdateSlot {
                            slot: 42,
                            parent: Some(41),
                            bank_id: Some(7),
                            status: SlotStatus::SlotFinalized as i32,
                            ..Default::default()
                        })),
                        ..Default::default()
                    }))
                    .await;
                let _ = tx.send(Err(Status::invalid_argument("replacement"))).await;
            }
        });
        Ok(Response::new(Box::pin(stream::unfold(
            rx,
            |mut rx| async { rx.recv().await.map(|update| (update, rx)) },
        ))))
    }

    async fn subscribe_deshred(
        &self,
        _: Request<Streaming<SubscribeDeshredRequest>>,
    ) -> Result<Response<Self::SubscribeDeshredStream>, Status> {
        Err(Status::unimplemented("unused"))
    }

    async fn subscribe_gossip(
        &self,
        _: Request<SubscribeGossipRequest>,
    ) -> Result<Response<Self::SubscribeGossipStream>, Status> {
        Err(Status::unimplemented("unused"))
    }

    async fn subscribe_replay_info(
        &self,
        _: Request<SubscribeReplayInfoRequest>,
    ) -> Result<Response<SubscribeReplayInfoResponse>, Status> {
        Err(Status::unimplemented("unused"))
    }

    async fn ping(&self, _: Request<PingRequest>) -> Result<Response<PongResponse>, Status> {
        Err(Status::unimplemented("unused"))
    }

    async fn get_latest_blockhash(
        &self,
        _: Request<GetLatestBlockhashRequest>,
    ) -> Result<Response<GetLatestBlockhashResponse>, Status> {
        Err(Status::unimplemented("unused"))
    }

    async fn get_block_height(
        &self,
        _: Request<GetBlockHeightRequest>,
    ) -> Result<Response<GetBlockHeightResponse>, Status> {
        Err(Status::unimplemented("unused"))
    }

    async fn get_slot(
        &self,
        _: Request<GetSlotRequest>,
    ) -> Result<Response<GetSlotResponse>, Status> {
        Err(Status::unimplemented("unused"))
    }

    async fn is_blockhash_valid(
        &self,
        _: Request<IsBlockhashValidRequest>,
    ) -> Result<Response<IsBlockhashValidResponse>, Status> {
        Err(Status::unimplemented("unused"))
    }

    async fn get_version(
        &self,
        _: Request<GetVersionRequest>,
    ) -> Result<Response<GetVersionResponse>, Status> {
        Err(Status::unimplemented("unused"))
    }
}

async fn assert_processed_loss_preserves_delivered_records(loss: Loss) {
    let _ = rustls::crypto::ring::default_provider().install_default();
    tokio::time::timeout(Duration::from_secs(10), async {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let incoming = stream::unfold(listener, |listener| async {
            Some((listener.accept().await.map(|(socket, _)| socket), listener))
        });
        let connections = Arc::new(AtomicUsize::new(0));
        let service = Server {
            connections: Arc::clone(&connections),
            loss,
        };
        let server = tokio::spawn(async move {
            tonic::transport::Server::builder()
                .add_service(geyser::geyser_server::GeyserServer::new(service))
                .serve_with_incoming(incoming)
                .await
                .unwrap();
        });
        let config: YellowstoneGrpcConfig = toml::from_str(&format!(
            "endpoint = \"http://{address}\"\ntimeout = 3\ncommitment-level = \
             \"processed\"\nreconnect-max-retries = 2"
        ))
        .unwrap();
        let filters = Filters::new(HashMap::from([("accounts".into(), Prefilter {
            account: Some(AccountPrefilter {
                owners: HashSet::from([Pubkey::new([0; 32])]),
                ..Default::default()
            }),
            ..Default::default()
        })]));
        let (raw_tx, mut raw_rx) = mpsc::channel(8);
        let context = SourceContext::new(filters, raw_tx);
        let source = YellowstoneGrpcSource::from_config(config);
        let task = tokio::spawn(async move { source.connect(context).await });
        let mut keys = Vec::new();
        let mut errors = Vec::new();
        while let Some(update) = raw_rx.recv().await {
            match update {
                Ok(update) => {
                    if let Some(UpdateOneof::Account(account)) = update.update_oneof {
                        keys.push(account.account.unwrap().pubkey[0]);
                    }
                },
                Err(status) => errors.push(status.code()),
            }
        }
        let result = task.await.unwrap();
        server.abort();
        assert_eq!(
            (connections.load(Ordering::SeqCst), keys),
            (1, vec![1]),
            "{loss:?}: preserve the original record without invalidating or replacing its bank"
        );
        match loss {
            Loss::Status => {
                assert_eq!(errors, [tonic::Code::Unavailable]);
                assert!(matches!(result.unwrap(), SourceExitStatus::StreamError {
                    code: tonic::Code::Unavailable,
                    ..
                }));
            },
            Loss::Eof => {
                assert!(errors.is_empty());
                assert!(matches!(result.unwrap(), SourceExitStatus::StreamEnded));
            },
        }
    })
    .await
    .expect("processed stream loss timed out");
}

#[tokio::test]
async fn processed_stream_error_preserves_delivered_accounts_without_reconnecting() {
    assert_processed_loss_preserves_delivered_records(Loss::Status).await;
}

#[tokio::test]
async fn processed_eof_preserves_delivered_accounts_without_reconnecting() {
    assert_processed_loss_preserves_delivered_records(Loss::Eof).await;
}
