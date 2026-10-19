//! The per-stream state machine: feed updates in, runtime updates out.

use std::collections::{BTreeMap, HashMap};

use triton_preconfs_proto::preconfs::{
    self, bam_update, harmonic_update, subscribe_request::Region, BamUpdate, ExecutionResult,
    HarmonicUpdate,
};
use yellowstone_grpc_proto::geyser::{
    subscribe_update::UpdateOneof, SubscribeUpdate, SubscribeUpdateTransaction,
    SubscribeUpdateTransactionInfo,
};

use crate::{
    config::{BamRegion, HarmonicRegion},
    convert,
};

/// Open slots this far behind the frontier are gone for good, so their index entries go when the
/// frontier next moves. A slot below the cutoff keeps counting until then.
const OPEN_SLOT_RETENTION: u64 = 64;

/// One stream: a feed and its region.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Feed {
    Harmonic(HarmonicRegion),
    Bam(BamRegion),
}

impl Feed {
    /// The region part of the subscribe request.
    pub(crate) fn region(self) -> Region {
        match self {
            Self::Harmonic(region) => {
                Region::HarmonicRegion(preconfs::HarmonicRegion::from(region).into())
            },
            Self::Bam(region) => Region::BamRegion(preconfs::BamRegion::from(region).into()),
        }
    }
}

/// One update from either feed.
pub(crate) enum FeedUpdate {
    Harmonic(HarmonicUpdate),
    Bam(BamUpdate),
}

/// What a stream dropped or saw, reported when the source exits.
#[derive(Default, Clone, Copy)]
pub(crate) struct Counts {
    pub unknown_result: u64,
    pub vote_rule_dropped: u64,
    pub undecodable: u64,
    pub out_of_frame: u64,
    pub unknown_payload: u64,
    pub clipped: u64,
    pub restarts: u64,
}

impl Counts {
    pub(crate) fn add(self, other: Self) -> Self {
        Self {
            unknown_result: self.unknown_result.saturating_add(other.unknown_result),
            vote_rule_dropped: self
                .vote_rule_dropped
                .saturating_add(other.vote_rule_dropped),
            undecodable: self.undecodable.saturating_add(other.undecodable),
            out_of_frame: self.out_of_frame.saturating_add(other.out_of_frame),
            unknown_payload: self.unknown_payload.saturating_add(other.unknown_payload),
            clipped: self.clipped.saturating_add(other.clipped),
            restarts: self.restarts.saturating_add(other.restarts),
        }
    }
}

/// One per stream, because each stream frames or counts its own slots.
pub(crate) struct Framer {
    feed: Feed,
    /// The parsers that asked for votes only (`true`) or no votes (`false`).
    vote_rules: HashMap<String, bool>,
    /// Each open slot and the index of its next transaction.
    open: BTreeMap<u64, u64>,
    /// The highest slot that ended on Harmonic, or that a BAM transaction named.
    frontier: Option<u64>,
    warned_unknown_result: bool,
    pub(crate) counts: Counts,
}

impl Framer {
    pub(crate) fn new(feed: Feed, vote_rules: HashMap<String, bool>) -> Self {
        Self {
            feed,
            vote_rules,
            open: BTreeMap::new(),
            frontier: None,
            warned_unknown_result: false,
            counts: Counts::default(),
        }
    }

    /// At most one runtime update per feed update; framing, pings, clips and drops give none.
    pub(crate) fn apply(&mut self, update: FeedUpdate) -> Option<SubscribeUpdate> {
        match update {
            FeedUpdate::Harmonic(update) => self.harmonic(update),
            FeedUpdate::Bam(update) => self.bam(update),
        }
    }

    fn harmonic(&mut self, update: HarmonicUpdate) -> Option<SubscribeUpdate> {
        let HarmonicUpdate { filters, payload } = update;

        match payload {
            Some(harmonic_update::Payload::Transaction(tx)) => {
                self.transaction(filters, tx.slot, tx.execution_result(), &tx.transaction)
            },
            Some(harmonic_update::Payload::SlotStart(start)) => {
                // A start at or below the frontier is a restart of an ended slot, or a late
                // start for a lower slot. Both reset that slot's index.
                if self.frontier.is_some_and(|frontier| start.slot <= frontier) {
                    self.counts.restarts = self.counts.restarts.saturating_add(1);
                    tracing::warn!(
                        slot = start.slot,
                        feed = ?self.feed,
                        "The builder restarted a slot; its earlier transactions stay delivered"
                    );
                }
                self.open.insert(start.slot, 0);
                None
            },
            Some(harmonic_update::Payload::SlotEnd(end)) => {
                self.open.remove(&end.slot);
                self.advance(end.slot);
                None
            },
            Some(harmonic_update::Payload::Clip(clip)) => self.clip(clip.transactions),
            Some(harmonic_update::Payload::Ping(_)) => None,
            None => self.unknown_payload(),
        }
    }

