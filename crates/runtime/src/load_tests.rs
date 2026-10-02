//! Synthetic load harness for the runtime buffer, in-process with no I/O.
//! There is no abort case because the permit is taken before the send; see
//! `runtime_tests::edge_abort_discards_queued_and_returns_promptly`. Ignored:
//! slow, with timing asserts that don't suit shared runners.
//!
//! ```sh, ignore
//! cargo test -p shipstern --lib --release load_ -- --ignored --nocapture
//! ```
//!

use std::{
    borrow::Cow,
    sync::{
        atomic::{AtomicU64, AtomicUsize, Ordering},
        LazyLock,
    },
    time::{Duration, Instant},
};

use async_trait::async_trait;
use shipstern_core::{ParseResult, Parser, Prefilter, SlotUpdate};
use tokio::sync::{mpsc::Sender, oneshot};
use yellowstone_grpc_proto::{
    geyser::{subscribe_update::UpdateOneof, SlotStatus, SubscribeUpdate, SubscribeUpdateSlot},
    tonic,
};

use crate::{
    config::{BufferConfig, NullConfig, ShipsternConfig},
    sources::{SourceExitStatus, SourceTrait},
    Error, Handler, Pipeline, Runtime,
};

const LOAD_FILTER: &str = "load::LoadParser";

fn make_slot_update(slot: u64) -> SubscribeUpdate {
    SubscribeUpdate {
        filters: vec![LOAD_FILTER.to_string()],
        update_oneof: Some(UpdateOneof::Slot(SubscribeUpdateSlot {
            slot,
            parent: Some(slot.saturating_sub(1)),
            status: SlotStatus::SlotProcessed as i32,
            dead_error: None,
        })),
        created_at: None,
    }
}

/// Serializes the tests in this module; `tokio`'s mutex doesn't poison, so a
/// panicking test still releases it cleanly.
static LOAD_LOCK: LazyLock<tokio::sync::Mutex<()>> = LazyLock::new(|| tokio::sync::Mutex::new(()));

/// Statics rather than fields: the handler is copied into the pipeline.
static IN_FLIGHT: AtomicUsize = AtomicUsize::new(0);
static PEAK_IN_FLIGHT: AtomicUsize = AtomicUsize::new(0);
static HANDLED: AtomicU64 = AtomicU64::new(0);
static PANICKED: AtomicU64 = AtomicU64::new(0);

fn reset_counters() {
    IN_FLIGHT.store(0, Ordering::SeqCst);
    PEAK_IN_FLIGHT.store(0, Ordering::SeqCst);
    HANDLED.store(0, Ordering::SeqCst);
    PANICKED.store(0, Ordering::SeqCst);
}

/// Records the high-water mark of concurrently running handlers.
fn enter_handler() {
    let now = IN_FLIGHT.fetch_add(1, Ordering::SeqCst) + 1;
    PEAK_IN_FLIGHT.fetch_max(now, Ordering::SeqCst);
}

fn exit_handler() { IN_FLIGHT.fetch_sub(1, Ordering::SeqCst); }

/// Emits `N` updates as fast as the pool accepts them, then completes.
#[derive(Debug)]
struct FloodSource<const N: u64>;

#[async_trait]
impl<const N: u64> SourceTrait for FloodSource<N> {
    type Config = NullConfig;

    fn new(_: NullConfig, _: shipstern_core::Filters) -> Self { Self }

    async fn connect(
        &self,
        tx: Sender<Result<SubscribeUpdate, tonic::Status>>,
        status_tx: oneshot::Sender<SourceExitStatus>,
    ) -> Result<(), Error> {
        for slot in 0..N {
            if tx.send(Ok(make_slot_update(slot))).await.is_err() {
                let _ = status_tx.send(SourceExitStatus::ReceiverDropped);
                return Ok(());
            }
        }

        let _ = status_tx.send(SourceExitStatus::Completed);
        drop(tx);
        Ok(())
    }
}

#[derive(Debug, Clone, Copy)]
struct LoadParser;

impl Parser for LoadParser {
    type Input = SlotUpdate;
    type Output = SlotUpdate;

    fn id(&self) -> Cow<'static, str> { LOAD_FILTER.into() }

    fn prefilter(&self) -> Prefilter { Prefilter::default() }

    async fn parse(&self, value: &Self::Input) -> ParseResult<Self::Output> { Ok(value.clone()) }
}

