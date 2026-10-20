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

/// Open slots older than this are gone for good, so their index entries can go.
const OPEN_SLOT_RETENTION: u64 = 64;

/// What a region stream dropped or saw, reported when the source exits.
#[derive(Default, Clone, Copy)]
pub(crate) struct Counts {
    pub unknown_result: u64,
    pub vote_rule_dropped: u64,
    pub undecodable: u64,
    pub out_of_frame: u64,
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
            clipped: self.clipped.saturating_add(other.clipped),
            restarts: self.restarts.saturating_add(other.restarts),
        }
    }
}

/// One per region stream, because the feed frames slots per region.
pub(crate) struct Framer {
    /// The parsers that asked for votes only (`true`) or no votes (`false`).
    vote_rules: HashMap<String, bool>,
    /// Each open slot and the index of its next transaction.
    open: BTreeMap<u64, u64>,
    /// The highest slot that ended.
    ended: Option<u64>,
    warned_unknown_result: bool,
    pub(crate) counts: Counts,
}

impl Framer {
    pub(crate) fn new(vote_rules: HashMap<String, bool>) -> Self {
        Self {
            vote_rules,
            open: BTreeMap::new(),
            ended: None,
            warned_unknown_result: false,
            counts: Counts::default(),
        }
    }

    /// At most one runtime update per feed update; framing, pings, clips and drops give none.
    pub(crate) fn apply(&mut self, update: HarmonicUpdate) -> Option<SubscribeUpdate> {
        let HarmonicUpdate { filters, payload } = update;

        match payload {
            Some(Payload::Transaction(tx)) => self.transaction(filters, tx),
            Some(Payload::SlotStart(start)) => {
                // A start at or below the highest ended slot is a restart of an ended slot,
                // or a late start for a lower slot. Both reset that slot's index.
                if self.ended.is_some_and(|ended| start.slot <= ended) {
                    self.counts.restarts = self.counts.restarts.saturating_add(1);
                    tracing::warn!(
                        slot = start.slot,
                        region = %start.region,
                        "The builder restarted a slot; its earlier transactions stay delivered"
                    );
                }
                self.open.insert(start.slot, 0);
                None
            },
            Some(Payload::SlotEnd(end)) => {
                self.open.remove(&end.slot);
                let ended = self.ended.map_or(end.slot, |ended| ended.max(end.slot));
                self.ended = Some(ended);
                self.open = self
                    .open
                    .split_off(&ended.saturating_sub(OPEN_SLOT_RETENTION));
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
            Some(Payload::Ping(_)) | None => None,
        }
    }

    fn transaction(
        &mut self,
        mut filters: Vec<String>,
        tx: HarmonicTransaction,
    ) -> Option<SubscribeUpdate> {
        // Unset is unknown, not success. The runtime has no field for unknown.
        let Some(result) = tx.execution_result() else {
            self.counts.unknown_result = self.counts.unknown_result.saturating_add(1);
            if !self.warned_unknown_result {
                self.warned_unknown_result = true;
                tracing::warn!(
                    slot = tx.slot,
                    region = %tx.region,
                    "The feed sent a transaction with no result; such transactions are dropped. \
                     `failed = false` filters them server-side"
                );
            }
            tracing::debug!(slot = tx.slot, region = %tx.region, "Dropping a preconf with no result");
            return None;
        };

        let decoded = match convert::decode(&tx.transaction) {
            Ok(decoded) => decoded,
            Err(err) => {
                self.counts.undecodable = self.counts.undecodable.saturating_add(1);
                tracing::warn!(
                    slot = tx.slot,
                    region = %tx.region,
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
            tracing::debug!(slot = tx.slot, region = %tx.region, is_vote, "No parser wants this preconf");
            return None;
        }

        let index = match self.open.get_mut(&tx.slot) {
            Some(next) => {
                let index = *next;
                *next = next.saturating_add(1);
                index
            },
            None => {
                self.counts.out_of_frame = self.counts.out_of_frame.saturating_add(1);
                tracing::debug!(
                    slot = tx.slot,
                    region = %tx.region,
                    "A preconf arrived outside its slot frame"
                );
                self.open.insert(tx.slot, 1);
                0
            },
        };

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
                slot: tx.slot,
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
        harmonic_update::Payload, CoverageClip, ExecutionResult, HarmonicSlotEnd,
        HarmonicSlotStart, HarmonicTransaction, HarmonicUpdate, Ping,
    };
    use yellowstone_grpc_proto::geyser::{
        subscribe_update::UpdateOneof, SubscribeUpdate, SubscribeUpdateTransaction,
    };

    use super::Framer;
    use crate::{
        filter::vote_rules,
        test_fixtures::{fixture, vote_transaction, LEGACY},
    };

    fn framer() -> Framer { Framer::new(HashMap::new()) }

    fn transaction(slot: u64, bytes: Vec<u8>, result: Option<ExecutionResult>) -> HarmonicUpdate {
        HarmonicUpdate {
            filters: vec!["p".to_string()],
            payload: Some(Payload::Transaction(HarmonicTransaction {
                transaction: bytes.into(),
                slot,
                result: result.map(|r| r as i32),
                region: "fra".to_string(),
                seq: 0,
            })),
        }
    }

    fn start(slot: u64) -> HarmonicUpdate {
        HarmonicUpdate {
            filters: vec![],
            payload: Some(Payload::SlotStart(HarmonicSlotStart {
                slot,
                region: "fra".to_string(),
            })),
        }
    }

    fn end(slot: u64) -> HarmonicUpdate {
        HarmonicUpdate {
            filters: vec![],
            payload: Some(Payload::SlotEnd(HarmonicSlotEnd {
                slot,
                region: "fra".to_string(),
            })),
        }
    }

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
