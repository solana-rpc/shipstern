//! Resubscribe one feed stream after a retryable failure.

use std::{future::Future, time::Duration};

use futures_util::{stream, Stream, StreamExt};
use tonic::{Code, Status};

use crate::framer::{Feed, FeedUpdate};

const INITIAL_WAIT: Duration = Duration::from_millis(100);
const MAX_WAIT: Duration = Duration::from_secs(10);

/// When and how often a stream is resubscribed.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Policy {
    pub enabled: bool,
    /// Consecutive failed attempts before the stream gives up; `None` never gives up.
    pub max_retries: Option<u32>,
    /// Bound on one subscribe call, so a request that never answers is retried.
    pub subscribe_timeout: Duration,
}

impl Policy {
    /// Whether failure number `attempt` (from 1) gets another subscribe.
    fn retries(self, attempt: u32, code: Option<Code>) -> bool {
        self.enabled
            && code.is_none_or(retryable)
            && self.max_retries.is_none_or(|max| attempt <= max)
    }

    /// The wait before attempt number `attempt` (from 1): 100 ms, doubled each time, at most 10 s.
    fn wait(self, attempt: u32) -> Duration {
        INITIAL_WAIT
            .saturating_mul(2u32.saturating_pow(attempt.saturating_sub(1)))
            .min(MAX_WAIT)
    }
}

/// The set the Triton client retries (preconfs-client 0.4.0 reconnect.rs). A restarting or
/// overloaded server clears with time; a refused request or token does not.
fn retryable(code: Code) -> bool {
    matches!(
        code,
        Code::Unavailable
            | Code::DataLoss
            | Code::ResourceExhausted
            | Code::Internal
            | Code::Unknown
            | Code::Aborted
            | Code::Cancelled
            | Code::DeadlineExceeded
    )
}

/// What one stream yields: an update, a new subscribe that succeeded, or the end.
pub(crate) enum Item {
    Update(Result<FeedUpdate, Status>),
    Reconnected,
    Ended,
}

struct Resubscriber<S, F> {
    feed: Feed,
    stream: Option<S>,
    subscribe: F,
    policy: Policy,
    attempt: u32,
    done: bool,
}

/// Wrap `first` so that a clean end or a retryable failure gets a new subscribe, which yields
/// [`Item::Reconnected`] once it succeeds. The first subscribe is the caller's: its failure is
/// never retried.
pub(crate) fn resubscribing<S, F, Fut>(
    feed: Feed,
    first: S,
    subscribe: F,
    policy: Policy,
) -> impl Stream<Item = Item>
where
    S: Stream<Item = Result<FeedUpdate, Status>> + Unpin,
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<S, Status>>,
{
    let state = Resubscriber {
        feed,
        stream: Some(first),
        subscribe,
        policy,
        attempt: 0,
        done: false,
    };
    stream::unfold(state, Resubscriber::next)
}

