//! Resubscribe one feed stream after a retryable failure.

use std::{future::Future, time::Duration};

use futures_util::{stream, Stream, StreamExt};
use tokio::time::Instant;
use tonic::{Code, Status};

use crate::framer::{Feed, FeedUpdate};

const INITIAL_WAIT: Duration = Duration::from_millis(100);
const MAX_WAIT: Duration = Duration::from_secs(10);
/// A stream that stayed up this long was healthy, so its drop starts the schedule over.
const STABLE: Duration = Duration::from_secs(60);

/// When and how often a stream is resubscribed.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Policy {
    pub enabled: bool,
    /// Subscribes in a row, since the last transaction, slot start or 60 s stream, before the
    /// stream gives up; `None` never gives up.
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
}

/// The wait before attempt number `attempt` (from 1): 100 ms, doubled each time, at most 10 s.
fn wait(attempt: u32) -> Duration {
    INITIAL_WAIT
        .saturating_mul(2u32.saturating_pow(attempt.saturating_sub(1)))
        .min(MAX_WAIT)
}

/// One subscribe, bounded, so a request that never answers fails instead of hanging.
pub(crate) async fn bounded<S>(
    timeout: Duration,
    subscribe: impl Future<Output = Result<S, Status>>,
) -> Result<S, Status> {
    tokio::time::timeout(timeout, subscribe)
        .await
        .unwrap_or_else(|_| Err(Status::deadline_exceeded("subscribe did not answer")))
}

/// The set the Triton client retries (preconfs-client 0.4.1 reconnect.rs). A restarting or
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

/// What one stream yields: an update, a new subscribe that succeeded, or the end. An `Err`
/// update is final, and nothing comes after it.
pub(crate) enum Item {
    Update(Result<FeedUpdate, Status>),
    Reconnected,
    Ended,
}

struct Resubscriber<S, F> {
    feed: Feed,
    stream: Option<S>,
    open_stream: F,
    policy: Policy,
    attempt: u32,
    done: bool,
    /// When the current stream subscribed.
    since: Instant,
}

