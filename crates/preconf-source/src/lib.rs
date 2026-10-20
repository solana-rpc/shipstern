//! A source for Triton preconfirmations.
//!
//! A preconfirmation is the block producer's statement that a transaction went into the slot
//! under construction. It is the earliest signal, not settlement: the slot can be skipped,
//! forked out, or restarted by the builder. Handlers must be idempotent per signature.

use std::{collections::BTreeSet, time::Duration};

use async_trait::async_trait;
use futures_util::{future::Either, stream, Stream, StreamExt};
use shipstern::{
    sources::{FromConfig, SourceContext, SourceExitStatus, SourceTrait},
    Error as ShipsternError,
};
use tokio::sync::mpsc::Sender;
use tonic::{transport::Channel, Status};
use triton_preconfs_proto::preconfs::{
    bam_client::BamClient, harmonic_client::HarmonicClient, SubscribeRequest,
};
use yellowstone_grpc_proto::geyser::SubscribeUpdate;

mod config;
mod connect;
mod convert;
mod filter;
mod framer;
mod reconnect;
#[cfg(test)]
mod test_fixtures;

pub use config::{BamRegion, HarmonicRegion, PreconfConfig};
pub use connect::ConnectError;
pub use filter::FilterError;

use crate::{
    connect::TokenInterceptor,
    framer::{Counts, Feed, FeedUpdate, Framer},
    reconnect::{Item, Policy},
};

/// The Harmonic and BAM feeds as one source. Each preconfirmed transaction arrives as a
/// `SubscribeUpdateTransaction` with `bank_id` 0, a per-slot arrival `index`, and a meta
/// that carries only the outcome. The source sends no slot updates.
#[derive(Debug)]
pub struct PreconfSource {
    config: PreconfConfig,
}

impl FromConfig for PreconfSource {
    type Config = PreconfConfig;

    fn from_config(config: Self::Config) -> Self { Self { config } }
}

/// One item of the merged streams, tagged with the stream's position in the feed list.
type StreamItem = (usize, Item);

#[async_trait]
impl SourceTrait for PreconfSource {
    async fn connect(&self, ctx: SourceContext) -> Result<SourceExitStatus, ShipsternError> {
        let SourceContext { filters, tx, .. } = ctx;
        let config = &self.config;

        // A region named twice would open two streams on it and double delivery and billing.
        let feeds: Vec<Feed> = dedup(&config.harmonic_regions)
            .map(Feed::Harmonic)
            .chain(dedup(&config.bam_regions).map(Feed::Bam))
            .collect();
        if feeds.is_empty() {
            tracing::error!("Preconf source needs at least one harmonic or bam region");
            return Err(ShipsternError::ConfigError);
        }

        // The feed refuses a request it cannot serve, so name the parser before the dial.
        let transactions = filter::transactions(&filters)?;
        for parser in filter::unserved(&filters) {
            tracing::warn!(
                parser,
                "This parser has no transaction prefilter, so the preconf source gives it nothing"
            );
        }
        let vote_rules = filter::vote_rules(&filters);

        let interceptor = TokenInterceptor::new(config.x_token.as_deref())
            .inspect_err(|err| tracing::error!(err = %err, "Preconf source token is not usable"))?;
        let channel = connect::channel(&config.endpoint, Duration::from_secs(config.timeout))
            .await
            .inspect_err(|err| tracing::error!(err = %err, "Preconf source connection failed"))?;

        let policy = Policy {
            enabled: config.auto_reconnect,
            max_retries: config.reconnect_max_retries,
            subscribe_timeout: Duration::from_secs(config.timeout),
        };
        let mut framers = Vec::with_capacity(feeds.len());
        let mut streams = Vec::with_capacity(feeds.len());
        for (index, &feed) in feeds.iter().enumerate() {
            let request = filter::request(&transactions, feed.region());
            let first = subscribe(channel.clone(), interceptor.clone(), feed, request.clone())
                .await
                .map_err(|status| {
                    tracing::error!(
                        ?feed,
                        code = ?status.code(),
                        message = status.message(),
                        "Preconf source subscription failed"
                    );
                    ShipsternError::YellowstoneStatus(status)
                })?;
            framers.push(Framer::new(feed, vote_rules.clone()));
            let (channel, interceptor) = (channel.clone(), interceptor.clone());
            let again =
                move || subscribe(channel.clone(), interceptor.clone(), feed, request.clone());
            // The unfold's step future is not `Unpin`, so one box per stream lets `select_all`
            // merge them.
            streams.push(
                reconnect::resubscribing(feed, first, again, policy)
                    .map(move |item| (index, item))
                    .boxed(),
            );
        }

        let exit_status = run(stream::select_all(streams), &mut framers, &feeds, &tx).await;

        let counts = framers
            .iter()
            .fold(Counts::default(), |sum, framer| sum.add(framer.counts));
        tracing::info!(
            unknown_result = counts.unknown_result,
            vote_rule_dropped = counts.vote_rule_dropped,
            undecodable = counts.undecodable,
            out_of_frame = counts.out_of_frame,
            unknown_payload = counts.unknown_payload,
            clipped = counts.clipped,
            restarts = counts.restarts,
            reconnects = counts.reconnects,
            "Preconf source stopped"
        );

        Ok(exit_status)
    }
}

