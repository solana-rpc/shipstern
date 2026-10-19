//! A source for Triton preconfirmations.
//!
//! A preconfirmation is the block producer's statement that a transaction went into the slot
//! under construction. It is an early signal, not settlement: the slot can be skipped, forked
//! out, or restarted by its leader. It usually arrives ahead of Geyser when the client runs near
//! the feed region. Handlers must be idempotent per signature.

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
    framer::{Feed, FeedUpdate, Framer},
    reconnect::{Item, Policy},
};

/// The Harmonic and BAM feeds as one source. Each preconfirmed transaction arrives as a
/// `SubscribeUpdateTransaction` with a per-slot arrival `index` and a meta that carries only
/// the outcome. The source sends no slot updates.
///
/// ```rust, ignore
/// Runtime::builder()
///     .instruction(Pipeline::new(InstructionParser, [Handler]))
///     .build::<PreconfSource>(config);
/// ```
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
            return Err(ConnectError::NoRegion.into());
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

        let interceptor = TokenInterceptor::new(config.x_token.as_deref())?;
        let timeout = Duration::from_secs(config.timeout);
        let channel = connect::channel(&config.endpoint, config.x_token.is_some(), timeout).await?;

        let policy = Policy {
            enabled: config.auto_reconnect,
            max_retries: config.reconnect_max_retries,
            subscribe_timeout: timeout,
        };
        let mut framers = Vec::with_capacity(feeds.len());
        let mut streams = Vec::with_capacity(feeds.len());
        for (index, &feed) in feeds.iter().enumerate() {
            // One closure for the first subscribe and each resubscribe, so the two cannot drift.
            let open_stream = {
                let (channel, interceptor) = (channel.clone(), interceptor.clone());
                let request = filter::request(&transactions, feed.region());
                move || subscribe(channel.clone(), interceptor.clone(), feed, request.clone())
            };
            let first = reconnect::bounded(timeout, open_stream())
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
            tracing::info!(?feed, "Preconf source subscribed");
            framers.push(Framer::new(feed, vote_rules.clone()));
            // The unfold's step future is not `Unpin`, so one box per stream lets `select_all`
            // merge them.
            streams.push(
                reconnect::resubscribing(feed, first, open_stream, policy)
                    .map(move |item| (index, item))
                    .boxed(),
            );
        }

        let exit_status = run(stream::select_all(streams), &mut framers, &tx).await;

        // One line for each stream, so a dark or clipped stream shows with its feed and region.
        for framer in &framers {
            let counts = framer.counts;
            tracing::info!(
                feed = ?framer.feed(),
                delivered = counts.delivered,
                unknown_result = counts.unknown_result,
                vote_rule_dropped = counts.vote_rule_dropped,
                undecodable = counts.undecodable,
                out_of_frame = counts.out_of_frame,
                clipped = counts.clipped,
                restarts = counts.restarts,
                unknown_payload = counts.unknown_payload,
                reconnects = counts.reconnects,
                "Preconf stream stopped"
            );
        }

        Ok(exit_status)
    }
}

fn dedup<T: Ord + Copy>(regions: &[T]) -> impl Iterator<Item = T> {
    regions.iter().copied().collect::<BTreeSet<_>>().into_iter()
}

