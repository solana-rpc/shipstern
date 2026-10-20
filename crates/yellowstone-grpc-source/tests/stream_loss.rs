//! A lost stream ends the run at every commitment, after one subscription, with the
//! records delivered before the loss intact.

use std::{
    collections::{HashMap, HashSet},
    pin::Pin,
    sync::{Arc, Mutex, PoisonError},
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

/// Sends one account on every subscription, then loses the stream. It records the
/// commitment each subscription asked for, so a test sees how many there were too.
#[derive(Clone)]
struct Server {
    commitments: Arc<Mutex<Vec<Option<i32>>>>,
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
        let request = request.into_inner().message().await?.unwrap();
        self.commitments
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(request.commitment);

        let (tx, rx) = mpsc::channel(4);
        let loss = self.loss;
        tokio::spawn(async move {
            tx.send(Ok(account(1))).await.unwrap();
            if matches!(loss, Loss::Status) {
                let _ = tx
                    .send(Err(Status::unavailable("node is restarting")))
                    .await;
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

/// Run the source against a server that loses the stream after one account, and check
/// that the run ends after one subscription with that account delivered once.
async fn assert_loss_ends_the_run(commitment: Option<CommitmentLevel>, loss: Loss) {
    let _ = rustls::crypto::ring::default_provider().install_default();
    tokio::time::timeout(Duration::from_secs(10), async {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let incoming = stream::unfold(listener, |listener| async {
            Some((listener.accept().await.map(|(socket, _)| socket), listener))
        });
        let commitments = Arc::new(Mutex::new(Vec::new()));
        let service = Server {
            commitments: Arc::clone(&commitments),
            loss,
        };
        let server = tokio::spawn(async move {
            tonic::transport::Server::builder()
                .add_service(geyser::geyser_server::GeyserServer::new(service))
                .serve_with_incoming(incoming)
                .await
                .unwrap();
        });
        let commitment_key = commitment
            .map(|level| {
                format!(
                    "commitment-level = \"{}\"",
                    level.as_str_name().to_lowercase()
                )
            })
            .unwrap_or_default();
        let config: YellowstoneGrpcConfig = toml::from_str(&format!(
            "endpoint = \"http://{address}\"\ntimeout = 3\n{commitment_key}"
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

        let commitments = commitments
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        assert_eq!(
            (commitments, keys),
            (vec![commitment.map(|level| level as i32)], vec![1]),
            "{commitment:?} {loss:?}: one subscription, and the delivered record once"
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
    .expect("stream loss timed out");
}

#[tokio::test]
async fn default_commitment_stops_on_loss() {
    for loss in [Loss::Status, Loss::Eof] {
        assert_loss_ends_the_run(None, loss).await;
    }
}

#[tokio::test]
async fn processed_stream_stops_on_loss() {
    for loss in [Loss::Status, Loss::Eof] {
        assert_loss_ends_the_run(Some(CommitmentLevel::Processed), loss).await;
    }
}

#[tokio::test]
async fn confirmed_stream_stops_on_loss() {
    for loss in [Loss::Status, Loss::Eof] {
        assert_loss_ends_the_run(Some(CommitmentLevel::Confirmed), loss).await;
    }
}

#[tokio::test]
async fn finalized_stream_stops_on_loss() {
    for loss in [Loss::Status, Loss::Eof] {
        assert_loss_ends_the_run(Some(CommitmentLevel::Finalized), loss).await;
    }
}
