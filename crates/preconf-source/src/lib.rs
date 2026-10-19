//! A source for Triton preconfirmations.
//!
//! A preconfirmation is the block producer's statement that a transaction went into the slot
//! under construction. It is the earliest signal, not settlement: the slot can be skipped,
//! forked out, or restarted by the builder. Handlers must be idempotent per signature.

use std::{collections::BTreeSet, time::Duration};

use async_trait::async_trait;
use futures_util::{future, stream, Stream, StreamExt};
use shipstern::{
    sources::{FromConfig, SourceContext, SourceExitStatus, SourceTrait},
    Error as ShipsternError,
};
use tokio::sync::mpsc::Sender;
use tonic::Status;
use triton_preconfs_proto::preconfs::{harmonic_client::HarmonicClient, HarmonicUpdate};
use yellowstone_grpc_proto::geyser::SubscribeUpdate;

mod config;
mod connect;
mod convert;
mod filter;
mod framer;
#[cfg(test)]
mod test_fixtures;

pub use config::{HarmonicRegion, PreconfConfig};
pub use connect::ConnectError;
pub use filter::FilterError;

use crate::framer::{Counts, Framer};

/// The Harmonic feed as a source. Each preconfirmed transaction arrives as a
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

/// One item of a merged region stream: the region's position in the configured list, and
/// `None` once that region's stream ended.
type RegionItem = (usize, Option<Result<HarmonicUpdate, Status>>);

