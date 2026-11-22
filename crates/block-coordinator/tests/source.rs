//! `CoordinatorSource` against a local Geyser server: what it forwards to the
//! coordinator and the runtime, and how it stops.

use std::{
    collections::HashMap,
    pin::Pin,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex, PoisonError,
    },
    time::Duration,
};

use futures_util::{stream, Stream};
use shipstern::sources::{FromConfig, SourceContext, SourceExitStatus, SourceTrait};
use shipstern_block_coordinator::{
    AccountCommitAt, BlockMachineCoordinator, CoordinatorInput, CoordinatorMessage,
    CoordinatorSource, CoordinatorSourceConfig, InstructionRecordSortKey, InstructionSlot,
    DEFAULT_MAX_PENDING_BANKS,
};
use shipstern_core::Filters;
use shipstern_yellowstone_grpc_source::YellowstoneGrpcConfig;
use tokio::sync::mpsc;
use yellowstone_grpc_proto::{
    geyser::{geyser_server::Geyser, subscribe_update::UpdateOneof, *},
    tonic::{self, Request, Response, Status, Streaming},
};

type Responses<T> = Pin<Box<dyn Stream<Item = Result<T, Status>> + Send>>;

type Script = Vec<Result<SubscribeUpdate, Status>>;

/// Plays one script per subscription, in order, and repeats the last. A script ends its
/// stream, or fails it with the status it ends with. It records each subscribe request, so a
/// test sees what the source asked for and how often.
#[derive(Clone)]
struct Server {
    scripts: Vec<Script>,
    connections: Arc<AtomicUsize>,
    requests: Arc<Mutex<Vec<SubscribeRequest>>>,
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
        self.requests
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(request);

        let connection = self.connections.fetch_add(1, Ordering::SeqCst);
        let script = self.scripts[connection.min(self.scripts.len() - 1)].clone();
        Ok(Response::new(Box::pin(stream::iter(script))))
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
    requests: Vec<SubscribeRequest>,
    /// The geyser updates forwarded to the coordinator.
    coordinator: Vec<SubscribeUpdate>,
    /// Everything sent to the runtime.
    runtime: Vec<Result<SubscribeUpdate, Status>>,
}

/// Start a server that plays `scripts`.
async fn serve(
    scripts: Vec<Script>,
) -> (
    std::net::SocketAddr,
    Arc<Mutex<Vec<SubscribeRequest>>>,
    tokio::task::JoinHandle<()>,
) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let incoming = stream::unfold(listener, |listener| async {
        Some((listener.accept().await.map(|(socket, _)| socket), listener))
    });
    let requests = Arc::new(Mutex::new(Vec::new()));
    let service = Server {
        scripts,
        connections: Arc::default(),
        requests: Arc::clone(&requests),
    };
    let server = tokio::spawn(async move {
        tonic::transport::Server::builder()
            .add_service(geyser_server::GeyserServer::new(service))
            .serve_with_incoming(incoming)
            .await
            .unwrap();
    });
    (address, requests, server)
}

/// A config for the server at `address` that never reconnects.
fn grpc_config(address: std::net::SocketAddr) -> YellowstoneGrpcConfig {
    YellowstoneGrpcConfig {
        endpoint: format!("http://{address}"),
        x_token: None,
        timeout: 3,
        commitment_level: None,
        from_slot: None,
        accounts_data_slice: Vec::new(),
        max_decoding_message_size: None,
        accept_compression: None,
        auto_reconnect: false,
        reconnect_max_retries: None,
    }
}

/// Run a `CoordinatorSource` against a server that sends `updates` and then ends the
/// stream. `configure` adjusts the gRPC config, whose endpoint is the server.
async fn run(
    configure: impl FnOnce(&mut YellowstoneGrpcConfig),
    updates: Vec<SubscribeUpdate>,
) -> Run {
    run_scripts(configure, vec![updates.into_iter().map(Ok).collect()]).await
}

