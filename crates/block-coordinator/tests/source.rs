//! `CoordinatorSource` against a local Geyser server: what it forwards to the
//! coordinator and the runtime, and how it stops.

use std::{collections::HashMap, pin::Pin, time::Duration};

use futures_util::{stream, Stream};
use shipstern::sources::{FromConfig, SourceContext, SourceExitStatus, SourceTrait};
use shipstern_block_coordinator::{CoordinatorInput, CoordinatorSource, CoordinatorSourceConfig};
use shipstern_core::Filters;
use shipstern_yellowstone_grpc_source::YellowstoneGrpcConfig;
use tokio::sync::mpsc;
use yellowstone_grpc_proto::{
    geyser::{geyser_server::Geyser, subscribe_update::UpdateOneof, *},
    tonic::{self, Request, Response, Status, Streaming},
};

type Responses<T> = Pin<Box<dyn Stream<Item = Result<T, Status>> + Send>>;

/// Sends `updates` on every subscription and then ends the stream.
#[derive(Clone)]
struct Server {
    updates: Vec<SubscribeUpdate>,
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
        request.into_inner().message().await?.unwrap();

        let updates = self.updates.clone().into_iter().map(Ok);
        Ok(Response::new(Box::pin(stream::iter(updates))))
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

/// What one run of the source did.
struct Run {
    exit: Result<SourceExitStatus, shipstern::Error>,
    /// The geyser updates forwarded to the coordinator.
    coordinator: Vec<SubscribeUpdate>,
    /// Everything sent to the runtime.
    runtime: Vec<Result<SubscribeUpdate, Status>>,
}

/// Run a `CoordinatorSource` against a server that sends `updates` and then ends the
/// stream. `configure` adjusts the gRPC config, whose endpoint is the server.
async fn run(
    configure: impl FnOnce(&mut YellowstoneGrpcConfig),
    updates: Vec<SubscribeUpdate>,
) -> Run {
    let _ = rustls::crypto::ring::default_provider().install_default();

    tokio::time::timeout(Duration::from_secs(10), async {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let incoming = stream::unfold(listener, |listener| async {
            Some((listener.accept().await.map(|(socket, _)| socket), listener))
        });
        let service = Server { updates };
        let server = tokio::spawn(async move {
            tonic::transport::Server::builder()
                .add_service(geyser_server::GeyserServer::new(service))
                .serve_with_incoming(incoming)
                .await
                .unwrap();
        });

        let mut grpc = YellowstoneGrpcConfig {
            endpoint: format!("http://{address}"),
            x_token: None,
            timeout: 3,
            commitment_level: None,
            from_slot: None,
            accounts_data_slice: Vec::new(),
            max_decoding_message_size: None,
            accept_compression: None,
        };
        configure(&mut grpc);

        let (coordinator_tx, mut coordinator_rx) = mpsc::channel(64);
        let source = CoordinatorSource::from_config(CoordinatorSourceConfig {
            source: grpc,
            source_label: None,
            stream_idle_warn_secs: 0,
            coordinator_input_tx: Some(coordinator_tx),
            fixture_path: None,
            fixture_slots: None,
        });
        let (tx, mut rx) = mpsc::channel(64);

        let exit = source
            .connect(SourceContext::new(Filters::new(HashMap::new()), tx))
            .await;

        // The source holds the coordinator sender, so the channel closes with it.
        drop(source);
        server.abort();

        let mut coordinator = Vec::new();
        while let Some(input) = coordinator_rx.recv().await {
            if let CoordinatorInput::GeyserUpdate(update) = input {
                coordinator.push(*update);
            }
        }

        let mut runtime = Vec::new();
        while let Some(update) = rx.recv().await {
            runtime.push(update);
        }

        Run {
            exit,
            coordinator,
            runtime,
        }
    })
    .await
    .expect("the source run timed out")
}

fn slot_status(slot: u64, status: i32, bank_id: Option<u64>) -> SubscribeUpdate {
    SubscribeUpdate {
        update_oneof: Some(UpdateOneof::Slot(SubscribeUpdateSlot {
            slot,
            parent: Some(slot - 1),
            status,
            bank_id,
            ..Default::default()
        })),
        ..Default::default()
    }
}

/// The slot and status of each slot update forwarded to the coordinator.
fn forwarded_statuses(run: &Run) -> Vec<(u64, i32)> {
    run.coordinator
        .iter()
        .filter_map(|update| match &update.update_oneof {
            Some(UpdateOneof::Slot(slot)) => Some((slot.slot, slot.status)),
            _ => None,
        })
        .collect()
}

/// A status this proto does not know, from a newer server, is skipped. It must not stop
/// the source as a status with no bank ID, and it must not reach the block machine, which
/// would read it as processed.
#[tokio::test]
async fn unknown_slot_status_is_skipped() {
    const UNKNOWN: i32 = 99;
    assert!(SlotStatus::try_from(UNKNOWN).is_err());

    let created = SlotStatus::SlotCreatedBank as i32;
    let run = run(|_| {}, vec![
        slot_status(42, UNKNOWN, None),
        slot_status(42, UNKNOWN, Some(7)),
        slot_status(42, created, Some(7)),
    ])
    .await;

    assert!(
        matches!(run.exit, Ok(SourceExitStatus::StreamEnded)),
        "the source must run to the end of the stream, got {:?}",
        run.exit
    );
    assert_eq!(forwarded_statuses(&run), [(42, created)]);
    assert!(run.runtime.is_empty());
}