/// Handler with a configurable cost: sleep, periodic panic, or a permanent
/// wedge on the first `wedge_first` calls.
#[derive(Debug, Clone, Copy)]
struct LoadHandler {
    sleep: Duration,
    panic_every: u64,
    wedge_first: u64,
}

impl LoadHandler {
    const fn instant() -> Self { Self::with_sleep(Duration::ZERO) }

    const fn with_sleep(sleep: Duration) -> Self {
        Self {
            sleep,
            panic_every: 0,
            wedge_first: 0,
        }
    }

    /// Wedges the first `n` calls, which never return.
    const fn wedging(n: u64) -> Self {
        Self {
            wedge_first: n,
            ..Self::instant()
        }
    }

    /// Panics on every `n`th call.
    const fn panicking_every(n: u64) -> Self {
        Self {
            panic_every: n,
            ..Self::instant()
        }
    }
}

impl Handler<SlotUpdate, SlotUpdate> for LoadHandler {
    async fn handle(&self, _: &SlotUpdate, _: &SlotUpdate) -> crate::HandlerResult<()> {
        enter_handler();

        let n = HANDLED.fetch_add(1, Ordering::Relaxed) + 1;

        if n <= self.wedge_first {
            // Holds its permit for good; only the drain ceiling ends this.
            std::future::pending::<()>().await;
        }

        if !self.sleep.is_zero() {
            tokio::time::sleep(self.sleep).await;
        }

        if self.panic_every > 0 && n.is_multiple_of(self.panic_every) {
            PANICKED.fetch_add(1, Ordering::Relaxed);
            // Skips `exit_handler` on purpose, so recovery is the pool's
            // permit accounting rather than ours.
            panic!("intentional load panic at {n}");
        }

        exit_handler();
        Ok(())
    }
}

fn config_with_jobs(jobs: usize) -> ShipsternConfig<NullConfig> {
    ShipsternConfig {
        source: NullConfig,
        buffer: BufferConfig {
            jobs: Some(jobs),
            ..BufferConfig::default()
        },
    }
}

async fn run_flood<const N: u64>(jobs: usize, handler: LoadHandler) -> (Duration, bool) {
    reset_counters();

    let runtime = Runtime::<FloodSource<N>>::builder()
        .slot(Pipeline::new(LoadParser, [handler]))
        .try_build(config_with_jobs(jobs))
        .unwrap();

    let t0 = Instant::now();
    let ok = runtime.try_run_async().await.is_ok();

    (t0.elapsed(), ok)
}

fn rate_per_s(n: u64, elapsed: Duration) -> u128 {
    let micros = elapsed.as_micros();
    if micros == 0 {
        return 0;
    }

    u128::from(n) * 1_000_000 / micros
}

