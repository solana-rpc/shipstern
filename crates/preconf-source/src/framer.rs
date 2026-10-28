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

/// Slots this far behind the frontier are gone for good, so their index entries go when the
/// frontier next moves. 64 slots is about 13 s at 200 ms slots, far past a 4-slot leader window.
const SLOT_RETENTION: u64 = 64;
/// A hard cap on the index map, so one bad far slot from the server cannot grow it forever.
/// Normal runs hold about 64 entries, so the cap fires only after such a slot.
const MAX_FRAMES: usize = 4 * SLOT_RETENTION as usize;

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
    pub delivered: u64,
    pub unknown_result: u64,
    pub vote_rule_dropped: u64,
    pub undecodable: u64,
    pub out_of_frame: u64,
    pub clipped: u64,
    pub restarts: u64,
    pub unknown_payload: u64,
}

/// The index state of a recent slot.
#[derive(Default)]
struct Frame {
    /// The index of the slot's next transaction.
    next: u64,
    /// Between the slot's start and its end. A BAM slot is open from its first transaction.
    open: bool,
    /// The slot got an end since its last start, so a new start is a restart.
    ended: bool,
}

/// One per stream, because each stream frames or counts its own slots.
pub(crate) struct Framer {
    feed: Feed,
    /// The parsers that asked for votes only (`true`) or no votes (`false`).
    vote_rules: HashMap<String, bool>,
    /// Recent slots; an entry outlives its slot end, so a late transaction never repeats an index.
    frames: BTreeMap<u64, Frame>,
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
            frames: BTreeMap::new(),
            frontier: None,
            warned_unknown_result: false,
            counts: Counts::default(),
        }
    }

    pub(crate) fn feed(&self) -> Feed { self.feed }

    /// At most one runtime update per feed update; framing, pings, clips and drops give none.
    pub(crate) fn apply(&mut self, update: FeedUpdate) -> Option<SubscribeUpdate> {
        let update = match update {
            FeedUpdate::Harmonic(update) => self.harmonic(update),
            FeedUpdate::Bam(update) => self.bam(update),
        };

        // A bad far slot stops the pruning, so drop the oldest entries past the cap.
        while self.frames.len() > MAX_FRAMES {
            self.frames.pop_first();
        }

        update
    }

    fn harmonic(&mut self, update: HarmonicUpdate) -> Option<SubscribeUpdate> {
        let HarmonicUpdate { filters, payload } = update;

        match payload {
            Some(harmonic_update::Payload::Transaction(tx)) => {
                let update =
                    self.transaction(filters, tx.slot, tx.execution_result(), &tx.transaction);
                if let Some(signature) = signature(update.as_ref()) {
                    tracing::debug!(
                        slot = tx.slot,
                        signature = %shipstern_core::bs58::encode(signature).into_string(),
                        seq = tx.seq,
                        region = %tx.region,
                        "Harmonic preconf"
                    );
                }
                update
            },
            Some(harmonic_update::Payload::SlotStart(start)) => {
                // The proto restarts a slot with a new start after its end. A late start for a
                // lower slot that never ended is not a restart. Both keep counting the index.
                let frame = self.frames.entry(start.slot).or_default();
                if frame.ended {
                    self.counts.restarts += 1;
                    tracing::warn!(
                        slot = start.slot,
                        feed = ?self.feed,
                        "A slot started again; the feed can resend or omit earlier transactions"
                    );
                }
                frame.open = true;
                frame.ended = false;
                None
            },
            Some(harmonic_update::Payload::SlotEnd(end)) => {
                let frame = self.frames.entry(end.slot).or_default();
                frame.open = false;
                frame.ended = true;
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
                // BAM frames no slots: the first transaction opens one, and the newest slot
                // bounds the index map.
                self.frames.entry(tx.slot).or_default().open = true;
                let update =
                    self.transaction(filters, tx.slot, tx.execution_result(), &tx.transaction);
                self.advance(tx.slot);
                if let Some(signature) = signature(update.as_ref()) {
                    tracing::debug!(
                        slot = tx.slot,
                        signature = %shipstern_core::bs58::encode(signature).into_string(),
                        node = %tx.node,
                        sequence = tx.sequence,
                        bundle_position = tx.bundle_position,
                        revert_on_error = tx.is_revert_on_error,
                        "BAM preconf"
                    );
                }
                update
            },
            Some(bam_update::Payload::Clip(clip)) => self.clip(clip.transactions),
            Some(bam_update::Payload::Ping(_)) => None,
            None => self.unknown_payload(),
        }
    }

    /// A payload this crate does not know: the server schema moved ahead of the proto crate.
    fn unknown_payload(&mut self) -> Option<SubscribeUpdate> {
        self.counts.unknown_payload += 1;
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
        self.frames = self.frames.split_off(&slot.saturating_sub(SLOT_RETENTION));
    }

    fn next_index(&mut self, slot: u64) -> u64 {
        let frame = self.frames.entry(slot).or_default();
        // Harmonic opens every slot with a start, and BAM opens it before it indexes, so a
        // transaction outside an open frame is a Harmonic gap.
        if !frame.open {
            self.counts.out_of_frame += 1;
            tracing::debug!(slot, feed = ?self.feed, "A preconf arrived outside its slot frame");
        }
        let index = frame.next;
        frame.next += 1;
        index
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
            self.counts.unknown_result += 1;
            if !self.warned_unknown_result {
                self.warned_unknown_result = true;
                tracing::warn!(
                    slot,
                    feed = ?self.feed,
                    "The feed sent a transaction with no result; the source drops these. Only a \
                     prefilter with `failed: None` gets them from the feed"
                );
            }
            tracing::debug!(slot, feed = ?self.feed, "Dropping a preconf with no result");
            return None;
        };

        let decoded = match convert::decode(bytes) {
            Ok(decoded) => decoded,
            Err(err) => {
                self.counts.undecodable += 1;
                tracing::warn!(
                    slot,
                    feed = ?self.feed,
                    err = %err,
                    "The feed sent a transaction that does not decode; skipping it"
                );
                return None;
            },
        };
        // The server filters on the first signature, so a transaction with none is malformed.
        let Some(signature) = decoded.signatures.first() else {
            self.counts.undecodable += 1;
            tracing::warn!(
                slot,
                feed = ?self.feed,
                "The feed sent a transaction with no signature; skipping it"
            );
            return None;
        };
        let signature = signature.as_ref().to_vec();
        let is_vote = convert::is_vote(&decoded);

        // The proto gives every transaction the names it matched. An empty list is not a
        // vote-rule drop.
        if filters.is_empty() {
            self.counts.unknown_payload += 1;
            tracing::debug!(slot, feed = ?self.feed, "A preconf named no filter");
            return None;
        }
        filters.retain(|name| {
            self.vote_rules
                .get(name)
                .is_none_or(|&wanted| wanted == is_vote)
        });
        if filters.is_empty() {
            self.counts.vote_rule_dropped += 1;
            tracing::debug!(slot, feed = ?self.feed, is_vote, "No parser wants this preconf");
            return None;
        }

        let index = self.next_index(slot);
        let info = SubscribeUpdateTransactionInfo {
            signature,
            is_vote,
            transaction: Some(convert::transaction(decoded)),
            meta: Some(convert::meta(result != ExecutionResult::Success)),
            index,
        };

        #[allow(
            clippy::needless_update,
            reason = "proto 14 (#358) adds `bank_id`, and its default, 0, fits a feed with no bank"
        )]
        let transaction = SubscribeUpdateTransaction {
            transaction: Some(info),
            slot,
            ..Default::default()
        };
        Some(SubscribeUpdate {
            filters,
            update_oneof: Some(UpdateOneof::Transaction(transaction)),
            // The feed sends no time, and the runtime does not read this field.
            created_at: None,
        })
    }
}

