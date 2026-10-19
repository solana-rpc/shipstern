//! The per-region state machine: feed updates in, runtime updates out.

use std::collections::{BTreeMap, HashMap};

use triton_preconfs_proto::preconfs::{
    harmonic_update::Payload, ExecutionResult, HarmonicTransaction, HarmonicUpdate,
};
use yellowstone_grpc_proto::geyser::{
    subscribe_update::UpdateOneof, SubscribeUpdate, SubscribeUpdateTransaction,
    SubscribeUpdateTransactionInfo,
};

use crate::convert;

/// Slots older than this are gone for good, so their index entries can go. 64 slots is about
/// 13 s at 200 ms slots, far past a 4-slot leader window.
const SLOT_RETENTION: u64 = 64;
/// A hard cap on the index map, so bad slots from the server cannot grow it forever. Normal runs
/// hold about 64 entries, so the cap fires only after such slots.
const MAX_FRAMES: usize = 4 * SLOT_RETENTION as usize;

/// What a region stream dropped or saw, reported when the source exits.
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
    /// Between the slot's start and its end.
    open: bool,
    /// The slot got an end since its last start, so a new start is a restart.
    ended: bool,
}

/// One per region stream, because the feed frames slots per region.
pub(crate) struct Framer {
    /// The parsers that asked for votes only (`true`) or no votes (`false`).
    vote_rules: HashMap<String, bool>,
    /// Recent slots; an entry outlives its slot end, so a late transaction never repeats an index.
    frames: BTreeMap<u64, Frame>,
    /// The highest slot that ended.
    ended: Option<u64>,
    warned_unknown_result: bool,
    pub(crate) counts: Counts,
}

impl Framer {
    pub(crate) fn new(vote_rules: HashMap<String, bool>) -> Self {
        Self {
            vote_rules,
            frames: BTreeMap::new(),
            ended: None,
            warned_unknown_result: false,
            counts: Counts::default(),
        }
    }

    /// At most one runtime update per feed update; framing, pings, clips and drops give none.
    pub(crate) fn apply(&mut self, update: HarmonicUpdate) -> Option<SubscribeUpdate> {
        let HarmonicUpdate { filters, payload } = update;

        let update = match payload {
            Some(Payload::Transaction(tx)) => self.transaction(filters, tx),
            Some(Payload::SlotStart(start)) => {
                // The proto restarts a slot with a new start after its end. A late start for a
                // lower slot that never ended is not a restart. Both keep counting the index.
                let frame = self.frames.entry(start.slot).or_default();
                if frame.ended {
                    self.counts.restarts += 1;
                    tracing::warn!(
                        slot = start.slot,
                        region = %start.region,
                        "A slot started again; the feed can resend or omit earlier transactions"
                    );
                }
                frame.open = true;
                frame.ended = false;
                None
            },
            Some(Payload::SlotEnd(end)) => {
                let frame = self.frames.entry(end.slot).or_default();
                frame.open = false;
                frame.ended = true;
                let ended = self.ended.map_or(end.slot, |ended| ended.max(end.slot));
                self.ended = Some(ended);
                self.frames = self.frames.split_off(&ended.saturating_sub(SLOT_RETENTION));
                None
            },
            Some(Payload::Clip(clip)) => {
                self.counts.clipped = self.counts.clipped.saturating_add(clip.transactions);
                tracing::warn!(
                    withheld = clip.transactions,
                    "The feed withheld transactions: the account is over its coverage share"
                );
                None
            },
            Some(Payload::Ping(_)) => None,
            // The server schema moved ahead of the proto crate, so count it for the exit summary.
            None => {
                self.counts.unknown_payload += 1;
                tracing::debug!("The feed sent a payload this crate does not know");
                None
            },
        };

        // Only a slot end prunes the map, so a stream that sends no ends needs this bound too.
        while self.frames.len() > MAX_FRAMES {
            self.frames.pop_first();
        }

        update
    }