/// Instant handler, so the producer is the bottleneck: raising `jobs` won't
/// move throughput here.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "load test; run with --ignored"]
async fn load_throughput_ceiling_by_jobs() {
    const N: u64 = 200_000;

    let _serialized = LOAD_LOCK.lock().await;

    println!("\n=== throughput ceiling: {N} updates, instant handler ===");

    for jobs in [1_usize, 4, 8, 32] {
        let (elapsed, ok) = run_flood::<N>(jobs, LoadHandler::instant()).await;

        assert!(ok, "jobs={jobs} must exit cleanly");
        assert_eq!(
            HANDLED.load(Ordering::SeqCst),
            N,
            "jobs={jobs} must handle every update"
        );

        println!(
            "  jobs={jobs:<3} {:>8} ix/s  peak_in_flight={:<3} elapsed={:?}",
            rate_per_s(N, elapsed),
            PEAK_IN_FLIGHT.load(Ordering::SeqCst),
            elapsed
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "load test; run with --ignored"]
async fn load_handler_latency_sweep_respects_bound() {
    const N: u64 = 2_000;

    let _serialized = LOAD_LOCK.lock().await;

    println!("\n=== latency sweep: {N} updates ===");

    for sleep_ms in [0_u64, 1, 10] {
        for jobs in [1_usize, 8, 32] {
            let handler = LoadHandler::with_sleep(Duration::from_millis(sleep_ms));
            let (elapsed, ok) = run_flood::<N>(jobs, handler).await;
            let peak = PEAK_IN_FLIGHT.load(Ordering::SeqCst);

            assert!(ok);
            assert_eq!(HANDLED.load(Ordering::SeqCst), N);
            assert!(
                peak <= jobs,
                "sleep={sleep_ms}ms jobs={jobs}: peak {peak} exceeded the bound"
            );

            println!(
                "  sleep={sleep_ms:>2}ms jobs={jobs:<3} peak_in_flight={peak:<3} {:>7} ix/s \
                 elapsed={elapsed:?}",
                rate_per_s(N, elapsed)
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "load test; run with --ignored"]
async fn load_backpressure_caps_outstanding_work() {
    const N: u64 = 5_000;
    const JOBS: usize = 4;

    let _serialized = LOAD_LOCK.lock().await;

    let handler = LoadHandler::with_sleep(Duration::from_millis(1));
    let (elapsed, ok) = run_flood::<N>(JOBS, handler).await;
    let peak = PEAK_IN_FLIGHT.load(Ordering::SeqCst);

    assert!(ok);
    assert_eq!(
        HANDLED.load(Ordering::SeqCst),
        N,
        "no update may be dropped"
    );
    assert!(peak <= JOBS, "peak {peak} exceeded jobs={JOBS}");
    assert_eq!(
        IN_FLIGHT.load(Ordering::SeqCst),
        0,
        "every handler must have exited at idle"
    );

    println!(
        "\n=== backpressure: N={N} jobs={JOBS} peak_in_flight={peak} elapsed={elapsed:?} ===\n"
    );
}

/// The handler must be slower than the producer, or `peak` never reaches
/// `jobs` and the drain goes untested. Keep the `peak == JOBS` assert below:
/// it's what makes this a saturation test instead of a throughput test.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "load test; run with --ignored"]
async fn load_clean_drain_under_saturation() {
    const N: u64 = 2_000;
    const JOBS: usize = 8;

    let _serialized = LOAD_LOCK.lock().await;

    let handler = LoadHandler::with_sleep(Duration::from_millis(1));
    let (elapsed, ok) = run_flood::<N>(JOBS, handler).await;
    let peak = PEAK_IN_FLIGHT.load(Ordering::SeqCst);

    assert!(ok);
    assert_eq!(
        peak, JOBS,
        "handlers must actually saturate the pool, else the drain is untested"
    );
    assert_eq!(
        HANDLED.load(Ordering::SeqCst),
        N,
        "a clean close must drain every queued update"
    );
    assert_eq!(
        IN_FLIGHT.load(Ordering::SeqCst),
        0,
        "the barrier must not return while handlers are still running"
    );

    println!(
        "\n=== clean drain: N={N} jobs={JOBS} peak_in_flight={peak} elapsed={elapsed:?} ===\n"
    );
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "load test; run with --ignored"]
async fn load_wedged_handler_bounded_by_ceiling() {
    const N: u64 = 1_000;
    const JOBS: usize = 8;

    let _serialized = LOAD_LOCK.lock().await;

    let handler = LoadHandler::wedging(1);

    let (elapsed, ok) = run_flood::<N>(JOBS, handler).await;
    let processed = HANDLED.load(Ordering::SeqCst);

    assert!(ok, "a wedged handler must not fail the runtime");
    assert!(
        processed >= N - JOBS as u64,
        "healthy handlers must still finish: only {processed}/{N}"
    );

    println!(
        "\n=== wedged handler: handled={processed}/{N} elapsed={elapsed:?} (ceiling-bounded) ===\n"
    );
}

/// A panicking handler skips its own cleanup, so only the pool's permit
/// release keeps it alive; a leak would starve the pool within `jobs` panics.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "load test; run with --ignored"]
async fn load_permit_accounting_survives_panic_churn() {
    const N: u64 = 50_000;
    const JOBS: usize = 8;
    const PANIC_EVERY: u64 = 10;

    let _serialized = LOAD_LOCK.lock().await;

    let handler = LoadHandler::panicking_every(PANIC_EVERY);

    let (elapsed, ok) = run_flood::<N>(JOBS, handler).await;

    let processed = HANDLED.load(Ordering::SeqCst);
    let panicked = PANICKED.load(Ordering::SeqCst);

    assert!(ok);
    assert_eq!(processed, N, "pool must survive {panicked} panics");
    assert!(
        panicked >= N / PANIC_EVERY - 1,
        "expected ~{} panics, saw {panicked}",
        N / PANIC_EVERY
    );

    println!(
        "\n=== panic churn: handled={processed}/{N} panics={panicked} elapsed={elapsed:?} ===\n"
    );
}