/// The signature of a delivered transaction. The feed fields have no place in a
/// `SubscribeUpdateTransaction`, so each feed logs them with it at debug.
fn signature(update: Option<&SubscribeUpdate>) -> Option<&[u8]> {
    let Some(UpdateOneof::Transaction(SubscribeUpdateTransaction {
        transaction: Some(info),
        ..
    })) = update.and_then(|update| update.update_oneof.as_ref())
    else {
        return None;
    };
    Some(&info.signature)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use shipstern_core::{Filters, Prefilter, Pubkey, TransactionPrefilter};
    use triton_preconfs_proto::preconfs::{
        self, bam_update, harmonic_update, subscribe_request::Region, BamUpdate, CoverageClip,
        ExecutionResult, HarmonicUpdate, Ping,
    };
    use yellowstone_grpc_proto::geyser::{
        subscribe_update::UpdateOneof, SubscribeUpdate, SubscribeUpdateTransaction,
    };

    use super::{Feed, FeedUpdate, Framer, MAX_FRAMES};
    use crate::{
        config::{BamRegion, HarmonicRegion},
        filter::vote_rules,
        test_fixtures::{
            bam_transaction, end, fixture, start, transaction, vote_transaction, LEGACY,
        },
    };

    fn framer() -> Framer { Framer::new(Feed::Harmonic(HarmonicRegion::Fra), HashMap::new()) }

    fn bam_framer() -> Framer { Framer::new(Feed::Bam(BamRegion::Fra), HashMap::new()) }

    /// Each service rejects the other feed's region, so a feed must name its own oneof.
    #[test]
    fn each_feed_names_its_own_region() {
        assert_eq!(
            Feed::Harmonic(HarmonicRegion::Fra).region(),
            Region::HarmonicRegion(preconfs::HarmonicRegion::Fra as i32)
        );
        assert_eq!(
            Feed::Bam(BamRegion::Fra).region(),
            Region::BamRegion(preconfs::BamRegion::Fra as i32)
        );
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

    /// A success maps to the fields the runtime reads, with no `err`.
    #[test]
    fn a_success_is_delivered_with_its_slot_and_no_err() {
        let (bytes, _) = fixture(LEGACY);
        let mut framer = framer();

        let update = framer.apply(transaction(7, bytes, Some(ExecutionResult::Success)));

        let (filters, tx) = delivered(update);
        assert_eq!(filters, ["p"]);
        assert_eq!(tx.slot, 7);
        let info = tx.transaction.unwrap();
        assert_eq!(
            shipstern_core::bs58::encode(&info.signature).into_string(),
            LEGACY
        );
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

    /// An unset result is unknown, not a success, so it is dropped and counted on both feeds.
    /// Only the checked `execution_result()` sees that; prost's `result()` reads unset as a
    /// success.
    #[test]
    fn an_unknown_result_is_dropped_and_counted() {
        for mut framer in [framer(), bam_framer()] {
            let (bytes, _) = fixture(LEGACY);
            let update = match framer.feed {
                Feed::Harmonic(_) => transaction(7, bytes, None),
                Feed::Bam(_) => bam_transaction(7, bytes, None),
            };

            assert!(framer.apply(update).is_none());
            assert_eq!(framer.counts.unknown_result, 1, "{:?}", framer.feed);
        }
    }

    /// The index counts arrivals per slot. A start for an ended slot is a restart, and the index
    /// keeps counting, so it never repeats.
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
        assert_eq!(index(&mut framer, 10), 3);
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

    /// A transaction with no open frame is still delivered and counted, before the first start
    /// and after its slot's end. The index keeps counting.
    #[test]
    fn a_transaction_outside_a_frame_is_delivered_and_counted() {
        let mut framer = framer();

        assert_eq!(index(&mut framer, 50), 0);
        assert_eq!(index(&mut framer, 50), 1);
        framer.apply(start(51));
        assert_eq!(index(&mut framer, 51), 0);
        framer.apply(end(51));
        assert_eq!(index(&mut framer, 51), 1);

        assert_eq!(framer.counts.out_of_frame, 3);
    }

    /// A slot inside the window keeps counting after the frontier moves; slots far behind it
    /// leave the index map, so it cannot grow forever.
    #[test]
    fn stale_slots_are_pruned() {
        let mut framer = framer();

        framer.apply(start(10));
        assert_eq!(index(&mut framer, 10), 0);
        framer.apply(end(10));
        framer.apply(start(11));
        framer.apply(end(11));
        assert_eq!(index(&mut framer, 10), 1);

        framer.apply(start(500));
        framer.apply(end(500));

        assert_eq!(framer.frames.keys().copied().collect::<Vec<_>>(), [500]);
    }

    /// Only a start after the slot's end is a restart. A far slot end stops the pruning of the
    /// lower slots, so the cap bounds the index map.
    #[test]
    fn a_start_for_a_slot_that_never_ended_is_not_a_restart() {
        let mut framer = framer();

        framer.apply(end(1_000_000));
        for slot in 0..1_000 {
            framer.apply(start(slot));
        }

        assert_eq!(framer.counts.restarts, 0);
        assert!(framer.frames.len() <= MAX_FRAMES, "{}", framer.frames.len());
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

    /// The server filters on the first signature, so a transaction with none is malformed.
    #[test]
    fn a_transaction_with_no_signature_is_undecodable() {
        let mut framer = framer();
        let mut tx = vote_transaction();
        tx.signatures.clear();
        let bytes = wincode::serialize(&tx).expect("serialize");

        let update = framer.apply(transaction(7, bytes, Some(ExecutionResult::Success)));

        assert!(update.is_none());
        assert_eq!(framer.counts.undecodable, 1);
    }

    /// The proto gives every transaction the names it matched, so an empty list is out of
    /// contract, not a vote-rule drop.
    #[test]
    fn a_transaction_with_no_filter_names_is_an_unknown_payload() {
        let mut framer = framer();
        let (bytes, _) = fixture(LEGACY);
        let FeedUpdate::Harmonic(mut update) =
            transaction(7, bytes, Some(ExecutionResult::Success))
        else {
            panic!("expected a Harmonic update");
        };
        update.filters.clear();

        let update = framer.apply(FeedUpdate::Harmonic(update));

        assert!(update.is_none());
        assert_eq!(framer.counts.unknown_payload, 1);
        assert_eq!(framer.counts.vote_rule_dropped, 0);
    }

    /// Without slot ends, the newest BAM slot bounds the index map.
    #[test]
    fn bam_slots_far_behind_the_newest_are_pruned() {
        let mut framer = bam_framer();

        index(&mut framer, 10);
        index(&mut framer, 500);

        assert_eq!(framer.frames.keys().copied().collect::<Vec<_>>(), [500]);
    }

    /// A slot below the cutoff keeps its count until the frontier moves again.
    #[test]
    fn a_bam_slot_below_the_cutoff_keeps_counting() {
        let mut framer = bam_framer();

        index(&mut framer, 500);

        assert_eq!([index(&mut framer, 10), index(&mut framer, 10)], [0, 1]);
    }

    /// A payload this crate does not know is counted on both feeds, so a newer server schema
    /// shows up.
    #[test]
    fn an_unknown_payload_is_counted() {
        for mut framer in [framer(), bam_framer()] {
            let update = match framer.feed {
                Feed::Harmonic(_) => FeedUpdate::Harmonic(HarmonicUpdate {
                    filters: vec![],
                    payload: None,
                }),
                Feed::Bam(_) => FeedUpdate::Bam(BamUpdate {
                    filters: vec![],
                    payload: None,
                }),
            };

            assert!(framer.apply(update).is_none());
            assert_eq!(framer.counts.unknown_payload, 1, "{:?}", framer.feed);
        }
    }

    /// A clip only counts, and a ping is not an update, on both feeds.
    #[test]
    fn a_clip_is_counted_and_a_ping_yields_nothing() {
        for mut framer in [framer(), bam_framer()] {
            let clip = CoverageClip { transactions: 5 };
            let (clip, ping) = match framer.feed {
                Feed::Harmonic(_) => (
                    FeedUpdate::Harmonic(HarmonicUpdate {
                        filters: vec![],
                        payload: Some(harmonic_update::Payload::Clip(clip)),
                    }),
                    FeedUpdate::Harmonic(HarmonicUpdate {
                        filters: vec![],
                        payload: Some(harmonic_update::Payload::Ping(Ping {})),
                    }),
                ),
                Feed::Bam(_) => (
                    FeedUpdate::Bam(BamUpdate {
                        filters: vec![],
                        payload: Some(bam_update::Payload::Clip(clip)),
                    }),
                    FeedUpdate::Bam(BamUpdate {
                        filters: vec![],
                        payload: Some(bam_update::Payload::Ping(Ping {})),
                    }),
                ),
            };

            assert!(framer.apply(clip).is_none());
            assert!(framer.apply(ping).is_none());
            assert_eq!(framer.counts.clipped, 5, "{:?}", framer.feed);
        }
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
