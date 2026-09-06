use std::{
    borrow::Cow,
    collections::{HashMap, HashSet},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Mutex, PoisonError,
    },
    time::Duration,
};

use async_trait::async_trait;
use shipstern_core::{
    instruction::InstructionUpdate, AccountPrefilter, Filters, ParseResult, Parser, Prefilter,
    Pubkey, SlotUpdate,
};
use tokio::sync::{mpsc::Sender, oneshot, watch};
use yellowstone_grpc_proto::{
    geyser::{subscribe_update::UpdateOneof, SlotStatus, SubscribeUpdate, SubscribeUpdateSlot},
    tonic,
};

use crate::{
    config::BufferConfig,
    instruction::InstructionPipeline,
    sources::{FilterUpdateSource, SourceExitStatus, SourceTrait},
    Error, FilterUpdateError, Handler, HandlerResult, Pipeline, Runtime,
};

async fn wait_for_runtime_ready() { tokio::time::sleep(Duration::from_millis(50)).await; }

async fn hold_channel_open_briefly() { tokio::time::sleep(Duration::from_millis(10)).await; }

fn signal_stream_ended(status_tx: oneshot::Sender<SourceExitStatus>) {
    let _ = status_tx.send(SourceExitStatus::StreamEnded);
}

fn signal_stream_error(
    status_tx: oneshot::Sender<SourceExitStatus>,
    code: tonic::Code,
    message: &str,
) {
    let _ = status_tx.send(SourceExitStatus::StreamError {
        code,
        message: message.to_string(),
    });
}

fn signal_error(status_tx: oneshot::Sender<SourceExitStatus>, message: &str) {
    let _ = status_tx.send(SourceExitStatus::Error(message.to_string()));
}

fn signal_receiver_dropped(status_tx: oneshot::Sender<SourceExitStatus>) {
    let _ = status_tx.send(SourceExitStatus::ReceiverDropped);
}

fn signal_completed(status_tx: oneshot::Sender<SourceExitStatus>) {
    let _ = status_tx.send(SourceExitStatus::Completed);
}

fn make_ping_update() -> SubscribeUpdate {
    SubscribeUpdate {
        filters: vec![],
        update_oneof: Some(
            yellowstone_grpc_proto::geyser::subscribe_update::UpdateOneof::Ping(
                yellowstone_grpc_proto::geyser::SubscribeUpdatePing {},
            ),
        ),
        created_at: None,
    }
}

const TEST_SLOT_FILTER: &str = "test::SlowSlotParser";
static SLOW_SLOT_HANDLED: AtomicUsize = AtomicUsize::new(0);

fn make_slot_update(slot: u64) -> SubscribeUpdate {
    SubscribeUpdate {
        filters: vec![TEST_SLOT_FILTER.to_string()],
        update_oneof: Some(UpdateOneof::Slot(SubscribeUpdateSlot {
            slot,
            parent: Some(slot.saturating_sub(1)),
            status: SlotStatus::SlotProcessed as i32,
            dead_error: None,
        })),
        created_at: None,
    }
}

fn default_buffer_config() -> BufferConfig { BufferConfig::default() }

fn assert_server_hangup(result: Result<(), Box<Error>>) {
    assert!(result.is_err());
    assert!(matches!(*result.unwrap_err(), Error::ServerHangup));
}

fn assert_yellowstone_status(
    result: Result<(), Box<Error>>,
    expected_code: tonic::Code,
    expected_message_substring: &str,
) {
    assert!(result.is_err());
    match *result.unwrap_err() {
        Error::YellowstoneStatus(status) => {
            assert_eq!(status.code(), expected_code);
            assert!(
                status.message().contains(expected_message_substring),
                "expected message to contain {expected_message_substring:?}, got {:?}",
                status.message()
            );
        },
        other => panic!("expected YellowstoneStatus, got {other:?}"),
    }
}

fn assert_other_error(result: Result<(), Box<Error>>) {
    assert!(result.is_err());
    assert!(matches!(*result.unwrap_err(), Error::Other(_)));
}

fn create_status_channel() -> (
    oneshot::Sender<SourceExitStatus>,
    oneshot::Receiver<SourceExitStatus>,
) {
    oneshot::channel()
}

#[allow(clippy::type_complexity)]
fn create_update_channel() -> (
    Sender<Result<SubscribeUpdate, tonic::Status>>,
    tokio::sync::mpsc::Receiver<Result<SubscribeUpdate, tonic::Status>>,
) {
    tokio::sync::mpsc::channel(1)
}

fn drop_receiver<T>(rx: T) { drop(rx); }

async fn send_update_expecting_failure(tx: &Sender<Result<SubscribeUpdate, tonic::Status>>) {
    let result = tx.send(Ok(make_ping_update())).await;
    assert!(result.is_err(), "Send should fail when receiver dropped");
}

fn assert_receiver_dropped(status: &SourceExitStatus) {
    assert!(matches!(status, SourceExitStatus::ReceiverDropped));
}

fn assert_stream_ended(status: &SourceExitStatus) {
    assert!(
        matches!(status, SourceExitStatus::StreamEnded),
        "Expected StreamEnded, got {status:?}"
    );
}

fn assert_completed(status: &SourceExitStatus) {
    assert!(
        matches!(status, SourceExitStatus::Completed),
        "Expected Completed, got {status:?}"
    );
}

fn assert_stream_error_details(
    status: &SourceExitStatus,
    expected_code: tonic::Code,
    expected_msg: &str,
) {
    match status {
        SourceExitStatus::StreamError { code, message } => {
            assert_eq!(*code, expected_code);
            assert_eq!(message, expected_msg);
        },
        _ => panic!("Expected StreamError, got {status:?}"),
    }
}

fn assert_stream_error_code(status: &SourceExitStatus, expected_code: tonic::Code) {
    match status {
        SourceExitStatus::StreamError { code, .. } => {
            assert_eq!(*code, expected_code);
        },
        _ => panic!("Expected StreamError, got {status:?}"),
    }
}