#[async_trait]
impl SourceTrait for PreconfSource {
    async fn connect(&self, ctx: SourceContext) -> Result<SourceExitStatus, ShipsternError> {
        let SourceContext { filters, tx, .. } = ctx;
        let config = &self.config;

        // A region named twice would open two streams on it and double delivery and billing.
        let regions: Vec<HarmonicRegion> = config
            .harmonic_regions
            .iter()
            .copied()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        if regions.is_empty() {
            tracing::error!("Preconf source needs at least one harmonic region");
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

        let interceptor = connect::TokenInterceptor::new(config.x_token.as_deref())
            .inspect_err(|err| tracing::error!(err = %err, "Preconf source token is not usable"))?;
        let channel = connect::channel(&config.endpoint, Duration::from_secs(config.timeout))
            .await
            .inspect_err(|err| tracing::error!(err = %err, "Preconf source connection failed"))?;

        let mut framers = Vec::with_capacity(regions.len());
        let mut streams = Vec::with_capacity(regions.len());
        for (index, &region) in regions.iter().enumerate() {
            let request = filter::request(&transactions, region.into());
            let mut client = HarmonicClient::with_interceptor(channel.clone(), interceptor.clone());
            let stream = match client.subscribe(request).await {
                Ok(response) => response.into_inner(),
                Err(status) => {
                    tracing::error!(
                        ?region,
                        code = ?status.code(),
                        message = status.message(),
                        "Preconf source subscription failed"
                    );
                    return Err(ShipsternError::YellowstoneStatus(status));
                },
            };
            framers.push(Framer::new(vote_rules.clone()));
            // The end marker tells the loop which region closed; `select_all` alone hides it.
            streams.push(
                stream
                    .map(move |item| (index, Some(item)))
                    .chain(stream::once(future::ready((index, None)))),
            );
        }

        let exit_status = run(stream::select_all(streams), &mut framers, &regions, &tx).await;

        let counts = framers
            .iter()
            .fold(Counts::default(), |sum, framer| sum.add(framer.counts));
        tracing::info!(
            unknown_result = counts.unknown_result,
            vote_rule_dropped = counts.vote_rule_dropped,
            undecodable = counts.undecodable,
            out_of_frame = counts.out_of_frame,
            clipped = counts.clipped,
            restarts = counts.restarts,
            "Preconf source stopped"
        );

        Ok(exit_status)
    }
}

/// Pump the merged region streams into the runtime until one region ends or fails.
async fn run(
    mut merged: impl Stream<Item = RegionItem> + Unpin,
    framers: &mut [Framer],
    regions: &[HarmonicRegion],
    tx: &Sender<Result<SubscribeUpdate, Status>>,
) -> SourceExitStatus {
    loop {
        let Some((index, item)) = merged.next().await else {
            tracing::info!("Source returned None, exiting");
            break SourceExitStatus::StreamEnded;
        };
        let Some(item) = item else {
            // One region gone would leave a partial feed with no signal, so stop instead.
            tracing::info!(region = ?regions.get(index), "A region stream ended, exiting");
            break SourceExitStatus::StreamEnded;
        };
        let Some(framer) = framers.get_mut(index) else {
            break SourceExitStatus::Error(format!("no framer for region stream {index}"));
        };

        match item {
            Ok(update) => {
                let Some(update) = framer.apply(update) else {
                    continue;
                };
                if tx.send(Ok(update)).await.is_err() {
                    tracing::info!("Receiver dropped, stopping source");
                    break SourceExitStatus::ReceiverDropped;
                }
            },
            Err(status) => {
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
    use triton_preconfs_proto::preconfs::{
        harmonic_update::Payload, ExecutionResult, HarmonicTransaction, HarmonicUpdate,
    };

    use super::{
        run, ConnectError, FilterError, Framer, HarmonicRegion, PreconfConfig, PreconfSource,
        RegionItem,
    };
    use crate::test_fixtures::{fixture, LEGACY};

    fn config(harmonic_regions: Vec<HarmonicRegion>) -> PreconfConfig {
        PreconfConfig {
            endpoint: "http://127.0.0.1:9".to_string(),
            x_token: None,
            harmonic_regions,
            timeout: 1,
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
        let source = PreconfSource::from_config(config(vec![HarmonicRegion::Fra]));

        let result = connect(source, filters()).await;

        let Err(shipstern::Error::Other(err)) = result else {
            panic!("expected a connect error, got {result:?}");
        };
        assert!(err.is::<ConnectError>(), "{err}");
    }

    /// A prefilter the feed cannot express is refused before any dial.
    #[tokio::test]
    async fn filters_are_checked_before_the_dial() {
        let source = PreconfSource::from_config(config(vec![HarmonicRegion::Fra]));

        let result = connect(source, Filters::new(HashMap::new())).await;

        let Err(shipstern::Error::Other(err)) = result else {
            panic!("expected a filter error, got {result:?}");
        };
        assert!(err.is::<FilterError>(), "{err}");
    }

    /// No region means no stream, which would end the source at once with no error.
    #[tokio::test]
    async fn a_region_is_required() {
        let source = PreconfSource::from_config(config(vec![]));

        let result = connect(source, filters()).await;

        assert!(matches!(result, Err(shipstern::Error::ConfigError)));
    }

    /// One region that ends stops the source, so a partial feed never runs on unseen.
    #[tokio::test]
    async fn a_region_that_ends_stops_the_source() {
        let (bytes, _) = fixture(LEGACY);
        let update = HarmonicUpdate {
            filters: vec!["p".to_string()],
            payload: Some(Payload::Transaction(HarmonicTransaction {
                transaction: bytes.into(),
                slot: 7,
                result: Some(ExecutionResult::Success as i32),
                region: "fra".to_string(),
                seq: 0,
            })),
        };
        let ended = stream::iter([(0, Some(Ok(update))), (0, None)]);
        let live = stream::pending::<RegionItem>();
        let regions = [HarmonicRegion::Fra, HarmonicRegion::Ams];
        let mut framers = vec![Framer::new(HashMap::new()), Framer::new(HashMap::new())];
        let (tx, mut rx) = mpsc::channel(4);

        let exit = tokio::time::timeout(
            Duration::from_secs(5),
            run(
                stream::select_all([ended.boxed(), live.boxed()]),
                &mut framers,
                &regions,
                &tx,
            ),
        )
        .await
        .expect("the source must stop when a region ends");

        assert!(matches!(exit, SourceExitStatus::StreamEnded), "{exit:?}");
        assert!(rx.try_recv().is_ok_and(|item| item.is_ok()));
    }
}