/// Subscribe to one feed in one region. Both feeds share the channel and the token.
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
    tx: &Sender<Result<SubscribeUpdate, Status>>,
) -> SourceExitStatus {
    // One registration for the whole run instead of one for each item.
    let closed = tx.closed();
    tokio::pin!(closed);
    loop {
        // While every stream sleeps in a backoff no send happens, so watch the receiver too.
        let next = tokio::select! {
            biased;
            () = &mut closed => {
                tracing::info!("Receiver dropped, stopping source");
                break SourceExitStatus::ReceiverDropped;
            },
            next = merged.next() => next,
        };
        let Some((index, item)) = next else {
            tracing::info!("Source returned None, exiting");
            break SourceExitStatus::StreamEnded;
        };
        let Some(framer) = framers.get_mut(index) else {
            break SourceExitStatus::Error(format!("no framer for stream {index}"));
        };
        let feed = framer.feed();
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
                    tracing::info!("Receiver dropped, stopping source; the update in hand is lost");
                    break SourceExitStatus::ReceiverDropped;
                }
                framer.counts.delivered += 1;
            },
            Item::Update(Err(status)) => {
                tracing::error!(?feed, code = ?status.code(), "A stream failed, exiting");
                let code = status.code();
                let message = status.message().to_string();
                let _ = tx.send(Err(status)).await;
                break SourceExitStatus::StreamError { code, message };
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, time::Duration};

    use futures_util::{stream, FutureExt, StreamExt};
    use shipstern::sources::{FromConfig, SourceContext, SourceExitStatus, SourceTrait};
    use shipstern_core::{Filters, Prefilter, Pubkey, TokenAccounts, TransactionPrefilter};
    use tokio::sync::mpsc;
    use tonic::{Code, Status};
    use triton_preconfs_proto::preconfs::ExecutionResult;
    use yellowstone_grpc_proto::geyser::{subscribe_update::UpdateOneof, SubscribeUpdate};

    use super::{
        run, BamRegion, ConnectError, Feed, FeedUpdate, FilterError, Framer, HarmonicRegion, Item,
        PreconfConfig, PreconfSource, StreamItem,
    };
    use crate::test_fixtures::{fixture, start, transaction, LEGACY};

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

    fn harmonic_transaction(slot: u64) -> FeedUpdate {
        transaction(slot, fixture(LEGACY).0, Some(ExecutionResult::Success))
    }

    fn framer(feed: Feed) -> Framer { Framer::new(feed, HashMap::new()) }

    async fn connect(
        source: PreconfSource,
        filters: Filters,
    ) -> Result<SourceExitStatus, shipstern::Error> {
        let (tx, mut rx) = mpsc::channel(1);
        let result = source.connect(SourceContext::new(filters, tx)).await;
        assert!(rx.try_recv().is_err());
        result
    }

    /// A refused dial is an error, not a panic, on either feed alone.
    #[tokio::test]
    async fn connect_reports_connection_errors_instead_of_panicking() {
        for config in [
            config(vec![HarmonicRegion::Fra], vec![]),
            config(vec![], vec![BamRegion::Fra]),
        ] {
            let source = PreconfSource::from_config(config);

            let result = connect(source, filters()).await;

            let Err(shipstern::Error::Other(err)) = result else {
                panic!("expected a connect error, got {result:?}");
            };
            assert!(
                matches!(err.downcast_ref(), Some(ConnectError::Transport(_))),
                "{err}"
            );
        }
    }

    /// Token-account expansion has no feed field, so the source refuses it before any dial.
    #[tokio::test]
    async fn filters_are_checked_before_the_dial() {
        let source = PreconfSource::from_config(config(vec![HarmonicRegion::Fra], vec![]));
        let filters = Filters::new(HashMap::from([("p".to_string(), Prefilter {
            transaction: Some(TransactionPrefilter {
                accounts_include: [Pubkey::new([1; 32])].into(),
                token_accounts: Some(TokenAccounts::All),
                ..Default::default()
            }),
            ..Default::default()
        })]));

        let result = connect(source, filters).await;

        let Err(shipstern::Error::Other(err)) = result else {
            panic!("expected a filter error, got {result:?}");
        };
        assert!(
            matches!(err.downcast_ref(), Some(FilterError::TokenAccounts { parser }) if parser == "p"),
            "{err}"
        );
    }

    /// A first subscribe that never answers fails with `DEADLINE_EXCEEDED` instead of a hang.
    #[tokio::test]
    async fn a_silent_subscribe_fails_the_start() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        // Take the connection and never read it, so no response comes.
        tokio::spawn(async move {
            let _held = listener.accept().await;
            std::future::pending::<()>().await;
        });
        let source = PreconfSource::from_config(PreconfConfig {
            endpoint,
            ..config(vec![HarmonicRegion::Fra], vec![])
        });

        let result = tokio::time::timeout(Duration::from_secs(5), connect(source, filters()))
            .await
            .expect("the subscribe must be bounded");

        let Err(shipstern::Error::YellowstoneStatus(status)) = result else {
            panic!("expected a status, got {result:?}");
        };
        assert_eq!(status.code(), Code::DeadlineExceeded);
    }

    /// A token without the feed stops the source with the server's code.
    #[tokio::test]
    async fn a_refused_subscribe_returns_its_code() {
        use std::{
            convert::Infallible,
            future::Ready,
            task::{Context, Poll},
        };

        use tonic::{
            body::Body,
            codegen::{http, Service},
            server::NamedService,
            transport::{server::TcpIncoming, Server},
        };

        #[derive(Clone)]
        struct Refuse;

        impl NamedService for Refuse {
            const NAME: &'static str = "preconfs.Harmonic";
        }

        impl Service<http::Request<Body>> for Refuse {
            type Error = Infallible;
            type Future = Ready<Result<Self::Response, Infallible>>;
            type Response = http::Response<Body>;

            fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Infallible>> {
                Poll::Ready(Ok(()))
            }

            // Refuse with the real code only if the token arrived.
            fn call(&mut self, request: http::Request<Body>) -> Self::Future {
                let status = match request.headers().get("x-token") {
                    Some(token) if token == "t" => Status::unauthenticated("not entitled"),
                    _ => Status::invalid_argument("no x-token"),
                };
                std::future::ready(Ok(status.into_http()))
            }
        }

        let incoming = TcpIncoming::bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let endpoint = format!("http://{}", incoming.local_addr().unwrap());
        tokio::spawn(
            Server::builder()
                .add_service(Refuse)
                .serve_with_incoming(incoming),
        );
        let source = PreconfSource::from_config(PreconfConfig {
            endpoint,
            x_token: Some("t".to_string()),
            ..config(vec![HarmonicRegion::Fra], vec![])
        });

        let result = connect(source, filters()).await;

        let Err(shipstern::Error::YellowstoneStatus(status)) = result else {
            panic!("expected the server status, got {result:?}");
        };
        assert_eq!(status.code(), Code::Unauthenticated);
    }

    /// A token that a header cannot carry fails before the dial.
    #[tokio::test]
    async fn an_invalid_token_fails_before_the_dial() {
        let source = PreconfSource::from_config(PreconfConfig {
            x_token: Some("bad\ntoken".to_string()),
            ..config(vec![HarmonicRegion::Fra], vec![])
        });

        let result = connect(source, filters()).await;

        let Err(shipstern::Error::Other(err)) = result else {
            panic!("expected a token error, got {result:?}");
        };
        assert!(
            matches!(err.downcast_ref(), Some(ConnectError::Token(_))),
            "{err}"
        );
    }

    /// tonic dials an unknown scheme in clear text and a `unix:` socket as http, so the source
    /// refuses both before the dial.
    #[tokio::test]
    async fn a_scheme_other_than_https_or_http_is_refused() {
        for (endpoint, expected) in [
            ("grpcs://127.0.0.1:9", "grpcs"),
            ("unix:///nonexistent/preconf.sock", "unix"),
        ] {
            let source = PreconfSource::from_config(PreconfConfig {
                endpoint: endpoint.to_string(),
                ..config(vec![HarmonicRegion::Fra], vec![])
            });

            let result = connect(source, filters()).await;

            let Err(shipstern::Error::Other(err)) = result else {
                panic!("expected a scheme error, got {result:?}");
            };
            assert!(
                matches!(err.downcast_ref(), Some(ConnectError::Scheme(Some(scheme))) if scheme == expected),
                "{endpoint}: {err}"
            );
        }
    }

    /// A token over http goes only to this machine, so a remote host fails before the dial.
    #[tokio::test]
    async fn a_token_over_http_to_a_remote_host_is_refused() {
        let source = PreconfSource::from_config(PreconfConfig {
            // TEST-NET-1, so a dial reaches no real host.
            endpoint: "http://192.0.2.1:9".to_string(),
            x_token: Some("t".to_string()),
            ..config(vec![HarmonicRegion::Fra], vec![])
        });

        let result = connect(source, filters()).await;

        let Err(shipstern::Error::Other(err)) = result else {
            panic!("expected a plain-text token error, got {result:?}");
        };
        assert!(
            matches!(err.downcast_ref(), Some(ConnectError::PlainTextToken)),
            "{err}"
        );
    }

    /// A token over http may go to this machine, so each loopback name reaches the dial.
    #[tokio::test]
    async fn a_token_over_http_to_loopback_is_dialed() {
        for endpoint in ["http://127.0.0.1:9", "http://localhost:9", "http://[::1]:9"] {
            let source = PreconfSource::from_config(PreconfConfig {
                endpoint: endpoint.to_string(),
                x_token: Some("t".to_string()),
                ..config(vec![HarmonicRegion::Fra], vec![])
            });

            let result = connect(source, filters()).await;

            let Err(shipstern::Error::Other(err)) = result else {
                panic!("expected a dial error, got {result:?}");
            };
            assert!(
                matches!(err.downcast_ref(), Some(ConnectError::Transport(_))),
                "{endpoint}: {err}"
            );
        }
    }

    /// No region on either feed means no stream, which would end the source at once with no
    /// error.
    #[tokio::test]
    async fn a_region_is_required() {
        let source = PreconfSource::from_config(config(vec![], vec![]));

        let result = connect(source, filters()).await;

        let Err(shipstern::Error::Other(err)) = result else {
            panic!("expected a region error, got {result:?}");
        };
        assert!(
            matches!(err.downcast_ref(), Some(ConnectError::NoRegion)),
            "{err}"
        );
    }

    /// A region named twice would double delivery and billing.
    #[test]
    fn a_region_named_twice_opens_one_stream() {
        use HarmonicRegion::{Ams, Fra};

        assert_eq!(super::dedup(&[Fra, Ams, Fra]).collect::<Vec<_>>(), [
            Ams, Fra
        ]);
    }

    /// A stream that ends for good stops the source, so the run does not continue on a partial
    /// feed.
    #[tokio::test]
    async fn a_stream_that_ends_stops_the_source() {
        let ended = stream::iter([
            (0, Item::Update(Ok(harmonic_transaction(7)))),
            (0, Item::Ended),
        ]);
        let live = stream::pending::<StreamItem>();
        let mut framers = [
            Feed::Harmonic(HarmonicRegion::Fra),
            Feed::Harmonic(HarmonicRegion::Ams),
        ]
        .map(framer);
        let (tx, mut rx) = mpsc::channel(4);

        let exit = tokio::time::timeout(
            Duration::from_secs(5),
            run(
                stream::select_all([ended.boxed(), live.boxed()]),
                &mut framers,
                &tx,
            ),
        )
        .await
        .expect("the source must stop when a stream ends for good");

        assert!(matches!(exit, SourceExitStatus::StreamEnded), "{exit:?}");
        assert!(rx.try_recv().is_ok_and(|item| item.is_ok()));
        assert_eq!(framers[0].counts.delivered, 1);
    }

    /// An update that the runtime never got is not counted as delivered.
    #[tokio::test]
    async fn a_send_to_a_dropped_receiver_is_not_delivered() {
        let mut framers = [framer(Feed::Harmonic(HarmonicRegion::Fra))];
        let (tx, rx) = mpsc::channel(1);
        // Fill the one slot, so the run waits in `send` with the update in hand.
        tx.try_send(Ok(SubscribeUpdate::default()))
            .expect("an empty channel");

        let exit = {
            let merged = stream::iter([(0, Item::Update(Ok(harmonic_transaction(7))))]);
            let running = run(merged, &mut framers, &tx);
            tokio::pin!(running);
            assert!(
                running.as_mut().now_or_never().is_none(),
                "the run must wait in send"
            );
            drop(rx);
            running.await
        };

        assert!(
            matches!(exit, SourceExitStatus::ReceiverDropped),
            "{exit:?}"
        );
        assert_eq!(framers[0].counts.delivered, 0);
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
    /// closed and the new slot counts from zero. A late transaction of the old slot keeps its
    /// count and is counted out of frame.
    #[tokio::test]
    async fn a_resubscribe_closes_the_open_slots() {
        let merged = stream::iter([
            (0, Item::Update(Ok(start(10)))),
            (0, Item::Update(Ok(harmonic_transaction(10)))),
            (0, Item::Reconnected),
            (0, Item::Update(Ok(harmonic_transaction(10)))),
            (0, Item::Update(Ok(start(11)))),
            (0, Item::Update(Ok(harmonic_transaction(11)))),
            (0, Item::Ended),
        ]);
        let mut framers = [framer(Feed::Harmonic(HarmonicRegion::Fra))];
        let (tx, mut rx) = mpsc::channel(4);

        let exit = run(merged, &mut framers, &tx).await;

        assert!(matches!(exit, SourceExitStatus::StreamEnded), "{exit:?}");
        assert_eq!(indexes(&mut rx), [0, 1, 0]);
        assert_eq!(framers[0].counts.delivered, 3);
        assert_eq!(framers[0].counts.reconnects, 1);
        assert_eq!(framers[0].counts.out_of_frame, 1);
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
        let mut framers = [
            Feed::Harmonic(HarmonicRegion::Fra),
            Feed::Bam(BamRegion::Fra),
        ]
        .map(framer);
        let (tx, mut rx) = mpsc::channel(4);

        let exit = run(merged, &mut framers, &tx).await;

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
        let mut framers = [framer(Feed::Harmonic(HarmonicRegion::Fra))];
        let (tx, rx) = mpsc::channel(1);
        drop(rx);

        let exit = tokio::time::timeout(
            Duration::from_secs(5),
            run(stream::pending::<StreamItem>(), &mut framers, &tx),
        )
        .await
        .expect("the run must notice the dropped receiver");

        assert!(
            matches!(exit, SourceExitStatus::ReceiverDropped),
            "{exit:?}"
        );
    }

    /// A stream of either feed that fails for good stops the source and forwards the status, so
    /// a BAM outage is never hidden.
    #[tokio::test]
    async fn a_stream_that_fails_stops_the_source() {
        let merged = stream::iter([
            (0, Item::Update(Ok(harmonic_transaction(7)))),
            (1, Item::Update(Err(Status::unavailable("maintenance")))),
            (0, Item::Update(Ok(harmonic_transaction(8)))),
        ]);
        let mut framers = [
            Feed::Harmonic(HarmonicRegion::Fra),
            Feed::Bam(BamRegion::Fra),
        ]
        .map(framer);
        let (tx, mut rx) = mpsc::channel(4);

        let exit = run(merged, &mut framers, &tx).await;

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