fn assert_error_message(status: &SourceExitStatus, expected: &str) {
    match status {
        SourceExitStatus::Error(msg) => assert_eq!(msg, expected),
        _ => panic!("Expected Error, got {status:?}"),
    }
}

fn assert_send_fails<T, E>(result: &Result<T, E>) {
    assert!(result.is_err());
}

#[derive(Debug)]
struct MockStreamEndSource;

#[async_trait]
impl SourceTrait for MockStreamEndSource {
    async fn connect(
        &self,
        _filters: Filters,
        tx: Sender<Result<SubscribeUpdate, tonic::Status>>,
        status_tx: oneshot::Sender<SourceExitStatus>,
    ) -> Result<(), Error> {
        wait_for_runtime_ready().await;
        signal_stream_ended(status_tx);
        hold_channel_open_briefly().await;
        drop(tx);
        Ok(())
    }
}

#[derive(Debug)]
struct MockStreamErrorSource;

#[async_trait]
impl SourceTrait for MockStreamErrorSource {
    async fn connect(
        &self,
        _filters: Filters,
        tx: Sender<Result<SubscribeUpdate, tonic::Status>>,
        _status_tx: oneshot::Sender<SourceExitStatus>,
    ) -> Result<(), Error> {
        wait_for_runtime_ready().await;
        let _ = tx
            .send(Err(tonic::Status::unavailable("server unavailable")))
            .await;
        // Buffer handles stream errors via tx channel - no need for oneshot
        hold_channel_open_briefly().await;
        Ok(())
    }
}

#[derive(Debug)]
struct MockSourceExitStreamErrorSource;

#[async_trait]
impl SourceTrait for MockSourceExitStreamErrorSource {
    async fn connect(
        &self,
        _filters: Filters,
        tx: Sender<Result<SubscribeUpdate, tonic::Status>>,
        status_tx: oneshot::Sender<SourceExitStatus>,
    ) -> Result<(), Error> {
        wait_for_runtime_ready().await;
        signal_stream_error(
            status_tx,
            tonic::Code::InvalidArgument,
            "failed to get replay position for slot 42",
        );
        hold_channel_open_briefly().await;
        drop(tx);
        Ok(())
    }
}

#[derive(Debug)]
struct MockErrorSource;

#[async_trait]
impl SourceTrait for MockErrorSource {
    async fn connect(
        &self,
        _filters: Filters,
        tx: Sender<Result<SubscribeUpdate, tonic::Status>>,
        status_tx: oneshot::Sender<SourceExitStatus>,
    ) -> Result<(), Error> {
        wait_for_runtime_ready().await;
        signal_error(status_tx, "something went wrong");
        hold_channel_open_briefly().await;
        drop(tx);
        Ok(())
    }
}

#[derive(Debug)]
struct MockStreamEndWithUpdatesSource {
    updates_to_send: u64,
}

#[async_trait]
impl SourceTrait for MockStreamEndWithUpdatesSource {
    async fn connect(
        &self,
        _filters: Filters,
        tx: Sender<Result<SubscribeUpdate, tonic::Status>>,
        status_tx: oneshot::Sender<SourceExitStatus>,
    ) -> Result<(), Error> {
        wait_for_runtime_ready().await;

        for _ in 0..self.updates_to_send {
            if tx.send(Ok(make_ping_update())).await.is_err() {
                signal_receiver_dropped(status_tx);
                return Ok(());
            }
        }

        signal_stream_ended(status_tx);
        hold_channel_open_briefly().await;
        drop(tx);
        Ok(())
    }
}

#[derive(Debug)]
struct MockCompletedWithUpdatesSource {
    updates_to_send: u64,
}

#[async_trait]
impl SourceTrait for MockCompletedWithUpdatesSource {
    async fn connect(
        &self,
        _filters: Filters,
        tx: Sender<Result<SubscribeUpdate, tonic::Status>>,
        status_tx: oneshot::Sender<SourceExitStatus>,
    ) -> Result<(), Error> {
        wait_for_runtime_ready().await;

        for slot in 0..self.updates_to_send {
            if tx.send(Ok(make_slot_update(slot))).await.is_err() {
                signal_receiver_dropped(status_tx);
                return Ok(());
            }
        }

        signal_completed(status_tx);
        drop(tx);
        Ok(())
    }
}

#[derive(Debug, Clone, Copy)]
struct SlowSlotParser;

impl Parser for SlowSlotParser {
    type Input = SlotUpdate;
    type Output = SlotUpdate;

    fn id(&self) -> Cow<'static, str> { TEST_SLOT_FILTER.into() }

    fn prefilter(&self) -> Prefilter { Prefilter::default() }

    async fn parse(&self, value: &Self::Input) -> ParseResult<Self::Output> { Ok(value.clone()) }
}

#[derive(Debug, Clone, Copy)]
struct SlowSlotHandler;

impl Handler<SlotUpdate, SlotUpdate> for SlowSlotHandler {
    async fn handle(
        &self,
        _value: &SlotUpdate,
        _raw_event: &SlotUpdate,
    ) -> crate::HandlerResult<()> {
        tokio::time::sleep(Duration::from_millis(50)).await;
        SLOW_SLOT_HANDLED.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
}

/// Filter sets a source received through the runtime filter update channel.
/// Tests run in parallel and all share this, so each one marks its sets with
/// its own owner pubkey and asserts on that marker alone.
static RECEIVED_FILTERS: Mutex<Vec<Filters>> = Mutex::new(Vec::new());

/// Sets the runtime handed to `connect_with_filter_updates`, which is the
/// initial subscribe. Separate from `RECEIVED_FILTERS` so a test can tell the first
/// request apart from the updates that follow it, under the same
/// mark-and-filter convention.
static INITIAL_FILTERS: Mutex<Vec<Filters>> = Mutex::new(Vec::new());

#[derive(Debug)]
struct MockFilterUpdateSource;

#[async_trait]
impl SourceTrait for MockFilterUpdateSource {
    async fn connect(
        &self,
        _filters: Filters,
        tx: Sender<Result<SubscribeUpdate, tonic::Status>>,
        status_tx: oneshot::Sender<SourceExitStatus>,
    ) -> Result<(), Error> {
        wait_for_runtime_ready().await;
        signal_stream_ended(status_tx);
        hold_channel_open_briefly().await;
        drop(tx);
        Ok(())
    }