/// Wrap `first` so that a clean end or a retryable failure gets a new subscribe, which yields
/// [`Item::Reconnected`] once it succeeds. The first subscribe is the caller's: its failure is
/// never retried.
pub(crate) fn resubscribing<S, F, Fut>(
    feed: Feed,
    first: S,
    open_stream: F,
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
        open_stream,
        policy,
        attempt: 0,
        done: false,
        since: Instant::now(),
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
            let status = match self.stream.as_mut() {
                None => match self.resubscribe().await {
                    Ok(()) => return Some((Item::Reconnected, self)),
                    Err(status) => Some(status),
                },
                Some(stream) => match stream.next().await {
                    Some(Ok(update)) => {
                        if update.is_progress() {
                            self.attempt = 0;
                        }
                        return Some((Item::Update(Ok(update)), self));
                    },
                    Some(Err(status)) => Some(status),
                    None => None,
                },
            };
            if let Some(item) = self.failed(status) {
                return Some((item, self));
            }
        }
    }

    /// Wait out the backoff, then subscribe once, bounded by the subscribe timeout.
    async fn resubscribe(&mut self) -> Result<(), Status> {
        tokio::time::sleep(wait(self.attempt)).await;
        let stream = bounded(self.policy.subscribe_timeout, (self.open_stream)()).await?;
        self.stream = Some(stream);
        self.since = Instant::now();
        Ok(())
    }

    /// Schedule the next subscribe and return nothing, or return the final item when the policy
    /// refuses one.
    fn failed(&mut self, status: Option<Status>) -> Option<Item> {
        if self.stream.is_some() && self.since.elapsed() >= STABLE {
            self.attempt = 0;
        }
        let attempt = self.attempt.saturating_add(1);
        let code = status.as_ref().map(Status::code);
        if self.policy.retries(attempt, code) {
            self.attempt = attempt;
            self.stream = None;
            tracing::warn!(
                feed = ?self.feed,
                ?code,
                message = status.as_ref().map_or("", Status::message),
                attempt,
                wait = ?wait(attempt),
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
    use triton_preconfs_proto::preconfs::{harmonic_update, HarmonicUpdate, Ping};

    use super::{resubscribing, wait, Item, Policy, STABLE};
    use crate::{
        framer::{Feed, FeedUpdate},
        test_fixtures::start,
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

    fn ping() -> Result<FeedUpdate, Status> { harmonic(harmonic_update::Payload::Ping(Ping {})) }

    fn down() -> Result<FeedUpdate, Status> { Err(Status::unavailable("down")) }

    fn feed(items: Vec<Result<FeedUpdate, Status>>) -> Stub { stream::iter(items) }

    /// Each subscribe pops the next prepared stream; after the last one the token is refused.
    fn subscribes<S>(
        streams: Vec<Result<S, Status>>,
    ) -> impl FnMut() -> std::future::Ready<Result<S, Status>> {
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
        let subscribe = subscribes(vec![Ok(feed(vec![Ok(start(0))]))]);

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
        let subscribe = subscribes(vec![Ok(feed(vec![Ok(start(0))]))]);

        let items = collect(
            feed(vec![Err(Status::permission_denied("off"))]),
            subscribe,
            ALWAYS,
        )
        .await;

        assert_eq!(items.len(), 1);
        assert!(is_err(&items[0].0, Code::PermissionDenied));
    }

    /// With reconnect off, the first failure is final.
    #[tokio::test(start_paused = true)]
    async fn a_disabled_policy_never_resubscribes() {
        let subscribe = subscribes(vec![Ok(feed(vec![Ok(start(0))]))]);
        let policy = Policy {
            enabled: false,
            ..ALWAYS
        };

        let items = collect(feed(vec![down()]), subscribe, policy).await;

        assert_eq!(items.len(), 1);
        assert!(is_err(&items[0].0, Code::Unavailable));

        // A clean end is final too, so the runtime sees a hangup and not an error.
        let items = collect(feed(vec![]), subscribes(vec![]), policy).await;
        assert!(matches!(items[..], [(Item::Ended, _)]));
    }

    /// The wait doubles from 100 ms and stays at 10 s, the stream sleeps for each wait, and a
    /// bounded budget gives up after its last attempt.
    #[tokio::test(start_paused = true)]
    async fn the_wait_doubles_and_caps() {
        let waits: Vec<_> = (1..=9).map(wait).collect();
        let expected: Vec<_> = [100, 200, 400, 800, 1600, 3200, 6400, 10_000, 10_000]
            .into_iter()
            .map(Duration::from_millis)
            .collect();
        assert_eq!(waits, expected);
        assert_eq!(wait(u32::MAX), Duration::from_secs(10));

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
            Ok(feed(vec![Ok(start(0)), down()])),
            Ok(feed(vec![Err(Status::unauthenticated("stop"))])),
        ]);

        let items = collect(feed(vec![Ok(start(0)), down()]), subscribe, ALWAYS).await;

        assert!(matches!(items[1].0, Item::Reconnected));
        assert!(matches!(items[3].0, Item::Reconnected));
        assert_eq!(items[3].1 - items[1].1, Duration::from_millis(100));
    }

    /// A ping does not reset the attempt count, or a point of presence that drops after each
    /// ping is retried every 100 ms forever.
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

    /// A stream that stayed up a minute starts the wait over, also when it carried only pings.
    #[tokio::test(start_paused = true)]
    async fn a_stable_stream_resets_the_attempts() {
        let quiet = || {
            stream::iter([ping()])
                .chain(stream::once(async {
                    tokio::time::sleep(STABLE).await;
                    down()
                }))
                .boxed()
        };
        // A failed resubscribe does not reset the count, so the UNAVAILABLE after the last
        // drop is final (attempt 2 > max 1).
        let subscribe = subscribes(vec![
            Ok(quiet()),
            Ok(quiet()),
            Err(Status::unavailable("down")),
        ]);
        let policy = Policy {
            max_retries: Some(1),
            ..ALWAYS
        };

        let items: Vec<_> = resubscribing(
            Feed::Harmonic(HarmonicRegion::Fra),
            quiet(),
            subscribe,
            policy,
        )
        .collect()
        .await;

        let reconnects = items
            .iter()
            .filter(|item| matches!(item, Item::Reconnected))
            .count();
        assert_eq!(reconnects, 2);
        assert!(is_err(items.last().unwrap(), Code::Unavailable));
    }

    /// A subscribe that never answers counts as a failed attempt and is retried, instead of a
    /// hung stream.
    #[tokio::test(start_paused = true)]
    async fn a_silent_subscribe_times_out_and_retries() {
        let policy = Policy {
            max_retries: Some(2),
            subscribe_timeout: Duration::from_secs(3),
            ..ALWAYS
        };
        let mut calls = 0;
        let subscribe = || {
            calls += 1;
            std::future::pending::<Result<Stub, Status>>()
        };

        // Without the bound the stream never ends, so fail instead of hanging the test run.
        let items: Vec<_> = tokio::time::timeout(
            Duration::from_secs(60),
            resubscribing(
                Feed::Harmonic(HarmonicRegion::Fra),
                feed(vec![down()]),
                subscribe,
                policy,
            )
            .collect(),
        )
        .await
        .expect("the resubscribe must be bounded");

        assert_eq!(calls, 2, "a timed-out subscribe is retried");
        assert_eq!(items.len(), 1);
        assert!(is_err(&items[0], Code::DeadlineExceeded));
    }
}