fn dedup<T: Ord + Copy>(regions: &[T]) -> impl Iterator<Item = T> {
    regions.iter().copied().collect::<BTreeSet<_>>().into_iter()
}

/// One stream per feed and region. Both feeds share the channel and the token.
async fn subscribe(
    channel: Channel,
    interceptor: TokenInterceptor,
    feed: Feed,
    request: SubscribeRequest,
) -> Result<impl Stream<Item = Result<FeedUpdate, Status>>, Status> {
    // The two clients yield different stream types, so `Either` joins them without a box.
    let stream = match feed {
        Feed::Harmonic(_) => Either::Left(
            HarmonicClient::with_interceptor(channel, interceptor)
                .subscribe(request)
                .await?
                .into_inner()
                .map(|item| item.map(FeedUpdate::Harmonic)),
        ),
        Feed::Bam(_) => Either::Right(
            BamClient::with_interceptor(channel, interceptor)
                .subscribe(request)
                .await?
                .into_inner()
                .map(|item| item.map(FeedUpdate::Bam)),
        ),
    };
    Ok(stream)
}

/// Pump the merged streams into the runtime until one stream ends or fails for good.
async fn run(
    mut merged: impl Stream<Item = StreamItem> + Unpin,
    framers: &mut [Framer],
    feeds: &[Feed],
    tx: &Sender<Result<SubscribeUpdate, Status>>,
) -> SourceExitStatus {
    loop {
        // While every stream sleeps in a backoff no send happens, so watch the receiver too.
        let next = tokio::select! {
            biased;
            () = tx.closed() => None,
            next = merged.next() => next,
        };
        let Some((index, item)) = next else {
            if tx.is_closed() {
                tracing::info!("Receiver dropped, stopping source");
                break SourceExitStatus::ReceiverDropped;
            }
            tracing::info!("Source returned None, exiting");
            break SourceExitStatus::StreamEnded;
        };
        let (Some(framer), Some(&feed)) = (framers.get_mut(index), feeds.get(index)) else {
            break SourceExitStatus::Error(format!("no framer for stream {index}"));
        };
        match item {
            Item::Ended => {
                // One stream gone would leave a partial feed with no signal, so stop instead.
                tracing::info!(?feed, "A stream ended, exiting");
                break SourceExitStatus::StreamEnded;
            },
            Item::Reconnected => {
                tracing::info!(?feed, "A stream subscribed again");
                framer.reconnected();
            },
            Item::Update(Ok(update)) => {
                let Some(update) = framer.apply(update) else {
                    continue;
                };
                if tx.send(Ok(update)).await.is_err() {
                    tracing::info!("Receiver dropped, stopping source");
                    break SourceExitStatus::ReceiverDropped;
                }
            },
            Item::Update(Err(status)) => {
                tracing::error!(?feed, code = ?status.code(), "A stream failed, exiting");
                let _ = tx.send(Err(status.clone())).await;
                break SourceExitStatus::StreamError {
                    code: status.code(),
                    message: status.message().to_string(),
                };
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, time::Duration};

    use futures_util::{stream, StreamExt};
    use shipstern::sources::{FromConfig, SourceContext, SourceExitStatus, SourceTrait};
    use shipstern_core::{Filters, Prefilter, Pubkey, TransactionPrefilter};
    use tokio::sync::mpsc;
    use tonic::{Code, Status};
    use triton_preconfs_proto::preconfs::{
        bam_update, harmonic_update, BamTransaction, BamUpdate, ExecutionResult, HarmonicSlotStart,
        HarmonicTransaction, HarmonicUpdate,
    };
    use yellowstone_grpc_proto::geyser::{subscribe_update::UpdateOneof, SubscribeUpdate};

    use super::{
        run, BamRegion, ConnectError, Feed, FeedUpdate, FilterError, Framer, HarmonicRegion, Item,
        PreconfConfig, PreconfSource, StreamItem,
    };
    use crate::test_fixtures::{fixture, LEGACY};

    fn config(harmonic_regions: Vec<HarmonicRegion>, bam_regions: Vec<BamRegion>) -> PreconfConfig {
        PreconfConfig {
            endpoint: "http://127.0.0.1:9".to_string(),
            x_token: None,
            harmonic_regions,
            bam_regions,
            timeout: 1,
            auto_reconnect: true,
            reconnect_max_retries: None,
        }
    }

    fn filters() -> Filters {
        Filters::new(HashMap::from([("p".to_string(), Prefilter {
            transaction: Some(TransactionPrefilter {
                accounts_include: [Pubkey::new([1; 32])].into(),
                ..Default::default()
            }),
            ..Default::default()
        })]))
    }

    fn slot_start(slot: u64) -> FeedUpdate {
        FeedUpdate::Harmonic(HarmonicUpdate {
            filters: vec![],
            payload: Some(harmonic_update::Payload::SlotStart(HarmonicSlotStart {
                slot,
                ..Default::default()
            })),
        })
    }

    fn harmonic_transaction(slot: u64) -> FeedUpdate {
        let (bytes, _) = fixture(LEGACY);
        FeedUpdate::Harmonic(HarmonicUpdate {
            filters: vec!["p".to_string()],
            payload: Some(harmonic_update::Payload::Transaction(HarmonicTransaction {
                transaction: bytes.into(),
                slot,
                result: Some(ExecutionResult::Success.into()),
                ..Default::default()
            })),
        })
    }

    fn bam_transaction(slot: u64) -> FeedUpdate {
        let (bytes, _) = fixture(LEGACY);
        FeedUpdate::Bam(BamUpdate {
            filters: vec!["p".to_string()],
            payload: Some(bam_update::Payload::Transaction(BamTransaction {
                transaction: bytes.into(),
                slot,
                result: Some(ExecutionResult::Success.into()),
                ..Default::default()
            })),
        })
    }

    async fn connect(
        source: PreconfSource,
        filters: Filters,
    ) -> Result<SourceExitStatus, shipstern::Error> {
        let (tx, mut rx) = mpsc::channel(1);
        let result = source.connect(SourceContext::new(filters, tx)).await;
        assert!(rx.try_recv().is_err());
        result
    }

    /// A refused dial is an error, not a panic.
    #[tokio::test]
    async fn connect_reports_connection_errors_instead_of_panicking() {
        let source = PreconfSource::from_config(config(vec![HarmonicRegion::Fra], vec![]));

        let result = connect(source, filters()).await;

        let Err(shipstern::Error::Other(err)) = result else {
            panic!("expected a connect error, got {result:?}");
        };
        assert!(err.is::<ConnectError>(), "{err}");
    }

    /// A BAM-only configuration reaches the dial: the feeds are peers.
    #[tokio::test]
    async fn bam_regions_alone_are_enough() {
        let source = PreconfSource::from_config(config(vec![], vec![BamRegion::Fra]));

        let result = connect(source, filters()).await;

        let Err(shipstern::Error::Other(err)) = result else {
            panic!("expected a connect error, got {result:?}");
        };
        assert!(err.is::<ConnectError>(), "{err}");
    }

    /// A prefilter the feed cannot express is refused before any dial.
    #[tokio::test]
    async fn filters_are_checked_before_the_dial() {
        let source = PreconfSource::from_config(config(vec![HarmonicRegion::Fra], vec![]));

        let result = connect(source, Filters::new(HashMap::new())).await;

        let Err(shipstern::Error::Other(err)) = result else {
            panic!("expected a filter error, got {result:?}");
        };
        assert!(err.is::<FilterError>(), "{err}");
    }

    /// No region on either feed means no stream, which would end the source at once with no
    /// error.
    #[tokio::test]
    async fn a_region_is_required() {
        let source = PreconfSource::from_config(config(vec![], vec![]));

        let result = connect(source, filters()).await;

        assert!(matches!(result, Err(shipstern::Error::ConfigError)));
    }

    /// One stream that ends stops the source, so a partial feed never runs on unseen.
    #[tokio::test]
    async fn a_stream_that_ends_stops_the_source() {
        let ended = stream::iter([
            (0, Item::Update(Ok(harmonic_transaction(7)))),
            (0, Item::Ended),
        ]);
        let live = stream::pending::<StreamItem>();
        let feeds = [
            Feed::Harmonic(HarmonicRegion::Fra),
            Feed::Harmonic(HarmonicRegion::Ams),
        ];
        let mut framers: Vec<_> = feeds
            .iter()
            .map(|&feed| Framer::new(feed, HashMap::new()))
            .collect();
        let (tx, mut rx) = mpsc::channel(4);

        let exit = tokio::time::timeout(
            Duration::from_secs(5),
            run(
                stream::select_all([ended.boxed(), live.boxed()]),
                &mut framers,
                &feeds,
                &tx,
            ),
        )
        .await
        .expect("the source must stop when a stream ends");

        assert!(matches!(exit, SourceExitStatus::StreamEnded), "{exit:?}");
        assert!(rx.try_recv().is_ok_and(|item| item.is_ok()));
    }

    /// The indexes of the delivered transactions; a forwarded status is not one.
    fn indexes(rx: &mut mpsc::Receiver<Result<SubscribeUpdate, Status>>) -> Vec<u64> {
        std::iter::from_fn(|| rx.try_recv().ok())
            .filter_map(|item| item.ok().map(|update| update.update_oneof))
            .map(|update| match update {
                Some(UpdateOneof::Transaction(tx)) => tx.transaction.unwrap().index,
                other => panic!("{other:?}"),
            })
            .collect()
    }

    /// A resubscribed Harmonic stream joins at the next slot start, so the old open slot is
    /// gone and the new slot counts from zero.
    #[tokio::test]
    async fn a_resubscribe_drops_the_open_slots() {
        let merged = stream::iter([
            (0, Item::Update(Ok(slot_start(10)))),
            (0, Item::Update(Ok(harmonic_transaction(10)))),
            (0, Item::Reconnected),
            (0, Item::Update(Ok(slot_start(11)))),
            (0, Item::Update(Ok(harmonic_transaction(11)))),
            (0, Item::Ended),
        ]);
        let feeds = [Feed::Harmonic(HarmonicRegion::Fra)];
        let mut framers = vec![Framer::new(feeds[0], HashMap::new())];
        let (tx, mut rx) = mpsc::channel(4);

        let exit = run(merged, &mut framers, &feeds, &tx).await;

        assert!(matches!(exit, SourceExitStatus::StreamEnded), "{exit:?}");
        assert_eq!(indexes(&mut rx), [0, 0]);
        assert_eq!(framers[0].open_slots(), [11]);
        assert_eq!(framers[0].counts.reconnects, 1);
        assert_eq!(framers[0].counts.out_of_frame, 0);
    }

    /// A resubscribed BAM stream resumes the slot in progress, so its index keeps counting.
    #[tokio::test]
    async fn a_bam_resubscribe_keeps_the_slot_index() {
        let merged = stream::iter([
            (0, Item::Update(Ok(bam_transaction(10)))),
            (0, Item::Reconnected),
            (0, Item::Update(Ok(bam_transaction(10)))),
            (0, Item::Ended),
        ]);
        let feeds = [Feed::Bam(BamRegion::Fra)];
        let mut framers = vec![Framer::new(feeds[0], HashMap::new())];
        let (tx, mut rx) = mpsc::channel(4);

        run(merged, &mut framers, &feeds, &tx).await;

        assert_eq!(indexes(&mut rx), [0, 1]);
        assert_eq!(framers[0].counts.reconnects, 1);
    }

    /// One stream that subscribes again does not stop the run: the other stream keeps
    /// delivering until a stream fails for good.
    #[tokio::test]
    async fn a_reconnected_stream_keeps_the_run_going() {
        let merged = stream::iter([
            (0, Item::Update(Ok(harmonic_transaction(7)))),
            (1, Item::Reconnected),
            (0, Item::Update(Ok(harmonic_transaction(8)))),
            (1, Item::Update(Err(Status::permission_denied("off")))),
        ]);
        let feeds = [
            Feed::Harmonic(HarmonicRegion::Fra),
            Feed::Bam(BamRegion::Fra),
        ];
        let mut framers: Vec<_> = feeds
            .iter()
            .map(|&feed| Framer::new(feed, HashMap::new()))
            .collect();
        let (tx, mut rx) = mpsc::channel(4);

        let exit = run(merged, &mut framers, &feeds, &tx).await;

        assert!(
            matches!(exit, SourceExitStatus::StreamError {
                code: Code::PermissionDenied,
                ..
            }),
            "{exit:?}"
        );
        assert_eq!(indexes(&mut rx).len(), 2);
        assert_eq!(framers[1].counts.reconnects, 1);
    }

    /// A dropped receiver ends the run at once, even while every stream sleeps in a backoff.
    #[tokio::test]
    async fn a_dropped_receiver_ends_a_sleeping_run() {
        let feeds = [Feed::Harmonic(HarmonicRegion::Fra)];
        let mut framers = vec![Framer::new(feeds[0], HashMap::new())];
        let (tx, rx) = mpsc::channel(1);
        drop(rx);

        let exit = tokio::time::timeout(
            Duration::from_secs(5),
            run(stream::pending::<StreamItem>(), &mut framers, &feeds, &tx),
        )
        .await
        .expect("the run must notice the dropped receiver");

        assert!(
            matches!(exit, SourceExitStatus::ReceiverDropped),
            "{exit:?}"
        );
    }

    /// A failed stream of either feed stops the source and forwards the status, so a BAM outage
    /// is never swallowed.
    #[tokio::test]
    async fn a_stream_that_fails_stops_the_source() {
        let merged = stream::iter([
            (0, Item::Update(Ok(harmonic_transaction(7)))),
            (1, Item::Update(Err(Status::unavailable("maintenance")))),
            (0, Item::Update(Ok(harmonic_transaction(8)))),
        ]);
        let feeds = [
            Feed::Harmonic(HarmonicRegion::Fra),
            Feed::Bam(BamRegion::Fra),
        ];
        let mut framers: Vec<_> = feeds
            .iter()
            .map(|&feed| Framer::new(feed, HashMap::new()))
            .collect();
        let (tx, mut rx) = mpsc::channel(4);

        let exit = run(merged, &mut framers, &feeds, &tx).await;

        assert!(
            matches!(exit, SourceExitStatus::StreamError {
                code: Code::Unavailable,
                ..
            }),
            "{exit:?}"
        );
        assert!(rx.try_recv().is_ok_and(|item| item.is_ok()));
        assert!(rx.try_recv().is_ok_and(|item| item.is_err()));
        assert!(rx.try_recv().is_err());
    }
}