    /// Records the set the runtime passed in, which is the initial subscribe,
    /// then every set published until the last handle is dropped, so a test
    /// controls when the run finishes by dropping its handle.
    async fn connect_with_filter_updates(
        &self,
        filters: Filters,
        tx: Sender<Result<SubscribeUpdate, tonic::Status>>,
        status_tx: oneshot::Sender<SourceExitStatus>,
        mut filter_updates_rx: watch::Receiver<Filters>,
    ) -> Result<(), Error> {
        INITIAL_FILTERS
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(filters);

        wait_for_runtime_ready().await;

        while filter_updates_rx.changed().await.is_ok() {
            let filters = filter_updates_rx.borrow_and_update().clone();
            RECEIVED_FILTERS
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(filters);
        }

        signal_stream_ended(status_tx);
        hold_channel_open_briefly().await;
        drop(tx);
        Ok(())
    }
}

impl FilterUpdateSource for MockFilterUpdateSource {}

/// A runtime with one registered slot pipeline, so `TEST_SLOT_FILTER` is a
/// parser ID that filter updates may name.
fn filter_update_runtime() -> Runtime<MockFilterUpdateSource> {
    Runtime::builder()
        .slot(Pipeline::new(SlowSlotParser, [SlowSlotHandler]))
        .try_build_with(MockFilterUpdateSource, default_buffer_config())
        .unwrap()
}

/// A prefilter carrying an actual account owner, since `Prefilter::default()`
/// converts to an entirely empty `SubscribeRequest`. The owner doubles as a
/// per-test marker.
fn owned_by(marker: u8) -> Prefilter {
    Prefilter {
        account: Some(AccountPrefilter {
            accounts: HashSet::new(),
            owners: HashSet::from([Pubkey::new([marker; 32])]),
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// Whether any set recorded in `recorded` carries `marker` under the slot
/// parser. Copies out and releases the guard before the caller asserts, so a
/// failing assertion cannot poison a static every other test locks.
fn recorded_marker(recorded: &Mutex<Vec<Filters>>, marker: u8) -> (bool, String) {
    let sets = recorded.lock().unwrap_or_else(PoisonError::into_inner);

    let seen = sets
        .iter()
        .filter_map(|filters| owners_of(filters, TEST_SLOT_FILTER))
        .any(|owners| owners.contains(&Pubkey::new([marker; 32])));

    (seen, format!("{sets:?}"))
}

fn owners_of(filters: &Filters, parser_id: &str) -> Option<HashSet<Pubkey>> {
    filters
        .get(parser_id)
        .and_then(|prefilter| prefilter.account.as_ref())
        .map(|account| account.owners.clone())
}

#[tokio::test]
async fn test_handle_filters_are_seeded_from_registered_pipelines() {
    let runtime = filter_update_runtime();

    let filters = runtime.handle().filters();

    assert_eq!(filters.parser_ids().collect::<Vec<_>>(), [TEST_SLOT_FILTER]);
}

#[tokio::test]
async fn test_update_filters_reaches_the_source_while_it_runs() {
    let runtime = filter_update_runtime();
    let handle = runtime.handle();

    let (result, ()) = tokio::join!(runtime.try_run_async(), async {
        // The update has to land after the source has read its seed, or it
        // folds into the initial subscribe and this stops testing delivery to
        // a running source. Poll order alone does not guarantee that.
        wait_for_runtime_ready().await;

        handle
            .update_filters(|filters| filters.merge(TEST_SLOT_FILTER, owned_by(1)))
            .unwrap();

        // Ending the run is the source's reaction to the last handle going away.
        drop(handle);
    });

    assert_server_hangup(result);

    let (marked, recorded) = recorded_marker(&RECEIVED_FILTERS, 1);

    assert!(
        marked,
        "source never saw the merged owner, recorded {recorded}"
    );
}

#[tokio::test]
async fn test_update_filters_advances_the_snapshot() {
    let runtime = filter_update_runtime();
    let handle = runtime.handle();

    handle
        .update_filters(|filters| filters.merge(TEST_SLOT_FILTER, owned_by(2)))
        .unwrap();

    assert_eq!(
        owners_of(&handle.filters(), TEST_SLOT_FILTER),
        Some(HashSet::from([Pubkey::new([2; 32])]))
    );
}

#[tokio::test]
async fn test_update_filters_rejects_an_unregistered_parser() {
    let runtime = filter_update_runtime();
    let handle = runtime.handle();

    let result = handle.update_filters(|filters| {
        filters.insert("test::Unregistered", owned_by(3));
    });

    assert_eq!(
        result,
        Err(FilterUpdateError::UnknownParser(
            "test::Unregistered".to_owned()
        ))
    );
    assert!(handle.filters().get("test::Unregistered").is_none());
}

#[tokio::test]
async fn test_send_filter_update_replaces_the_whole_set() {
    let runtime = filter_update_runtime();
    let handle = runtime.handle();

    handle
        .send_filter_update(Filters::new(HashMap::new()))
        .unwrap();

    assert_eq!(handle.filters().parser_ids().count(), 0);
}

/// An edit that changes nothing must not republish the set. Marker 7 is unique
/// to this test because the recording statics are shared across the binary.
#[tokio::test]
async fn test_update_that_changes_nothing_is_not_handed_to_the_source() {
    let runtime = filter_update_runtime();
    let handle = runtime.handle();

    let (result, ()) = tokio::join!(runtime.try_run_async(), async {
        wait_for_runtime_ready().await;

        handle
            .update_filters(|filters| filters.merge(TEST_SLOT_FILTER, owned_by(7)))
            .unwrap();

        // Two spellings of "no change": an empty edit, and the set that is
        // already live sent back whole.
        handle.update_filters(|_| {}).unwrap();
        handle.send_filter_update(handle.filters()).unwrap();

        drop(handle);
    });

    assert_server_hangup(result);

    let sets = RECEIVED_FILTERS
        .lock()
        .unwrap_or_else(PoisonError::into_inner);

    let with_marker = sets
        .iter()
        .filter_map(|filters| owners_of(filters, TEST_SLOT_FILTER))
        .filter(|owners| owners.contains(&Pubkey::new([7; 32])))
        .count();

    assert_eq!(
        with_marker, 1,
        "the real update should reach the source once and the two no-ops not at all, got {sets:?}"
    );
}

#[tokio::test]
async fn test_reset_filters_restores_the_registered_set() {
    let runtime = filter_update_runtime();
    let handle = runtime.handle();

    handle
        .update_filters(|filters| filters.merge(TEST_SLOT_FILTER, owned_by(4)))
        .unwrap();

    handle.reset_filters().unwrap();

    let filters = handle.filters();
    assert_eq!(filters.parser_ids().collect::<Vec<_>>(), [TEST_SLOT_FILTER]);
    assert!(owners_of(&filters, TEST_SLOT_FILTER).is_none());
}

const TEST_IX_PARSER: &str = "test::MarkerIxParser";

#[derive(Debug, Clone, Copy)]
struct MarkerIxParser;

impl Parser for MarkerIxParser {
    type Input = InstructionUpdate;
    type Output = ();

    fn id(&self) -> Cow<'static, str> { TEST_IX_PARSER.into() }

    fn prefilter(&self) -> Prefilter { Prefilter::default() }

    async fn parse(&self, _value: &Self::Input) -> ParseResult<Self::Output> { Ok(()) }
}

#[derive(Debug, Clone, Copy)]
struct MarkerIxHandler;

impl Handler<(), InstructionUpdate> for MarkerIxHandler {
    async fn handle(&self, _value: &(), _raw: &InstructionUpdate) -> HandlerResult<()> { Ok(()) }
}

fn instruction_filter_update_runtime() -> Runtime<MockFilterUpdateSource> {
    Runtime::<MockFilterUpdateSource>::builder()
        .instruction(Pipeline::new(MarkerIxParser, [MarkerIxHandler]))
        .try_build_with(MockFilterUpdateSource, default_buffer_config())
        .unwrap()
}

/// The builder bundles every instruction parser behind one
/// [`InstructionPipeline`], so the filter set keys them under its ID rather
/// than each parser's own. Naming the parser is the mistake the docs have to
/// steer callers away from.
#[tokio::test]
async fn test_instruction_filters_are_keyed_by_the_bundle_not_the_parser() {
    let runtime = instruction_filter_update_runtime();
    let handle = runtime.handle();

    assert_eq!(handle.filters().parser_ids().collect::<Vec<_>>(), [
        InstructionPipeline::ID
    ]);

    let result = handle.update_filters(|filters| filters.merge(MarkerIxParser.id(), owned_by(5)));

    assert_eq!(
        result,
        Err(FilterUpdateError::UnknownParser(TEST_IX_PARSER.to_owned()))
    );

    handle
        .update_filters(|filters| filters.merge(InstructionPipeline::ID, owned_by(5)))
        .unwrap();

    assert_eq!(
        owners_of(&handle.filters(), InstructionPipeline::ID),
        Some(HashSet::from([Pubkey::new([5; 32])]))
    );
}

/// `update_filters` holds a lock across the read, the edit and the send, so
/// concurrent callers on separate handles serialise instead of overwriting one
/// another. Without the lock the last writer's snapshot wins and the other
/// owners vanish.
#[tokio::test]
async fn test_concurrent_update_filters_lose_no_edit() {
    const WRITERS: u8 = 8;

    let runtime = filter_update_runtime();
    let handle = runtime.handle();

    let writers: Vec<_> = (0..WRITERS)
        .map(|marker| {
            // A clone per thread also pins that handles share one view.
            let handle = handle.clone();

            std::thread::spawn(move || {
                handle
                    .update_filters(|filters| {
                        filters.merge(TEST_SLOT_FILTER, owned_by(0x40 + marker));
                    })
                    .unwrap();
            })
        })
        .collect();

    for writer in writers {
        writer.join().expect("writer thread panicked");
    }

    let owners = owners_of(&handle.filters(), TEST_SLOT_FILTER).expect("owners must be set");

    for marker in 0..WRITERS {
        assert!(
            owners.contains(&Pubkey::new([0x40 + marker; 32])),
            "edit from writer {marker} was lost, got {owners:?}"
        );
    }
}

/// An `edit` closure that panics leaves the lock poisoned. The next caller has
/// to get through anyway, because nothing partial was published.
#[tokio::test]
async fn test_update_filters_recovers_from_a_poisoned_lock() {
    let runtime = filter_update_runtime();
    let handle = runtime.handle();

    let poisoner = handle.clone();
    std::thread::spawn(move || {
        poisoner.update_filters(|_| panic!("edit blew up")).ok();
    })
    .join()
    .expect_err("the writer must have panicked");

    handle
        .update_filters(|filters| filters.merge(TEST_SLOT_FILTER, owned_by(0x39)))
        .expect("a poisoned lock must not wedge the handle");

    assert_eq!(
        owners_of(&handle.filters(), TEST_SLOT_FILTER),
        Some(HashSet::from([Pubkey::new([0x39; 32])]))
    );
}

#[tokio::test]
async fn test_filter_updates_rejected_once_the_runtime_is_gone() {
    let runtime = filter_update_runtime();
    let handle = runtime.handle();
    drop(runtime);

    let result = handle.update_filters(|_| {});

    assert_eq!(result, Err(FilterUpdateError::Closed));
}

/// An update sent before `run` must reach the initial subscribe, not arrive as
/// a second request that leaves the wider set live in between.
#[tokio::test]
async fn test_update_before_run_reaches_the_initial_subscribe() {
    let runtime = filter_update_runtime();
    let handle = runtime.handle();

    handle
        .update_filters(|filters| filters.merge(TEST_SLOT_FILTER, owned_by(6)))
        .unwrap();

    let (result, ()) = tokio::join!(runtime.try_run_async(), async {
        wait_for_runtime_ready().await;
        drop(handle);
    });

    assert_server_hangup(result);

    let (marked, subscribed) = recorded_marker(&INITIAL_FILTERS, 6);

    assert!(
        marked,
        "initial subscribe used a stale set, subscribed with {subscribed}"
    );

    // Seeding marks the set seen, so it must not also arrive as an update.
    let (resent, received) = recorded_marker(&RECEIVED_FILTERS, 6);

    assert!(
        !resent,
        "seeded set was resent as an update, got {received}"
    );
}

#[tokio::test]
async fn test_source_runs_when_the_filter_update_handle_is_never_taken() {
    let runtime = filter_update_runtime();

    assert_server_hangup(runtime.try_run_async().await);
}

#[tokio::test]
async fn test_stream_end_returns_error() {
    let runtime = Runtime::builder()
        .try_build_with(MockStreamEndSource, default_buffer_config())
        .unwrap();

    assert_server_hangup(runtime.try_run_async().await);
}

#[tokio::test]
async fn test_stream_error_returns_error() {
    let runtime = Runtime::builder()
        .try_build_with(MockStreamErrorSource, default_buffer_config())
        .unwrap();

    assert!(runtime.try_run_async().await.is_err());
}

#[tokio::test]
async fn test_source_exit_stream_error_maps_to_yellowstone_status() {
    let runtime = Runtime::builder()
        .try_build_with(MockSourceExitStreamErrorSource, default_buffer_config())
        .unwrap();

    assert_yellowstone_status(
        runtime.try_run_async().await,
        tonic::Code::InvalidArgument,
        "replay position",
    );
}

#[tokio::test]
async fn test_error_status_returns_error() {
    let runtime = Runtime::builder()
        .try_build_with(MockErrorSource, default_buffer_config())
        .unwrap();

    assert_other_error(runtime.try_run_async().await);
}

#[tokio::test]
async fn test_stream_end_after_updates_returns_error() {
    let runtime = Runtime::builder()
        .try_build_with(
            MockStreamEndWithUpdatesSource { updates_to_send: 5 },
            default_buffer_config(),
        )
        .unwrap();

    assert_server_hangup(runtime.try_run_async().await);
}

#[tokio::test]
async fn test_completed_source_drains_buffered_updates_before_returning() {
    SLOW_SLOT_HANDLED.store(0, Ordering::Relaxed);

    let runtime = Runtime::builder()
        .slot(Pipeline::new(SlowSlotParser, [SlowSlotHandler]))
        .try_build_with(
            MockCompletedWithUpdatesSource { updates_to_send: 3 },
            default_buffer_config(),
        )
        .unwrap();

    assert!(runtime.try_run_async().await.is_ok());
    assert_eq!(
        SLOW_SLOT_HANDLED.load(Ordering::Relaxed),
        3,
        "runtime must wait for buffered slot handlers after finite source completion"
    );
}

#[tokio::test]
async fn test_source_exit_status_receiver_dropped() {
    let (tx, rx) = create_update_channel();
    let (status_tx, status_rx) = create_status_channel();

    drop_receiver(rx);
    send_update_expecting_failure(&tx).await;
    signal_receiver_dropped(status_tx);

    assert_receiver_dropped(&status_rx.await.unwrap());
}

#[tokio::test]
async fn test_source_exit_status_stream_ended() {
    let (status_tx, status_rx) = create_status_channel();

    signal_stream_ended(status_tx);

    assert_stream_ended(&status_rx.await.unwrap());
}

#[tokio::test]
async fn test_source_exit_status_completed() {
    let (status_tx, status_rx) = create_status_channel();

    signal_completed(status_tx);

    assert_completed(&status_rx.await.unwrap());
}

#[tokio::test]
async fn test_source_exit_status_stream_error_preserves_details() {
    let (status_tx, status_rx) = create_status_channel();

    signal_stream_error(status_tx, tonic::Code::PermissionDenied, "auth expired");

    assert_stream_error_details(
        &status_rx.await.unwrap(),
        tonic::Code::PermissionDenied,
        "auth expired",
    );
}

#[tokio::test]
async fn test_source_exit_status_error_preserves_message() {
    let (status_tx, status_rx) = create_status_channel();

    signal_error(status_tx, "connection timeout");

    assert_error_message(&status_rx.await.unwrap(), "connection timeout");
}

#[tokio::test]
async fn test_grpc_unavailable_error() {
    let (status_tx, status_rx) = create_status_channel();

    signal_stream_error(status_tx, tonic::Code::Unavailable, "service unavailable");

    assert_stream_error_code(&status_rx.await.unwrap(), tonic::Code::Unavailable);
}

#[tokio::test]
async fn test_grpc_unauthenticated_error() {
    let (status_tx, status_rx) = create_status_channel();

    signal_stream_error(status_tx, tonic::Code::Unauthenticated, "invalid token");

    assert_stream_error_details(
        &status_rx.await.unwrap(),
        tonic::Code::Unauthenticated,
        "invalid token",
    );
}

#[tokio::test]
async fn test_grpc_resource_exhausted_error() {
    let (status_tx, status_rx) = create_status_channel();

    signal_stream_error(
        status_tx,
        tonic::Code::ResourceExhausted,
        "rate limit exceeded",
    );

    assert_stream_error_code(&status_rx.await.unwrap(), tonic::Code::ResourceExhausted);
}

#[tokio::test]
async fn test_status_channel_dropped_before_send() {
    let (status_tx, status_rx) = create_status_channel();

    drop_receiver(status_rx);

    assert_send_fails(&status_tx.send(SourceExitStatus::StreamEnded));
}

// Buffer pool edge cases. Each test uses its own atomic counter so they stay
// correct under parallel `cargo test`.

static BURST_HANDLED: AtomicUsize = AtomicUsize::new(0);
static PANIC_SURVIVED: AtomicUsize = AtomicUsize::new(0);
static SERIAL_HANDLED: AtomicUsize = AtomicUsize::new(0);
static STOP_RESPONSIVE_HANDLED: AtomicUsize = AtomicUsize::new(0);
static ZERO_JOBS_HANDLED: AtomicUsize = AtomicUsize::new(0);

/// Emits `N` slot updates then signals `Completed`.
#[derive(Debug)]
struct MockBurstSource<const N: u64>;

#[async_trait]
impl<const N: u64> SourceTrait for MockBurstSource<N> {
    async fn connect(
        &self,
        _filters: Filters,
        tx: Sender<Result<SubscribeUpdate, tonic::Status>>,
        status_tx: oneshot::Sender<SourceExitStatus>,
    ) -> Result<(), Error> {
        wait_for_runtime_ready().await;
        for slot in 0..N {
            if tx.send(Ok(make_slot_update(slot))).await.is_err() {
                signal_receiver_dropped(status_tx);
                return Ok(());
            }
        }
        signal_completed(status_tx);
        drop(tx);
        Ok(())
    }
}

/// Bumps a counter; optionally panics on even slots or sleeps to force overlap.
#[derive(Debug, Clone, Copy)]
struct CountingHandler {
    counter: &'static AtomicUsize,
    sleep: Duration,
    panic_on_even_slot: bool,
}

impl Handler<SlotUpdate, SlotUpdate> for CountingHandler {
    async fn handle(
        &self,
        value: &SlotUpdate,
        _raw_event: &SlotUpdate,
    ) -> crate::HandlerResult<()> {
        // assert! form (not `if c { panic!() }`) satisfies clippy::manual_assert.
        assert!(
            !(self.panic_on_even_slot && value.slot.is_multiple_of(2)),
            "intentional handler panic on slot {}",
            value.slot
        );
        if !self.sleep.is_zero() {
            tokio::time::sleep(self.sleep).await;
        }
        self.counter.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
}

/// Slow handler, to actually exercise backpressure at this burst size.
#[tokio::test]
async fn edge_high_volume_drain_processes_every_update() {
    const N: u64 = 500;
    BURST_HANDLED.store(0, Ordering::Relaxed);

    let runtime = Runtime::builder()
        .slot(Pipeline::new(SlowSlotParser, [CountingHandler {
            counter: &BURST_HANDLED,
            sleep: Duration::from_millis(1),
            panic_on_even_slot: false,
        }]))
        .try_build_with(MockBurstSource::<N>, default_buffer_config())
        .unwrap();

    assert!(runtime.try_run_async().await.is_ok());
    assert_eq!(
        BURST_HANDLED.load(Ordering::Relaxed),
        usize::try_from(N).unwrap(),
        "all {N} updates must be handled"
    );
}

/// Each handler runs as its own task, so a panic stays isolated and the pool
/// survives.
#[tokio::test]
async fn edge_panicking_handler_does_not_kill_pool() {
    const N: u64 = 20; // slots 0..20 -> 10 even (panic), 10 odd (survive)
    PANIC_SURVIVED.store(0, Ordering::Relaxed);

    let runtime = Runtime::builder()
        .slot(Pipeline::new(SlowSlotParser, [CountingHandler {
            counter: &PANIC_SURVIVED,
            sleep: Duration::from_millis(1),
            panic_on_even_slot: true,
        }]))
        .try_build_with(MockBurstSource::<N>, default_buffer_config())
        .unwrap();

    // ~10 "task panicked" lines on stderr are expected.
    assert!(runtime.try_run_async().await.is_ok());
    assert_eq!(
        PANIC_SURVIVED.load(Ordering::Relaxed),
        10,
        "odd-slot handlers must all run despite even-slot panics"
    );
}

#[tokio::test]
async fn edge_single_job_serializes_and_drains() {
    const N: u64 = 25;
    SERIAL_HANDLED.store(0, Ordering::Relaxed);

    let buffer = BufferConfig {
        jobs: Some(1),
        ..BufferConfig::default()
    };

    let runtime = Runtime::builder()
        .slot(Pipeline::new(SlowSlotParser, [CountingHandler {
            counter: &SERIAL_HANDLED,
            sleep: Duration::from_millis(1),
            panic_on_even_slot: false,
        }]))
        .try_build_with(MockBurstSource::<N>, buffer)
        .unwrap();

    assert!(runtime.try_run_async().await.is_ok());
    assert_eq!(
        SERIAL_HANDLED.load(Ordering::Relaxed),
        usize::try_from(N).unwrap(),
        "jobs=1 must still process every update"
    );
}

/// Parks forever, like a handler blocked on a full channel whose reader
/// stalled. Holds its job permit the whole time.
#[derive(Debug, Clone, Copy)]
struct WedgedHandler;

impl Handler<SlotUpdate, SlotUpdate> for WedgedHandler {
    async fn handle(&self, _: &SlotUpdate, _: &SlotUpdate) -> crate::HandlerResult<()> {
        std::future::pending::<()>().await;
        unreachable!("pending never resolves")
    }
}

/// Holds its permit forever, so the drain barrier can't acquire them all; must
/// give up at the ceiling instead of hanging.
#[tokio::test]
async fn edge_wedged_handler_does_not_hang_shutdown() {
    const N: u64 = 2;

    let runtime = Runtime::builder()
        .slot(Pipeline::new(SlowSlotParser, [WedgedHandler]))
        .try_build_with(MockBurstSource::<N>, default_buffer_config())
        .unwrap();

    // Fails by hanging if the barrier is unbounded; the outer timeout is the
    // real assertion, sized well above the (test-shortened) drain ceiling.
    let ran = tokio::time::timeout(Duration::from_secs(5), runtime.try_run_async()).await;

    assert!(
        ran.is_ok(),
        "shutdown must not block on a handler that never returns"
    );
    assert!(ran.unwrap().is_ok(), "bounded drain is a clean shutdown");
}

static PROBE_IN_FLIGHT: AtomicUsize = AtomicUsize::new(0);
static PROBE_PEAK: AtomicUsize = AtomicUsize::new(0);
static PROBE_HANDLED: AtomicUsize = AtomicUsize::new(0);

/// Tracks how many copies of itself run at once, recording the peak.
#[derive(Debug, Clone, Copy)]
struct ConcurrencyProbeHandler {
    in_flight: &'static AtomicUsize,
    peak: &'static AtomicUsize,
    handled: &'static AtomicUsize,
}

impl Handler<SlotUpdate, SlotUpdate> for ConcurrencyProbeHandler {
    async fn handle(
        &self,
        value: &SlotUpdate,
        _raw_event: &SlotUpdate,
    ) -> crate::HandlerResult<()> {
        // A handler only observes in-flight concurrency; peak in-flight <= jobs
        // is the observable half of the queued+in-flight <= jobs bound.
        let current = self.in_flight.fetch_add(1, Ordering::Relaxed) + 1;
        self.peak.fetch_max(current, Ordering::Relaxed);

        // Vary latency so handlers overlap.
        tokio::time::sleep(Duration::from_millis(1 + value.slot % 5)).await;

        self.in_flight.fetch_sub(1, Ordering::Relaxed);
        self.handled.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
}

/// Peak must exceed 1, or the bound goes unexercised.
#[tokio::test]
async fn edge_concurrency_never_exceeds_jobs() {
    const N: u64 = 200;
    const JOBS: usize = 4;
    PROBE_IN_FLIGHT.store(0, Ordering::Relaxed);
    PROBE_PEAK.store(0, Ordering::Relaxed);
    PROBE_HANDLED.store(0, Ordering::Relaxed);

    let buffer = BufferConfig {
        jobs: Some(JOBS),
        ..BufferConfig::default()
    };

    let runtime = Runtime::builder()
        .slot(Pipeline::new(SlowSlotParser, [ConcurrencyProbeHandler {
            in_flight: &PROBE_IN_FLIGHT,
            peak: &PROBE_PEAK,
            handled: &PROBE_HANDLED,
        }]))
        .try_build_with(MockBurstSource::<N>, buffer)
        .unwrap();

    assert!(runtime.try_run_async().await.is_ok());

    let peak = PROBE_PEAK.load(Ordering::Relaxed);
    assert!(peak <= JOBS, "peak in-flight {peak} exceeded jobs={JOBS}");
    assert!(
        peak > 1,
        "handlers never overlapped (peak {peak}); bound not exercised"
    );
    assert_eq!(
        PROBE_HANDLED.load(Ordering::Relaxed),
        usize::try_from(N).unwrap(),
        "every update must still be processed"
    );
}

static ABORT_COMPLETED: AtomicUsize = AtomicUsize::new(0);

/// Sends `PRE` updates, a stream error, then `POST` more. The buffer breaks on
/// the error and drops its receiver, so the `POST` updates are never read.
/// `PRE` < jobs so the producer never blocks on a permit before the error.
#[derive(Debug)]
struct MockAbortAfterSource<const PRE: u64, const POST: u64>;

#[async_trait]
impl<const PRE: u64, const POST: u64> SourceTrait for MockAbortAfterSource<PRE, POST> {
    async fn connect(
        &self,
        _filters: Filters,
        tx: Sender<Result<SubscribeUpdate, tonic::Status>>,
        _status_tx: oneshot::Sender<SourceExitStatus>,
    ) -> Result<(), Error> {
        wait_for_runtime_ready().await;

        for slot in 0..PRE {
            if tx.send(Ok(make_slot_update(slot))).await.is_err() {
                return Ok(());
            }
        }

        // Trigger the abort path via a stream error.
        if tx
            .send(Err(tonic::Status::unavailable("aborting mid-burst")))
            .await
            .is_err()
        {
            return Ok(());
        }

        // Never read: the producer already broke on the error above.
        for slot in PRE..PRE + POST {
            if tx.send(Ok(make_slot_update(slot))).await.is_err() {
                break;
            }
        }

        // Keep the source alive briefly so the error surfaces via the buffer's
        // `wait_for_stop` rather than a status-channel-closed race. The source
        // task is detached, so this does not delay the shutdown return.
        tokio::time::sleep(Duration::from_millis(300)).await;

        Ok(())
    }
}

/// Asserts only the deterministic contract (`completed <= PRE`), never
/// cancelled-vs-unfinished timing, which would be flaky.
#[tokio::test]
async fn edge_abort_discards_queued_and_returns_promptly() {
    const PRE: u64 = 3; // < JOBS, so the producer never blocks on a permit
    const POST: u64 = 100;
    const JOBS: usize = 4;
    const HANDLER_SLEEP: Duration = Duration::from_secs(2);
    const ABORT_BUDGET: Duration = Duration::from_millis(1000);

    ABORT_COMPLETED.store(0, Ordering::Relaxed);

    let buffer = BufferConfig {
        jobs: Some(JOBS),
        ..BufferConfig::default()
    };

    let runtime = Runtime::builder()
        .slot(Pipeline::new(SlowSlotParser, [CountingHandler {
            counter: &ABORT_COMPLETED,
            sleep: HANDLER_SLEEP,
            panic_on_even_slot: false,
        }]))
        .try_build_with(MockAbortAfterSource::<PRE, POST>, buffer)
        .unwrap();

    // Must return within budget, not block on the 2s handlers.
    let result = tokio::time::timeout(ABORT_BUDGET, runtime.try_run_async())
        .await
        .expect("abort must return promptly, not wait for in-flight handlers");

    assert!(result.is_err(), "a stream error must surface as an error");

    // Only the PRE pre-error updates can ever run.
    let completed = ABORT_COMPLETED.load(Ordering::Relaxed);
    assert!(
        completed <= usize::try_from(PRE).unwrap(),
        ">= {POST} post-error updates must be discarded; {completed} ran"
    );
}

/// Sends two updates then a receiver-dropped signal to force shutdown. With
/// jobs=1, the first update's handler holds the only permit, so the producer
/// parks waiting for a permit for the second one when the stop arrives.
#[derive(Debug)]
struct MockStopUnderBackpressureSource;

#[async_trait]
impl SourceTrait for MockStopUnderBackpressureSource {
    async fn connect(
        &self,
        _filters: Filters,
        tx: Sender<Result<SubscribeUpdate, tonic::Status>>,
        status_tx: oneshot::Sender<SourceExitStatus>,
    ) -> Result<(), Error> {
        wait_for_runtime_ready().await;

        // Occupies the single permit for far longer than the shutdown budget.
        let _ = tx.send(Ok(make_slot_update(0))).await;
        // Makes the producer park on the permit acquire.
        let _ = tx.send(Ok(make_slot_update(1))).await;

        // Give the producer time to dequeue update 1 and park, then force stop.
        tokio::time::sleep(Duration::from_millis(300)).await;
        signal_receiver_dropped(status_tx);
        drop(tx);
        Ok(())
    }
}

/// A stop must break the producer out of a permit wait even when a stuck
/// handler holds the only permit.
#[tokio::test]
async fn edge_stop_is_responsive_while_waiting_for_a_permit() {
    const HANDLER_SLEEP: Duration = Duration::from_secs(10);
    const SHUTDOWN_BUDGET: Duration = Duration::from_secs(3);

    STOP_RESPONSIVE_HANDLED.store(0, Ordering::Relaxed);

    let buffer = BufferConfig {
        jobs: Some(1),
        ..BufferConfig::default()
    };

    let runtime = Runtime::builder()
        .slot(Pipeline::new(SlowSlotParser, [CountingHandler {
            counter: &STOP_RESPONSIVE_HANDLED,
            sleep: HANDLER_SLEEP,
            panic_on_even_slot: false,
        }]))
        .try_build_with(MockStopUnderBackpressureSource, buffer)
        .unwrap();

    // Must return well before the 10s handler frees the permit.
    let result = tokio::time::timeout(SHUTDOWN_BUDGET, runtime.try_run_async())
        .await
        .expect("stop must be honored while the producer is waiting for a permit");

    assert!(result.is_ok(), "forced shutdown should return cleanly");
}

/// Clamped to 1: a 0-permit semaphore would park the producer forever.
#[tokio::test]
async fn edge_zero_jobs_does_not_deadlock() {
    const N: u64 = 10;
    ZERO_JOBS_HANDLED.store(0, Ordering::Relaxed);

    let buffer = BufferConfig {
        jobs: Some(0),
        ..BufferConfig::default()
    };

    let runtime = Runtime::builder()
        .slot(Pipeline::new(SlowSlotParser, [CountingHandler {
            counter: &ZERO_JOBS_HANDLED,
            sleep: Duration::from_millis(1),
            panic_on_even_slot: false,
        }]))
        .try_build_with(MockBurstSource::<N>, buffer)
        .unwrap();

    // Bounded so a 0-permit deadlock regression fails instead of hanging.
    let result = tokio::time::timeout(Duration::from_secs(5), runtime.try_run_async())
        .await
        .expect("jobs=0 must not deadlock");

    assert!(result.is_ok());
    assert_eq!(
        ZERO_JOBS_HANDLED.load(Ordering::Relaxed),
        usize::try_from(N).unwrap(),
        "every update must be processed when jobs is clamped from 0 to 1"
    );
}

static STOP_INFLIGHT_STARTED: AtomicUsize = AtomicUsize::new(0);
static STOP_INFLIGHT_FINISHED: AtomicUsize = AtomicUsize::new(0);

/// Counts entry and completion separately, so a cancelled handler is
/// distinguishable from a completed one.
#[derive(Debug, Clone, Copy)]
struct StartFinishHandler {
    sleep: Duration,
}

impl Handler<SlotUpdate, SlotUpdate> for StartFinishHandler {
    async fn handle(&self, _: &SlotUpdate, _: &SlotUpdate) -> crate::HandlerResult<()> {
        STOP_INFLIGHT_STARTED.fetch_add(1, Ordering::SeqCst);
        tokio::time::sleep(self.sleep).await;
        STOP_INFLIGHT_FINISHED.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

/// Sends `N` updates, then stops while their handlers are still running.
#[derive(Debug)]
struct MockStopWithInFlightSource<const N: u64>;

#[async_trait]
impl<const N: u64> SourceTrait for MockStopWithInFlightSource<N> {
    async fn connect(
        &self,
        _filters: Filters,
        tx: Sender<Result<SubscribeUpdate, tonic::Status>>,
        status_tx: oneshot::Sender<SourceExitStatus>,
    ) -> Result<(), Error> {
        for slot in 0..N {
            let _ = tx.send(Ok(make_slot_update(slot))).await;
        }

        // Long enough for every handler to be parked in its sleep.
        tokio::time::sleep(Duration::from_millis(20)).await;
        signal_receiver_dropped(status_tx);

        // Keeps the channel open, so this is the stop path, not a clean close.
        tokio::time::sleep(Duration::from_secs(5)).await;
        drop(tx);
        Ok(())
    }
}

/// A stop must not cut off handlers that already started: `try_run` drops the
/// runtime once shutdown returns, so anything unfinished dies mid-write.
#[tokio::test]
async fn edge_stop_waits_for_in_flight_handlers() {
    const N: u64 = 2;
    const HANDLER_SLEEP: Duration = Duration::from_millis(60);

    STOP_INFLIGHT_STARTED.store(0, Ordering::SeqCst);
    STOP_INFLIGHT_FINISHED.store(0, Ordering::SeqCst);

    let buffer = BufferConfig {
        jobs: Some(usize::try_from(N).unwrap()),
        ..BufferConfig::default()
    };

    let runtime = Runtime::builder()
        .slot(Pipeline::new(SlowSlotParser, [StartFinishHandler {
            sleep: HANDLER_SLEEP,
        }]))
        .try_build_with(MockStopWithInFlightSource::<N>, buffer)
        .unwrap();

    assert!(runtime.try_run_async().await.is_ok());

    let started = STOP_INFLIGHT_STARTED.load(Ordering::SeqCst);
    let finished = STOP_INFLIGHT_FINISHED.load(Ordering::SeqCst);

    assert_eq!(
        started,
        usize::try_from(N).unwrap(),
        "both handlers must have started before the stop"
    );
    assert_eq!(
        finished, started,
        "only {finished} of {started} handlers finished before shutdown returned"
    );
}