    fn bam(&mut self, update: BamUpdate) -> Option<SubscribeUpdate> {
        let BamUpdate { filters, payload } = update;

        match payload {
            Some(bam_update::Payload::Transaction(tx)) => {
                // These fields have no place in a `SubscribeUpdateTransaction`.
                tracing::debug!(
                    slot = tx.slot,
                    node = %tx.node,
                    sequence = tx.sequence,
                    bundle_position = tx.bundle_position,
                    revert_on_error = tx.is_revert_on_error,
                    "BAM preconf"
                );
                // BAM frames no slots: the first transaction opens one, and the newest slot
                // bounds the index map.
                self.open.entry(tx.slot).or_insert(0);
                let update =
                    self.transaction(filters, tx.slot, tx.execution_result(), &tx.transaction);
                self.advance(tx.slot);
                update
            },
            Some(bam_update::Payload::Clip(clip)) => self.clip(clip.transactions),
            Some(bam_update::Payload::Ping(_)) => None,
            None => self.unknown_payload(),
        }
    }

    /// A payload this crate does not know: the server schema moved ahead of the proto crate.
    fn unknown_payload(&mut self) -> Option<SubscribeUpdate> {
        self.counts.unknown_payload = self.counts.unknown_payload.saturating_add(1);
        tracing::debug!(feed = ?self.feed, "The feed sent a payload this crate does not know");
        None
    }

    fn clip(&mut self, withheld: u64) -> Option<SubscribeUpdate> {
        self.counts.clipped = self.counts.clipped.saturating_add(withheld);
        tracing::warn!(
            withheld,
            feed = ?self.feed,
            "The feed withheld transactions: the account is over its coverage share"
        );
        None
    }

    /// Move the frontier up and drop the index entries of slots that are gone for good.
    fn advance(&mut self, slot: u64) {
        // The cutoff moves only with the frontier, so there is nothing to prune otherwise.
        if self.frontier.is_some_and(|frontier| slot <= frontier) {
            return;
        }
        self.frontier = Some(slot);
        self.open = self
            .open
            .split_off(&slot.saturating_sub(OPEN_SLOT_RETENTION));
    }

    fn next_index(&mut self, slot: u64) -> u64 {
        if let Some(next) = self.open.get_mut(&slot) {
            let index = *next;
            *next = next.saturating_add(1);
            return index;
        }

        // Harmonic opens every slot with a start, and BAM opens it before it indexes, so a
        // transaction with no entry is a Harmonic gap.
        self.counts.out_of_frame = self.counts.out_of_frame.saturating_add(1);
        tracing::debug!(slot, feed = ?self.feed, "A preconf arrived outside its slot frame");
        self.open.insert(slot, 1);
        0
    }