    fn transaction(
        &mut self,
        mut filters: Vec<String>,
        tx: HarmonicTransaction,
    ) -> Option<SubscribeUpdate> {
        // Unset is unknown, not success. The runtime has no field for unknown.
        let Some(result) = tx.execution_result() else {
            self.counts.unknown_result += 1;
            if !self.warned_unknown_result {
                self.warned_unknown_result = true;
                tracing::warn!(
                    slot = tx.slot,
                    region = %tx.region,
                    "The feed sent a transaction with no result; the source drops these. Only a \
                     prefilter with `failed: None` gets them from the feed"
                );
            }
            tracing::debug!(slot = tx.slot, region = %tx.region, "Dropping a preconf with no result");
            return None;
        };

        let decoded = match convert::decode(&tx.transaction) {
            Ok(decoded) => decoded,
            Err(err) => {
                self.counts.undecodable += 1;
                tracing::warn!(
                    slot = tx.slot,
                    region = %tx.region,
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
                slot = tx.slot,
                region = %tx.region,
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
            tracing::debug!(slot = tx.slot, region = %tx.region, "A preconf named no filter");
            return None;
        }
        filters.retain(|name| {
            self.vote_rules
                .get(name)
                .is_none_or(|&wanted| wanted == is_vote)
        });
        if filters.is_empty() {
            self.counts.vote_rule_dropped += 1;
            tracing::debug!(slot = tx.slot, region = %tx.region, is_vote, "No parser wants this preconf");
            return None;
        }

        let frame = self.frames.entry(tx.slot).or_default();
        if !frame.open {
            self.counts.out_of_frame += 1;
            tracing::debug!(
                slot = tx.slot,
                region = %tx.region,
                "A preconf arrived outside its slot frame"
            );
        }
        let index = frame.next;
        frame.next += 1;

        // These fields have no place in a `SubscribeUpdateTransaction`, so log them for each
        // delivered transaction.
        tracing::debug!(
            slot = tx.slot,
            signature = %shipstern_core::bs58::encode(&signature).into_string(),
            seq = tx.seq,
            region = %tx.region,
            "Harmonic preconf"
        );
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
            slot: tx.slot,
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

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use shipstern_core::{Filters, Prefilter, Pubkey, TransactionPrefilter};
    use triton_preconfs_proto::preconfs::{
        harmonic_update::Payload, CoverageClip, ExecutionResult, HarmonicUpdate, Ping,
    };
    use yellowstone_grpc_proto::geyser::{
        subscribe_update::UpdateOneof, SubscribeUpdate, SubscribeUpdateTransaction,
    };

    use super::{Framer, MAX_FRAMES};
    use crate::{
        filter::vote_rules,
        test_fixtures::{end, fixture, start, transaction, vote_transaction, LEGACY},
    };

    fn framer() -> Framer { Framer::new(HashMap::new()) }

    fn delivered(update: Option<SubscribeUpdate>) -> (Vec<String>, SubscribeUpdateTransaction) {
        let update = update.expect("a transaction update");
        let Some(UpdateOneof::Transaction(tx)) = update.update_oneof else {
            panic!("expected a transaction, got {:?}", update.update_oneof);
        };
        (update.filters, tx)
    }

    fn index(framer: &mut Framer, slot: u64) -> u64 {
        let (bytes, _) = fixture(LEGACY);
        let update = framer.apply(transaction(slot, bytes, Some(ExecutionResult::Success)));
        delivered(update).1.transaction.unwrap().index
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

    /// An unset result is unknown, not a success, so it is dropped and counted.
    #[test]
    fn an_unknown_result_is_dropped_and_counted() {
        let (bytes, _) = fixture(LEGACY);
        let mut framer = framer();

        let update = framer.apply(transaction(7, bytes, None));

        assert!(update.is_none());
        assert_eq!(framer.counts.unknown_result, 1);
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

    /// A payload this crate does not know is counted, so a newer server schema shows up.
    #[test]
    fn an_unknown_payload_is_counted() {
        let mut framer = framer();

        let update = framer.apply(HarmonicUpdate {
            filters: vec![],
            payload: None,
        });

        assert!(update.is_none());
        assert_eq!(framer.counts.unknown_payload, 1);
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
        let mut update = transaction(7, bytes, Some(ExecutionResult::Success));
        update.filters.clear();

        let update = framer.apply(update);

        assert!(update.is_none());
        assert_eq!(framer.counts.unknown_payload, 1);
        assert_eq!(framer.counts.vote_rule_dropped, 0);
    }

    /// A clip only counts, and a ping is not an update.
    #[test]
    fn a_clip_is_counted_and_a_ping_yields_nothing() {
        let mut framer = framer();

        let clip = framer.apply(HarmonicUpdate {
            filters: vec![],
            payload: Some(Payload::Clip(CoverageClip { transactions: 5 })),
        });
        let ping = framer.apply(HarmonicUpdate {
            filters: vec![],
            payload: Some(Payload::Ping(Ping {})),
        });

        assert!(clip.is_none());
        assert!(ping.is_none());
        assert_eq!(framer.counts.clipped, 5);
    }

    /// Clip counts come from the server, so two large clips must not overflow.
    #[test]
    fn clip_counts_saturate() {
        let mut framer = framer();
        let clip = |n| HarmonicUpdate {
            filters: vec![],
            payload: Some(Payload::Clip(CoverageClip { transactions: n })),
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
        let mut framer = Framer::new(vote_rules(&filters));
        let deliver = |framer: &mut Framer, bytes: Vec<u8>, names: Vec<String>| {
            let mut update = transaction(7, bytes, Some(ExecutionResult::Success));
            update.filters = names;
            framer.apply(update).map(|u| u.filters)
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
