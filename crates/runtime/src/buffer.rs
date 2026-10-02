use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

use tokio::sync::{mpsc::Receiver, OwnedSemaphorePermit, Semaphore};
use tracing::{warn, Instrument};
use yellowstone_grpc_proto::{
    geyser::{subscribe_update::UpdateOneof, SubscribeUpdate, SubscribeUpdatePing},
    tonic::Status,
};

#[cfg(feature = "prometheus")]
use crate::metrics;
use crate::{
    config::BufferConfig,
    handler::PipelineSets,
    stop::{self, StopCode, StopTx},
};

/// Cap on the clean-close drain, so a stuck handler can't hold shutdown open
/// forever.
const SHUTDOWN_DRAIN_CEILING: Duration = Duration::from_secs(30);

/// The ceiling `run_yellowstone` actually uses. Short under test so the
/// wedged-handler case doesn't burn 30s: it can't use a paused clock there,
/// since the bridge thread races the auto-advance.
#[cfg(not(test))]
const ACTIVE_DRAIN_CEILING: Duration = SHUTDOWN_DRAIN_CEILING;
#[cfg(test)]
const ACTIVE_DRAIN_CEILING: Duration = Duration::from_millis(200);

type TaskHandle = tokio::task::JoinHandle<Result<StopCode, crate::Error>>;
pub struct Buffer(TaskHandle, StopTx);

impl Buffer {
    pub async fn join(self) -> Result<StopCode, crate::Error> {
        self.1.maybe_send();
        self.0
            .await
            .map_err(|e| std::io::Error::from(e).into())
            .and_then(std::convert::identity)
    }

    pub async fn wait_for_stop(&mut self) -> Result<(), crate::Error> {
        // Potential SubscribeUpdate errors are already converted to `crate::Error::YellowstoneStatus` errors
        let result = match (&mut self.0).await {
            Ok(update_result) => update_result,
            Err(e) => return Err(crate::Error::Io(std::io::Error::from(e))),
        };

        match result {
            Ok(_stop_code) => Ok(()),
            Err(e) => Err(e),
        }
    }
}

struct Job(tracing::Span, SubscribeUpdate);

/// Caller must have already dropped `tx`, or `jobs` permits never all free up.
async fn drain_in_flight(sem: &Semaphore, jobs: usize, ceiling: Duration) {
    let all_permits = u32::try_from(jobs).unwrap_or(u32::MAX);

    if tokio::time::timeout(ceiling, sem.acquire_many(all_permits))
        .await
        .is_err()
    {
        // Lower bound: a straggler can finish before this read. Diagnostic only.
        let in_flight = jobs.saturating_sub(sem.available_permits());
        warn!(
            timeout_secs = ceiling.as_secs(),
            in_flight, "Buffer drain timed out; abandoning in-flight handlers"
        );
    }
}

async fn run_job(pipelines: &PipelineSets, job: Job) {
    let Job(
        span,
        SubscribeUpdate {
            filters,
            update_oneof,
            created_at: _,
        },
    ) = job;
    let Some(update) = update_oneof else { return };

    #[cfg(feature = "prometheus")]
    let update_type = metrics::UpdateType::from(&update);

    match update {
        UpdateOneof::Account(a) => {
            pipelines
                .account
                .get_handlers(&filters)
                .run(
                    span,
                    &a,
                    #[cfg(feature = "prometheus")]
                    update_type,
                )
                .instrument(tracing::info_span!("shipstern.process.account"))
                .await;
        },
        UpdateOneof::Transaction(t) => {
            let transaction_fut = pipelines.transaction.get_handlers(&filters).run(
                span.clone(),
                &t,
                #[cfg(feature = "prometheus")]
                update_type,
            );

            let instruction_fut = pipelines.instruction.get_handlers(&filters).run(
                span,
                &t,
                #[cfg(feature = "prometheus")]
                update_type,
            );

            futures_util::future::join_all([transaction_fut, instruction_fut])
                .instrument(tracing::info_span!("shipstern.process.transaction"))
                .await;
        },
        UpdateOneof::BlockMeta(b) => {
            pipelines
                .block_meta
                .get_handlers(&filters)
                .run(
                    span,
                    &b,
                    #[cfg(feature = "prometheus")]
                    update_type,
                )
                .instrument(tracing::info_span!("shipstern.process.block_meta"))
                .await;
        },
        UpdateOneof::Block(b) => {
            pipelines
                .block
                .get_handlers(&filters)
                .run(
                    span,
                    &b,
                    #[cfg(feature = "prometheus")]
                    update_type,
                )
                .instrument(tracing::info_span!("shipstern.process.block"))
                .await;
        },
        UpdateOneof::Slot(s) => {
            pipelines
                .slot
                .get_handlers(&filters)
                .run(
                    span,
                    &s,
                    #[cfg(feature = "prometheus")]
                    update_type,
                )
                .instrument(tracing::info_span!("shipstern.process.slot"))
                .await;
        },
        UpdateOneof::Ping(SubscribeUpdatePing {}) => (),
        var => warn!(?var, "Unknown update variant"),
    }
}