    fn transaction(
        &mut self,
        mut filters: Vec<String>,
        slot: u64,
        result: Option<ExecutionResult>,
        bytes: &[u8],
    ) -> Option<SubscribeUpdate> {
        // Unset is unknown, not success. The runtime has no field for unknown.
        let Some(result) = result else {
            self.counts.unknown_result = self.counts.unknown_result.saturating_add(1);
            if !self.warned_unknown_result {
                self.warned_unknown_result = true;
                tracing::warn!(
                    slot,
                    feed = ?self.feed,
                    "The feed sent a transaction with no result; such transactions are dropped. \
                     `failed = false` filters them server-side"
                );
            }
            tracing::debug!(slot, feed = ?self.feed, "Dropping a preconf with no result");
            return None;
        };

        let decoded = match convert::decode(bytes) {
            Ok(decoded) => decoded,
            Err(err) => {
                self.counts.undecodable = self.counts.undecodable.saturating_add(1);
                tracing::warn!(
                    slot,
                    feed = ?self.feed,
                    err = %err,
                    "The feed sent a transaction that does not decode; skipping it"
                );
                return None;
            },
        };
        let is_vote = convert::is_vote(&decoded);

        filters.retain(|name| {
            self.vote_rules
                .get(name)
                .is_none_or(|&wanted| wanted == is_vote)
        });
        if filters.is_empty() {
            self.counts.vote_rule_dropped = self.counts.vote_rule_dropped.saturating_add(1);
            tracing::debug!(slot, feed = ?self.feed, is_vote, "No parser wants this preconf");
            return None;
        }

        let index = self.next_index(slot);
        let signature = decoded
            .signatures
            .first()
            .map(|signature| signature.as_ref().to_vec())
            .unwrap_or_default();
        let info = SubscribeUpdateTransactionInfo {
            signature,
            is_vote,
            transaction: Some(convert::transaction(decoded)),
            meta: Some(convert::meta(result != ExecutionResult::Success)),
            index,
        };

        Some(SubscribeUpdate {
            filters,
            update_oneof: Some(UpdateOneof::Transaction(SubscribeUpdateTransaction {
                transaction: Some(info),
                slot,
                bank_id: 0,
            })),
            // The feed sends no time, and the runtime does not read this field.
            created_at: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use shipstern_core::{Filters, Prefilter, Pubkey, TransactionPrefilter};
    use triton_preconfs_proto::preconfs::{
        bam_update, harmonic_update, BamTransaction, BamUpdate, CoverageClip, ExecutionResult,
        HarmonicSlotEnd, HarmonicSlotStart, HarmonicTransaction, HarmonicUpdate, Ping,
    };
    use yellowstone_grpc_proto::geyser::{
        subscribe_update::UpdateOneof, SubscribeUpdate, SubscribeUpdateTransaction,
    };

    use super::{Feed, FeedUpdate, Framer};
    use crate::{
        config::{BamRegion, HarmonicRegion},
        filter::vote_rules,
        test_fixtures::{fixture, vote_transaction, LEGACY},
    };

    fn framer() -> Framer { Framer::new(Feed::Harmonic(HarmonicRegion::Fra), HashMap::new()) }

    fn bam_framer() -> Framer { Framer::new(Feed::Bam(BamRegion::Fra), HashMap::new()) }

    fn transaction(slot: u64, bytes: Vec<u8>, result: Option<ExecutionResult>) -> FeedUpdate {
        FeedUpdate::Harmonic(HarmonicUpdate {
            filters: vec!["p".to_string()],
            payload: Some(harmonic_update::Payload::Transaction(HarmonicTransaction {
                transaction: bytes.into(),
                slot,
                result: result.map(|r| r as i32),
                ..Default::default()
            })),
        })
    }

    fn bam_transaction(slot: u64, bytes: Vec<u8>, result: Option<ExecutionResult>) -> FeedUpdate {
        FeedUpdate::Bam(BamUpdate {
            filters: vec!["p".to_string()],
            payload: Some(bam_update::Payload::Transaction(BamTransaction {
                transaction: bytes.into(),
                slot,
                result: result.map(|r| r as i32),
                ..Default::default()
            })),
        })
    }

    fn start(slot: u64) -> FeedUpdate {
        FeedUpdate::Harmonic(HarmonicUpdate {
            filters: vec![],
            payload: Some(harmonic_update::Payload::SlotStart(HarmonicSlotStart {
                slot,
                ..Default::default()
            })),
        })
    }

    fn end(slot: u64) -> FeedUpdate {
        FeedUpdate::Harmonic(HarmonicUpdate {
            filters: vec![],
            payload: Some(harmonic_update::Payload::SlotEnd(HarmonicSlotEnd {
                slot,
                ..Default::default()
            })),
        })
    }

    fn delivered(update: Option<SubscribeUpdate>) -> (Vec<String>, SubscribeUpdateTransaction) {
        let update = update.expect("a transaction update");
        let Some(UpdateOneof::Transaction(tx)) = update.update_oneof else {
            panic!("expected a transaction, got {:?}", update.update_oneof);
        };
        (update.filters, tx)
    }

    /// Deliver one success on the framer's own feed and return its index.
    fn index(framer: &mut Framer, slot: u64) -> u64 {
        let (bytes, _) = fixture(LEGACY);
        let result = Some(ExecutionResult::Success);
        let update = match framer.feed {
            Feed::Harmonic(_) => transaction(slot, bytes, result),
            Feed::Bam(_) => bam_transaction(slot, bytes, result),
        };
        delivered(framer.apply(update)).1.transaction.unwrap().index
    }

    /// A success maps to the fields the runtime reads, with no `err` and bank_id 0.
    #[test]
    fn a_success_is_delivered_with_its_slot_and_no_err() {
        let (bytes, _) = fixture(LEGACY);
        let mut framer = framer();

        let update = framer.apply(transaction(7, bytes, Some(ExecutionResult::Success)));

        let (filters, tx) = delivered(update);
        assert_eq!(filters, ["p"]);
        assert_eq!(tx.slot, 7);
        assert_eq!(tx.bank_id, 0);
        let info = tx.transaction.unwrap();
        assert_eq!(bs58::encode(&info.signature).into_string(), LEGACY);
        assert!(!info.is_vote);
        assert_eq!(info.index, 0);
        assert!(info.meta.unwrap().err.is_none());
        assert!(info.transaction.is_some());
    }

    /// Both failure kinds set `err`, so a sink cannot read a failed preconf as a success.
    #[test]
    fn a_failure_sets_err() {
        let (bytes, _) = fixture(LEGACY);
        let mut framer = framer();

        for result in [ExecutionResult::ExecutionFailure, ExecutionResult::FeesOnly] {
            let update = framer.apply(transaction(7, bytes.clone(), Some(result)));

            let (_, tx) = delivered(update);
            assert!(
                tx.transaction.unwrap().meta.unwrap().err.is_some(),
                "{result:?}"
            );
        }
    }

    /// An unset result is unknown, not a success, so it is dropped and counted.
    #[test]
    fn an_unknown_result_is_dropped_and_counted() {
        let (bytes, _) = fixture(LEGACY);
        let mut framer = framer();

        let update = framer.apply(transaction(7, bytes, None));

        assert!(update.is_none());
        assert_eq!(framer.counts.unknown_result, 1);
    }

    /// The index counts arrivals per slot, and a start for an ended slot is a restart.
    #[test]
    fn indexes_count_per_slot_and_a_repeated_start_is_a_restart() {
        let mut framer = framer();

        assert!(framer.apply(start(10)).is_none());
        assert_eq!(
            [
                index(&mut framer, 10),
                index(&mut framer, 10),
                index(&mut framer, 10)
            ],
            [0, 1, 2]
        );
        assert!(framer.apply(end(10)).is_none());
        assert_eq!(framer.counts.restarts, 0);

        assert!(framer.apply(start(10)).is_none());
        assert_eq!(framer.counts.restarts, 1);
        assert_eq!(index(&mut framer, 10), 0);
    }

    /// Two open slots keep separate indexes, so a late transaction never repeats an index.
    #[test]
    fn a_late_transaction_keeps_its_own_slot_index() {
        let mut framer = framer();

        framer.apply(start(103));
        assert_eq!(index(&mut framer, 103), 0);
        framer.apply(start(104));
        assert_eq!(index(&mut framer, 104), 0);

        assert_eq!(index(&mut framer, 103), 1);
        assert_eq!(index(&mut framer, 104), 1);
        assert_eq!(framer.counts.out_of_frame, 0);
    }

    /// A transaction with no open frame is still delivered, and the gap is counted.
    #[test]
    fn a_transaction_outside_a_frame_is_delivered_and_counted() {
        let mut framer = framer();

        assert_eq!(index(&mut framer, 50), 0);
        assert_eq!(index(&mut framer, 50), 1);

        assert_eq!(framer.counts.out_of_frame, 1);
    }

    /// Ended slots far behind the frontier leave the index map, so it cannot grow forever.
    #[test]
    fn stale_open_slots_are_pruned() {
        let mut framer = framer();

        framer.apply(start(10));
        framer.apply(start(500));
        framer.apply(end(500));

        assert!(framer.open.is_empty());
    }

    /// BAM has no frames, so each slot counts from 0 at its first transaction and no gap is
    /// counted.
    #[test]
    fn bam_transactions_index_per_slot_without_frames() {
        let mut framer = bam_framer();

        assert_eq!(index(&mut framer, 9), 0);
        assert_eq!(index(&mut framer, 9), 1);
        assert_eq!(index(&mut framer, 10), 0);

        assert_eq!(framer.counts.out_of_frame, 0);
    }

    /// The BAM node can leave the result unset too. Only the checked `execution_result()` sees
    /// that; prost's `result()` reads unset as a success.
    #[test]
    fn a_bam_transaction_with_no_result_is_dropped_and_counted() {
        let (bytes, _) = fixture(LEGACY);
        let mut framer = bam_framer();

        let update = framer.apply(bam_transaction(9, bytes, None));

        assert!(update.is_none());
        assert_eq!(framer.counts.unknown_result, 1);
    }

    /// Without slot ends, the newest BAM slot bounds the index map.
    #[test]
    fn bam_slots_far_behind_the_newest_are_pruned() {
        let mut framer = bam_framer();

        index(&mut framer, 10);
        index(&mut framer, 500);

        assert_eq!(framer.open.keys().copied().collect::<Vec<_>>(), [500]);
    }

    /// A slot below the cutoff keeps its count until the frontier moves, so a late BAM
    /// transaction never repeats index 0.
    #[test]
    fn a_bam_slot_below_the_cutoff_keeps_counting() {
        let mut framer = bam_framer();

        index(&mut framer, 500);

        assert_eq!([index(&mut framer, 10), index(&mut framer, 10)], [0, 1]);
    }

    /// A payload this crate does not know is counted, so a newer server schema shows up.
    #[test]
    fn an_unknown_payload_is_counted() {
        let mut framer = bam_framer();

        let update = framer.apply(FeedUpdate::Bam(BamUpdate {
            filters: vec![],
            payload: None,
        }));

        assert!(update.is_none());
        assert_eq!(framer.counts.unknown_payload, 1);
    }

    /// A clip only counts, and a ping is not an update.
    #[test]
    fn a_clip_is_counted_and_a_ping_yields_nothing() {
        let mut framer = framer();

        let clip = framer.apply(FeedUpdate::Harmonic(HarmonicUpdate {
            filters: vec![],
            payload: Some(harmonic_update::Payload::Clip(CoverageClip {
                transactions: 5,
            })),
        }));
        let ping = framer.apply(FeedUpdate::Harmonic(HarmonicUpdate {
            filters: vec![],
            payload: Some(harmonic_update::Payload::Ping(Ping {})),
        }));

        assert!(clip.is_none());
        assert!(ping.is_none());
        assert_eq!(framer.counts.clipped, 5);
    }

    /// Clip counts come from the server, so two large clips must not overflow.
    #[test]
    fn clip_counts_saturate() {
        let mut framer = framer();
        let clip = |n| {
            FeedUpdate::Harmonic(HarmonicUpdate {
                filters: vec![],
                payload: Some(harmonic_update::Payload::Clip(CoverageClip {
                    transactions: n,
                })),
            })
        };

        framer.apply(clip(u64::MAX));
        framer.apply(clip(1));

        assert_eq!(framer.counts.clipped, u64::MAX);
    }

    /// The feed has no vote flag, so each parser's `vote` wish trims the echoed names.
    #[test]
    fn vote_rules_trim_the_filter_names() {
        let parser = |vote| Prefilter {
            transaction: Some(TransactionPrefilter {
                accounts_include: [Pubkey::new([1; 32])].into(),
                vote,
                ..Default::default()
            }),
            ..Default::default()
        };
        let filters = Filters::new(HashMap::from([
            ("no-votes".to_string(), parser(Some(false))),
            ("all".to_string(), parser(None)),
            ("only-votes".to_string(), parser(Some(true))),
        ]));
        let names = vec![
            "no-votes".to_string(),
            "all".to_string(),
            "only-votes".to_string(),
        ];
        let vote = wincode::serialize(&vote_transaction()).unwrap();
        let (transfer, _) = fixture(LEGACY);
        let mut framer = Framer::new(Feed::Harmonic(HarmonicRegion::Fra), vote_rules(&filters));
        let deliver = |framer: &mut Framer, bytes: Vec<u8>, names: Vec<String>| {
            let FeedUpdate::Harmonic(mut update) =
                transaction(7, bytes, Some(ExecutionResult::Success))
            else {
                unreachable!()
            };
            update.filters = names;
            framer
                .apply(FeedUpdate::Harmonic(update))
                .map(|u| u.filters)
        };

        assert_eq!(
            deliver(&mut framer, vote.clone(), names.clone()),
            Some(vec!["all".to_string(), "only-votes".to_string()])
        );
        assert_eq!(
            deliver(&mut framer, transfer, names),
            Some(vec!["no-votes".to_string(), "all".to_string()])
        );
        assert_eq!(
            deliver(&mut framer, vote, vec!["no-votes".to_string()]),
            None
        );
        assert_eq!(framer.counts.vote_rule_dropped, 1);
    }

    /// One bad message must not stop every consumer of a shared feed: skip and count it.
    #[test]
    fn a_transaction_that_does_not_decode_is_skipped_and_counted() {
        let mut framer = framer();

        let update = framer.apply(transaction(
            7,
            vec![1, 2, 3],
            Some(ExecutionResult::Success),
        ));

        assert!(update.is_none());
        assert_eq!(framer.counts.undecodable, 1);
        assert_eq!(index(&mut framer, 7), 0);
    }
}