/// Like [`run`], with one script per subscription.
async fn run_scripts(
    configure: impl FnOnce(&mut YellowstoneGrpcConfig),
    scripts: Vec<Script>,
) -> Run {
    let _ = rustls::crypto::ring::default_provider().install_default();

    tokio::time::timeout(Duration::from_secs(10), async {
        let (address, requests, server) = serve(scripts).await;

        let mut grpc = grpc_config(address);
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

        let requests = requests
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();

        Run {
            exit,
            requests,
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

/// An UpdateParent is the only notice that a bank was cleared, and the coordinator
/// releases that bank's records on it. The source asks for it and forwards it as is.
#[tokio::test]
async fn entry_update_parent_reaches_the_coordinator() {
    let cleared = SubscribeUpdateEntryUpdateParent {
        slot: 42,
        cleared_bank_id: 7,
        parent_slot: 40,
        parent_block_id: vec![1; 32],
    };
    let run = run(|_| {}, vec![SubscribeUpdate {
        update_oneof: Some(UpdateOneof::EntryUpdateParent(cleared.clone())),
        ..Default::default()
    }])
    .await;

    assert!(
        matches!(run.exit, Ok(SourceExitStatus::StreamEnded)),
        "got {:?}",
        run.exit
    );

    let [request] = run.requests.as_slice() else {
        panic!("expected one subscription, got {}", run.requests.len());
    };
    assert_eq!(
        request.entry["coordinator"].include_update_parent,
        Some(true)
    );

    let forwarded: Vec<_> = run
        .coordinator
        .iter()
        .filter_map(|update| match &update.update_oneof {
            Some(UpdateOneof::EntryUpdateParent(parent)) => Some(parent.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(forwarded, [cleared]);
    assert!(run.runtime.is_empty(), "no runtime pipeline takes it");
}

// =============================================================================
// Reconnect: the source, the coordinator and a handler run together
// =============================================================================

const EPOCH_SHIFT: u32 = 48;

fn status_update(slot: u64, status: SlotStatus, bank_id: u64) -> SubscribeUpdate {
    slot_status(slot, status as i32, Some(bank_id))
}

fn entry_update(slot: u64, bank_id: u64, transactions: u64) -> SubscribeUpdate {
    SubscribeUpdate {
        update_oneof: Some(UpdateOneof::Entry(SubscribeUpdateEntry {
            slot,
            index: 0,
            num_hashes: 1,
            hash: vec![slot as u8; 32],
            executed_transaction_count: transactions,
            starting_transaction_index: 0,
            bank_id,
        })),
        ..Default::default()
    }
}

fn transaction_update(slot: u64, bank_id: u64, index: u64) -> SubscribeUpdate {
    SubscribeUpdate {
        update_oneof: Some(UpdateOneof::Transaction(SubscribeUpdateTransaction {
            transaction: Some(SubscribeUpdateTransactionInfo {
                signature: vec![index as u8; 64],
                index,
                ..Default::default()
            }),
            slot,
            bank_id,
        })),
        ..Default::default()
    }
}

fn block_meta_update(slot: u64, bank_id: u64, transactions: u64) -> SubscribeUpdate {
    let hash = |slot: u64| bs58::encode([slot as u8; 32]).into_string();
    SubscribeUpdate {
        update_oneof: Some(UpdateOneof::BlockMeta(SubscribeUpdateBlockMeta {
            slot,
            blockhash: hash(slot),
            parent_slot: slot - 1,
            parent_blockhash: hash(slot - 1),
            executed_transaction_count: transactions,
            entries_count: 1,
            bank_id,
            ..Default::default()
        })),
        ..Default::default()
    }
}

/// A bank from its first shred to its finalization, with its transactions in the middle.
fn bank(slot: u64, bank_id: u64, transactions: &[u64]) -> Script {
    let mut script = bank_start(slot, bank_id, transactions.len() as u64);
    script.extend(
        transactions
            .iter()
            .map(|&index| Ok(transaction_update(slot, bank_id, index))),
    );
    script.extend(bank_end(slot, bank_id, transactions.len() as u64));
    script
}

fn bank_start(slot: u64, bank_id: u64, transactions: u64) -> Script {
    vec![
        Ok(status_update(
            slot,
            SlotStatus::SlotFirstShredReceived,
            bank_id,
        )),
        Ok(status_update(slot, SlotStatus::SlotCreatedBank, bank_id)),
        Ok(entry_update(slot, bank_id, transactions)),
    ]
}

fn bank_end(slot: u64, bank_id: u64, transactions: u64) -> Script {
    vec![
        Ok(status_update(slot, SlotStatus::SlotCompleted, bank_id)),
        Ok(block_meta_update(slot, bank_id, transactions)),
        Ok(status_update(slot, SlotStatus::SlotConfirmed, bank_id)),
        Ok(status_update(slot, SlotStatus::SlotFinalized, bank_id)),
    ]
}

fn cut() -> Result<SubscribeUpdate, Status> { Err(Status::unavailable("connection lost")) }

/// The end of the last script: a status the client does not retry, so the source stops.
fn end_of_script() -> Result<SubscribeUpdate, Status> {
    Err(Status::failed_precondition("end of script"))
}

/// What the coordinator flushed, and whether it was still running when it did.
struct Pipeline {
    flushed: Vec<InstructionSlot<String>>,
    requests: usize,
    /// How the coordinator stopped, if it did before the run ended.
    coordinator_stopped: Option<String>,
}

/// Run the source against a server that plays `scripts`, with the real coordinator behind it and a
/// handler that parses each transaction the way kafka-sink does: a record named after the bank
/// and the transaction index, then `TransactionParsed`, both with the bank ID of the update.
/// `delay` holds a handler back by the delivery of the bank (0 before any cut-off) and the index.
async fn run_pipeline(
    scripts: Vec<Script>,
    delay: impl Fn(u16, u64) -> Duration + Send + Sync + 'static,
    wanted_slots: usize,
) -> Pipeline {
    let _ = rustls::crypto::ring::default_provider().install_default();

    tokio::time::timeout(Duration::from_secs(20), async {
        let (address, requests, server) = serve(scripts).await;

        let (input_tx, input_rx) = mpsc::channel(256);
        let (parsed_tx, parsed_rx) = mpsc::channel(256);
        let (output_tx, mut output_rx) = mpsc::channel(64);
        let coordinator = tokio::spawn(BlockMachineCoordinator::run(
            input_rx,
            parsed_rx,
            Some(output_tx),
            None,
            AccountCommitAt::Confirmed,
            true,
            DEFAULT_MAX_PENDING_BANKS,
        ));

        let source = CoordinatorSource::from_config(CoordinatorSourceConfig {
            source: YellowstoneGrpcConfig {
                auto_reconnect: true,
                reconnect_max_retries: Some(3),
                ..grpc_config(address)
            },
            source_label: None,
            stream_idle_warn_secs: 0,
            coordinator_input_tx: Some(input_tx),
            fixture_path: None,
            fixture_slots: None,
        });
        let (tx, mut rx) = mpsc::channel(64);
        let source = tokio::spawn(async move {
            source
                .connect(SourceContext::new(Filters::new(HashMap::new()), tx))
                .await
        });

        let delay = Arc::new(delay);
        let handler = tokio::spawn(async move {
            while let Some(update) = rx.recv().await {
                let Ok(SubscribeUpdate {
                    update_oneof: Some(UpdateOneof::Transaction(transaction)),
                    ..
                }) = update
                else {
                    continue;
                };
                let (parsed_tx, delay) = (parsed_tx.clone(), Arc::clone(&delay));
                tokio::spawn(async move {
                    let index = transaction.transaction.map_or(0, |info| info.index);
                    let epoch = (transaction.bank_id >> EPOCH_SHIFT) as u16;
                    tokio::time::sleep(delay(epoch, index)).await;

                    let (slot, bank_id) = (transaction.slot, transaction.bank_id);
                    let bank = bank_id & ((1 << EPOCH_SHIFT) - 1);
                    let _ = parsed_tx
                        .send(CoordinatorMessage::InstructionParsed {
                            slot,
                            bank_id,
                            key: InstructionRecordSortKey::new(index, vec![0]),
                            record: format!("b{bank}:tx{index}"),
                        })
                        .await;
                    let _ = parsed_tx
                        .send(CoordinatorMessage::TransactionParsed { slot, bank_id })
                        .await;
                });
            }
        });

        let mut flushed = Vec::new();
        while flushed.len() < wanted_slots {
            let Some(slot) = output_rx.recv().await else {
                break;
            };
            flushed.push(slot);
        }

        // It also ends with Ok once the source and the handler are done, which is no failure.
        let coordinator_stopped = if coordinator.is_finished() {
            match coordinator.await {
                Ok(Ok(())) => None,
                other => Some(format!("{other:?}")),
            }
        } else {
            coordinator.abort();
            None
        };
        source.abort();
        handler.abort();
        server.abort();
        let requests = requests
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len();

        Pipeline {
            flushed,
            requests,
            coordinator_stopped,
        }
    })
    .await
    .expect("the pipeline run timed out")
}

fn records(slot: &InstructionSlot<String>) -> Vec<&str> {
    slot.records.iter().map(String::as_str).collect()
}

/// The reconnect cuts slot 100 after two of its three transactions. The server replays all
/// three. The flush holds each transaction once, and it waits for the last one: the replay of the
/// first two must not count toward the gate on top of the two the handlers already parsed.
#[tokio::test]
async fn reconnect_mid_slot_flushes_each_record_once() {
    let mut first = bank_start(100, 7, 3);
    first.extend([0, 1].map(|index| Ok(transaction_update(100, 7, index))));
    first.push(cut());

    let mut second = bank(100, 7, &[0, 1, 2]);
    second.push(end_of_script());

    let run = run_pipeline(
        vec![first, second],
        |_, index| Duration::from_millis(if index == 2 { 600 } else { 0 }),
        1,
    )
    .await;

    assert_eq!(run.requests, 2, "the source must reconnect once");
    assert_eq!(run.coordinator_stopped, None);
    let [slot] = run.flushed.as_slice() else {
        panic!("expected one flushed slot, got {}", run.flushed.len());
    };
    assert_eq!((slot.slot, slot.bank_id), (100, 7));
    assert_eq!(slot.executed_transaction_count, 3);
    assert_eq!(records(slot), ["b7:tx0", "b7:tx1", "b7:tx2"]);
}

/// A handler can still be parsing a transaction of the cut-off delivery when the replay begins.
/// Its transaction signal must not count toward the replay either, or the gate opens before the
/// replay's last transaction is parsed.
#[tokio::test]
async fn late_record_of_the_cut_off_delivery_is_rejected() {
    let mut first = bank_start(100, 7, 3);
    first.extend([0, 1].map(|index| Ok(transaction_update(100, 7, index))));
    first.push(cut());

    let mut second = bank(100, 7, &[0, 1, 2]);
    second.push(end_of_script());

    let run = run_pipeline(
        vec![first, second],
        |epoch, index| match (epoch, index) {
            (0, 1) => Duration::from_millis(300),
            (_, 2) => Duration::from_millis(900),
            _ => Duration::ZERO,
        },
        1,
    )
    .await;

    assert_eq!(run.coordinator_stopped, None);
    let [slot] = run.flushed.as_slice() else {
        panic!("expected one flushed slot, got {}", run.flushed.len());
    };
    assert_eq!(records(slot), ["b7:tx0", "b7:tx1", "b7:tx2"]);
}

/// The replay comes from another node, which numbers its banks differently. The cut-off bank
/// never confirms. It must not flush, hold the next slot back or stop the coordinator, and a
/// bank is pending only while it waits.
#[tokio::test]
async fn reconnect_to_new_bank_ids_flushes_the_replacement() {
    let mut first = bank_start(100, 7, 2);
    first.push(Ok(transaction_update(100, 7, 0)));
    first.push(cut());

    let mut second = bank(100, 9000, &[0, 1]);
    second.extend(bank(101, 9001, &[0]));
    second.push(end_of_script());

    let run = run_pipeline(vec![first, second], |_, _| Duration::ZERO, 2).await;

    assert_eq!(run.coordinator_stopped, None);
    let [first, second] = run.flushed.as_slice() else {
        panic!("expected two flushed slots, got {}", run.flushed.len());
    };
    assert_eq!((first.slot, first.bank_id), (100, 9000));
    assert_eq!(records(first), ["b9000:tx0", "b9000:tx1"]);
    assert_eq!((second.slot, second.bank_id), (101, 9001));
    assert_eq!(records(second), ["b9001:tx0"]);
}

/// The source passes the client's retry budget on, and it does not reconnect when the config
/// turns that off.
#[tokio::test]
async fn reconnect_follows_the_config() {
    for (auto_reconnect, max_retries, from_slot, expect) in [
        (true, None, None, true),
        (false, None, None, false),
        (true, Some(0), None, false),
        (true, None, Some(5), false),
    ] {
        let mut first = bank_start(100, 7, 1);
        first.push(cut());
        let mut second = bank(100, 7, &[0]);
        second.push(end_of_script());

        let run = run_scripts(
            |grpc| {
                grpc.auto_reconnect = auto_reconnect;
                grpc.reconnect_max_retries = max_retries;
                grpc.from_slot = from_slot;
            },
            vec![first, second],
        )
        .await;

        let reconnected = run.requests.len() > 1;
        assert_eq!(
            reconnected, expect,
            "auto_reconnect {auto_reconnect}, retries {max_retries:?}, from_slot {from_slot:?}, \
             exit {:?}",
            run.exit
        );
    }
}