impl Buffer {
    /// Must be called from within a Tokio runtime: the bridge thread captures
    /// the current handle and panics without one.
    #[allow(clippy::large_enum_variant)]
    pub fn run_yellowstone(
        config: BufferConfig,
        mut stream: Receiver<Result<SubscribeUpdate, Status>>,
        pipelines: PipelineSets,
    ) -> Self {
        let BufferConfig {
            jobs,
            sources_channel_size: _,
        } = config;
        // Config-supplied, so clamp both ends: 0 permits parks the producer
        // forever, and `Semaphore::new` panics above `MAX_PERMITS`.
        let jobs = jobs
            .unwrap_or_else(|| {
                std::thread::available_parallelism()
                    .map(std::num::NonZeroUsize::get)
                    .unwrap_or(1)
            })
            .clamp(1, Semaphore::MAX_PERMITS);

        let pipelines = Arc::new(pipelines);

        // Unbounded is safe because the permit gates the send: a bounded
        // crossbeam `send` would block the producer's executor worker.
        let (tx, rx) = crossbeam_channel::unbounded::<(OwnedSemaphorePermit, Job)>();
        let sem = Arc::new(Semaphore::new(jobs));
        let abort = Arc::new(AtomicBool::new(false));

        // Spawns each job as its own task so a handler panic stays inside it.
        let rt = tokio::runtime::Handle::current();
        let bridge = {
            let pipelines = Arc::clone(&pipelines);
            let abort = Arc::clone(&abort);

            std::thread::spawn(move || {
                while let Ok((permit, job)) = rx.recv() {
                    if abort.load(Ordering::Acquire) {
                        drop(permit);
                        continue;
                    }

                    let pipelines = Arc::clone(&pipelines);
                    rt.spawn(async move {
                        run_job(&pipelines, job).await;
                        // Moves the permit into the task so the slot is held until the job ends.
                        // Removing it breaks the bound (edge_concurrency_never_exceeds_jobs fails).
                        drop(permit);
                    });
                }
            })
        };

        let (stop_tx, mut stop_rx) = stop::channel();

        let task = tokio::task::spawn(async move {
            enum Event {
                Update(Option<Result<SubscribeUpdate, Status>>),
                Stop(StopCode),
            }

            let (result, drain) = loop {
                let event = tokio::select! {
                    u = stream.recv() => Event::Update(u),
                    c = &mut stop_rx => Event::Stop(c),
                };

                let update = match event {
                    Event::Update(Some(u)) => match u {
                        Ok(u) => u,
                        Err(e) => {
                            tracing::error!(
                                code = ?e.code(),
                                message = %e.message(),
                                "Yellowstone grpc stream error"
                            );
                            break (Err(crate::Error::YellowstoneStatus(e)), false);
                        },
                    },
                    Event::Update(None) => {
                        tracing::info!("Update stream closed");
                        break (Ok(StopCode::default()), true);
                    },
                    Event::Stop(c) => break (Ok(c), false),
                };

                // Entered to capture the fields, then exited to a Send span
                // that survives the await below.
                let span = tracing::trace_span!("process_update", ?update).entered();

                #[cfg(feature = "prometheus")]
                if let Some(update_oneof) = update.update_oneof.as_ref() {
                    let update_type = metrics::UpdateType::from(update_oneof);
                    metrics::increment_received_updates(update_type);
                }

                let span = span.exit();

                // Permit before enqueue, which is the backpressure. Must stay
                // the only acquire site or the drain barrier breaks silently.
                // Stop stays selected so a hung pool can still abort.
                let permit = tokio::select! {
                    p = Arc::clone(&sem).acquire_owned() => {
                        p.expect("job semaphore is never closed")
                    },
                    c = &mut stop_rx => break (Ok(c), false),
                };

                if tx.send((permit, Job(span, update))).is_err() {
                    // Bridge is gone; nothing can drain.
                    break (Ok(StopCode::default()), false);
                }
            };

            if !drain {
                // Before the drop below, so the bridge discards the remaining
                // queue instead of spawning it.
                abort.store(true, Ordering::Release);
            }

            // Sole producer, so this both stops the bridge and settles the
            // permit count the drain barrier waits on.
            drop(tx);

            // Waits on both paths: `try_run` drops the runtime once this
            // returns, cancelling anything still running mid-write.
            drain_in_flight(&sem, jobs, ACTIVE_DRAIN_CEILING).await;

            // Joined off the executor. Reported rather than ignored: a bridge
            // panic otherwise surfaces as an ordinary clean shutdown.
            match tokio::task::spawn_blocking(move || bridge.join()).await {
                Ok(Ok(())) => (),
                Ok(Err(_)) => warn!("Buffer bridge thread panicked; queued updates were dropped"),
                Err(e) => warn!(err = %e, "Failed to join buffer bridge thread"),
            }

            result
        });

        Self(task, stop_tx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Paused clock, so the 30s ceiling is instant. Calling the barrier
    /// directly keeps the bridge thread out, so nothing races the auto-advance,
    /// which is why the end-to-end wedged-handler test uses a short ceiling.
    #[tokio::test(start_paused = true)]
    async fn drain_gives_up_at_the_shipped_ceiling() {
        const JOBS: usize = 4;

        let sem = Semaphore::new(JOBS);
        let _wedged = sem.acquire().await.expect("semaphore is never closed");

        let started = tokio::time::Instant::now();
        drain_in_flight(&sem, JOBS, SHUTDOWN_DRAIN_CEILING).await;
        let waited = started.elapsed();

        assert!(
            waited >= SHUTDOWN_DRAIN_CEILING,
            "must wait the full ceiling before abandoning, waited {waited:?}"
        );
        assert_eq!(
            sem.available_permits(),
            JOBS - 1,
            "the wedged handler keeps its permit"
        );
    }

    /// Nothing in flight: the barrier must not cost the ceiling.
    #[tokio::test(start_paused = true)]
    async fn drain_returns_at_once_when_idle() {
        const JOBS: usize = 4;

        let sem = Semaphore::new(JOBS);

        let started = tokio::time::Instant::now();
        drain_in_flight(&sem, JOBS, SHUTDOWN_DRAIN_CEILING).await;

        assert!(
            started.elapsed() < SHUTDOWN_DRAIN_CEILING,
            "an idle pool must drain immediately"
        );
    }
}