impl<S, F, Fut> Resubscriber<S, F>
where
    S: Stream<Item = Result<FeedUpdate, Status>> + Unpin,
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<S, Status>>,
{
    async fn next(mut self) -> Option<(Item, Self)> {
        if self.done {
            return None;
        }
        loop {
            let Some(stream) = self.stream.as_mut() else {
                tokio::time::sleep(self.policy.wait(self.attempt)).await;
                let subscribed =
                    tokio::time::timeout(self.policy.subscribe_timeout, (self.subscribe)())
                        .await
                        .unwrap_or_else(|_| {
                            Err(Status::deadline_exceeded("subscribe did not answer"))
                        });
                match subscribed {
                    Ok(stream) => {
                        self.stream = Some(stream);
                        return Some((Item::Reconnected, self));
                    },
                    Err(status) => {
                        if let Some(item) = self.failed(Some(status)) {
                            return Some((item, self));
                        }
                    },
                }
                continue;
            };
            let status = match stream.next().await {
                Some(Ok(update)) => {
                    if update.is_progress() {
                        self.attempt = 0;
                    }
                    return Some((Item::Update(Ok(update)), self));
                },
                Some(Err(status)) => Some(status),
                None => None,
            };
            if let Some(item) = self.failed(status) {
                return Some((item, self));
            }
        }
    }

    /// Schedule the next subscribe and return nothing, or return the final item when the policy
    /// refuses one.
    fn failed(&mut self, status: Option<Status>) -> Option<Item> {
        let attempt = self.attempt.saturating_add(1);
        let code = status.as_ref().map(Status::code);
        if self.policy.retries(attempt, code) {
            self.attempt = attempt;
            self.stream = None;
            tracing::warn!(
                feed = ?self.feed,
                ?code,
                attempt,
                wait = ?self.policy.wait(attempt),
                "A stream dropped, resubscribing"
            );
            return None;
        }

        self.done = true;
        Some(status.map_or(Item::Ended, |status| Item::Update(Err(status))))
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use futures_util::{stream, StreamExt};
    use tokio::time::Instant;
    use tonic::{Code, Status};
    use triton_preconfs_proto::preconfs::{
        harmonic_update, HarmonicSlotStart, HarmonicUpdate, Ping,
    };

    use super::{resubscribing, Item, Policy};
    use crate::{
        framer::{Feed, FeedUpdate},
        HarmonicRegion,
    };

    type Stub = stream::Iter<std::vec::IntoIter<Result<FeedUpdate, Status>>>;

    const ALWAYS: Policy = Policy {
        enabled: true,
        max_retries: None,
        subscribe_timeout: Duration::from_secs(10),
    };

    fn harmonic(payload: harmonic_update::Payload) -> Result<FeedUpdate, Status> {
        Ok(FeedUpdate::Harmonic(HarmonicUpdate {
            filters: vec![],
            payload: Some(payload),
        }))
    }

    fn slot_start() -> Result<FeedUpdate, Status> {
        harmonic(harmonic_update::Payload::SlotStart(
            HarmonicSlotStart::default(),
        ))
    }

    fn ping() -> Result<FeedUpdate, Status> { harmonic(harmonic_update::Payload::Ping(Ping {})) }

    fn down() -> Result<FeedUpdate, Status> { Err(Status::unavailable("down")) }

    fn feed(items: Vec<Result<FeedUpdate, Status>>) -> Stub { stream::iter(items) }

    /// Each subscribe pops the next prepared stream; after the last one the token is refused.
    fn subscribes(
        streams: Vec<Result<Stub, Status>>,
    ) -> impl FnMut() -> std::future::Ready<Result<Stub, Status>> {
        let mut streams = streams.into_iter();
        move || {
            std::future::ready(
                streams
                    .next()
                    .unwrap_or_else(|| Err(Status::unauthenticated("no more streams"))),
            )
        }
    }

    async fn collect(
        first: Stub,
        subscribe: impl FnMut() -> std::future::Ready<Result<Stub, Status>>,
        policy: Policy,
    ) -> Vec<(Item, Duration)> {
        let started = Instant::now();
        resubscribing(
            Feed::Harmonic(HarmonicRegion::Fra),
            first,
            subscribe,
            policy,
        )
        .map(|item| (item, started.elapsed()))
        .collect()
        .await
    }

    fn is_err(item: &Item, code: Code) -> bool {
        matches!(item, Item::Update(Err(status)) if status.code() == code)
    }

    /// A retryable failure resubscribes, and the new stream delivers.
    #[tokio::test(start_paused = true)]
    async fn a_retryable_failure_resubscribes_and_delivery_continues() {
        let subscribe = subscribes(vec![Ok(feed(vec![slot_start()]))]);

        let items = collect(feed(vec![down()]), subscribe, ALWAYS).await;

        assert!(matches!(items[0].0, Item::Reconnected));
        assert!(matches!(items[1].0, Item::Update(Ok(_))));
        // The delivering stream then ends cleanly, which is retried once more and refused.
        assert!(is_err(&items[2].0, Code::Unauthenticated));
        assert_eq!(items.len(), 3);
    }

    /// A refused request or token is final.
    #[tokio::test(start_paused = true)]
    async fn a_non_retryable_failure_ends_the_stream() {
        let subscribe = subscribes(vec![Ok(feed(vec![slot_start()]))]);

        let items = collect(
            feed(vec![Err(Status::permission_denied("off"))]),
            subscribe,
            ALWAYS,
        )
        .await;

        assert_eq!(items.len(), 1);
        assert!(is_err(&items[0].0, Code::PermissionDenied));
    }

    /// With reconnect off, the first failure is final: the behavior before reconnect existed.
    #[tokio::test(start_paused = true)]
    async fn a_disabled_policy_never_resubscribes() {
        let subscribe = subscribes(vec![Ok(feed(vec![slot_start()]))]);
        let policy = Policy {
            enabled: false,
            ..ALWAYS
        };

        let items = collect(feed(vec![down()]), subscribe, policy).await;

        assert_eq!(items.len(), 1);
        assert!(is_err(&items[0].0, Code::Unavailable));
    }

    /// The wait doubles from 100 ms and stops at 10 s, and the stream really waits that long.
    #[tokio::test(start_paused = true)]
    async fn the_wait_doubles_and_caps() {
        let waits: Vec<_> = (1..=9).map(|attempt| ALWAYS.wait(attempt)).collect();
        let expected: Vec<_> = [100, 200, 400, 800, 1600, 3200, 6400, 10_000, 10_000]
            .into_iter()
            .map(Duration::from_millis)
            .collect();
        assert_eq!(waits, expected);
        assert_eq!(ALWAYS.wait(u32::MAX), Duration::from_secs(10));

        let subscribe = subscribes((0..8).map(|_| Err(Status::unavailable("down"))).collect());
        let policy = Policy {
            max_retries: Some(8),
            ..ALWAYS
        };

        let items = collect(feed(vec![down()]), subscribe, policy).await;

        // Each failed subscribe is silent, so the only item is the final refusal, after every
        // wait summed up.
        assert_eq!(items.len(), 1);
        assert!(is_err(&items[0].0, Code::Unavailable));
        assert_eq!(
            items[0].1,
            Duration::from_millis(100 + 200 + 400 + 800 + 1600 + 3200 + 6400 + 10_000)
        );
    }

    /// A delivered slot start or transaction resets the attempt count, so a flaky stream never
    /// reaches the cap.
    #[tokio::test(start_paused = true)]
    async fn a_delivered_update_resets_the_attempts() {
        let subscribe = subscribes(vec![
            Ok(feed(vec![slot_start(), down()])),
            Ok(feed(vec![Err(Status::unauthenticated("stop"))])),
        ]);

        let items = collect(feed(vec![slot_start(), down()]), subscribe, ALWAYS).await;

        assert!(matches!(items[1].0, Item::Reconnected));
        assert!(matches!(items[3].0, Item::Reconnected));
        assert_eq!(items[3].1 - items[1].1, Duration::from_millis(100));
    }

    /// A ping does not reset the attempt count, or a point of presence that drops after each
    /// ping is retried every 100 ms for ever.
    #[tokio::test(start_paused = true)]
    async fn a_ping_does_not_reset_the_attempts() {
        let subscribe = subscribes(vec![
            Ok(feed(vec![ping(), down()])),
            Ok(feed(vec![Err(Status::unauthenticated("stop"))])),
        ]);

        let items = collect(feed(vec![ping(), down()]), subscribe, ALWAYS).await;

        assert!(matches!(items[1].0, Item::Reconnected));
        assert!(matches!(items[3].0, Item::Reconnected));
        assert_eq!(items[3].1 - items[1].1, Duration::from_millis(200));
    }

    /// A bounded budget stops after that many consecutive failed subscribes.
    #[tokio::test(start_paused = true)]
    async fn a_bounded_budget_gives_up() {
        let subscribe = subscribes((0..5).map(|_| Err(Status::unavailable("down"))).collect());
        let policy = Policy {
            max_retries: Some(2),
            ..ALWAYS
        };

        let items = collect(feed(vec![down()]), subscribe, policy).await;

        assert_eq!(items.len(), 1);
        assert!(is_err(&items[0].0, Code::Unavailable));
        assert_eq!(items[0].1, Duration::from_millis(100 + 200));
    }

    /// A subscribe that never answers counts as a failed attempt instead of a hung stream.
    #[tokio::test(start_paused = true)]
    async fn a_silent_subscribe_times_out_and_retries() {
        let policy = Policy {
            max_retries: Some(1),
            subscribe_timeout: Duration::from_secs(3),
            ..ALWAYS
        };

        let items: Vec<_> = resubscribing(
            Feed::Harmonic(HarmonicRegion::Fra),
            feed(vec![down()]),
            std::future::pending::<Result<Stub, Status>>,
            policy,
        )
        .collect()
        .await;

        assert_eq!(items.len(), 1);
        assert!(is_err(&items[0], Code::DeadlineExceeded));
    }
}
