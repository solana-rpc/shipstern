//! Slot-checkpoint resume for a confirmed or finalized stream.
//!
//! A slot at those commitments never changes once delivered, so a lost stream is recovered
//! by subscribing again from the oldest slot that is not complete, and dropping the repeats of
//! the slots from there up. Everything after them is new by construction.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use yellowstone_grpc_proto::geyser::{
    subscribe_update::UpdateOneof, SlotStatus, SubscribeRequest, SubscribeRequestFilterSlots,
    SubscribeUpdate,
};

/// The `slots` filter `SlotResume` adds under its own key, so the checkpoint moves with every
/// slot at the stream's commitment even before the first update matches the user's filters.
pub(crate) const RESUME_FILTER_KEY: &str = "__shipstern_slot_resume";

/// Ask for the slot status at the stream's commitment under [`RESUME_FILTER_KEY`]. The user's
/// own `slots` filters stay as they are.
pub(crate) fn add_resume_filter(request: &mut SubscribeRequest) {
    request
        .slots
        .insert(RESUME_FILTER_KEY.to_owned(), SubscribeRequestFilterSlots {
            filter_by_commitment: Some(true),
            interslot_updates: Some(false),
        });
}

/// Take the resume key off an update's matched filters. False when only the resume filter
/// matched, so the runtime never sees a slot status it did not ask for.
pub(crate) fn strip_resume_filter(update: &mut SubscribeUpdate) -> bool {
    if update.filters.is_empty() {
        return true;
    }
    update.filters.retain(|name| name != RESUME_FILTER_KEY);
    !update.filters.is_empty()
}

/// What tells two updates of one slot apart.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum UpdateKey {
    Account { pubkey: Vec<u8>, write_version: u64 },
    Transaction(Vec<u8>),
    TransactionStatus(Vec<u8>),
    Entry(u64),
    Slot(i32),
    BlockMeta,
    Block(BTreeSet<String>),
    // Each server numbers its banks in its own way, so the key has no `bank_id`. A confirmed
    // slot has only one bank.
    BlockFooter(BTreeSet<String>),
}

/// The server sends one block per `blocks` filter and one footer per certificates group, so
/// the filter names tell those copies apart.
/// Geyser v16.2.0: <https://github.com/rpcpool/yellowstone-grpc/blob/b408d1fa01458b90818fc4ba99831a6b254cbf26/yellowstone-grpc-geyser/src/plugin/filter/filter.rs#L2208-L2410>
fn filter_names(update: &SubscribeUpdate) -> BTreeSet<String> {
    update.filters.iter().cloned().collect()
}

/// What an update tells about its slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    /// Part of the slot's content.
    Content,
    /// The status at the stream's commitment. Geyser sends it after the slot's content, so the
    /// slot is complete.
    CommitmentStatus,
    /// A status of another level. It can come before the slot's content.
    OtherStatus,
}

/// The slot an update belongs to, its key in that slot, and its role. A ping, a pong or a
/// parent update has no slot.
fn classify(update: &SubscribeUpdate, commitment: SlotStatus) -> Option<(u64, UpdateKey, Role)> {
    let classified = match update.update_oneof.as_ref()? {
        UpdateOneof::Account(account) => {
            let info = account.account.as_ref()?;
            let key = UpdateKey::Account {
                pubkey: info.pubkey.clone(),
                write_version: info.write_version,
            };
            (account.slot, key, Role::Content)
        },
        UpdateOneof::Transaction(tx) => {
            let signature = tx.transaction.as_ref()?.signature.clone();
            (tx.slot, UpdateKey::Transaction(signature), Role::Content)
        },
        UpdateOneof::TransactionStatus(tx) => {
            let key = UpdateKey::TransactionStatus(tx.signature.clone());
            (tx.slot, key, Role::Content)
        },
        UpdateOneof::Entry(entry) => (entry.slot, UpdateKey::Entry(entry.index), Role::Content),
        UpdateOneof::Slot(slot) => {
            let role = if slot.status == commitment as i32 {
                Role::CommitmentStatus
            } else {
                Role::OtherStatus
            };
            (slot.slot, UpdateKey::Slot(slot.status), role)
        },
        UpdateOneof::BlockMeta(meta) => (meta.slot, UpdateKey::BlockMeta, Role::Content),
        UpdateOneof::Block(block) => {
            let key = UpdateKey::Block(filter_names(update));
            (block.slot, key, Role::Content)
        },
        UpdateOneof::BlockFooter(footer) => {
            let key = UpdateKey::BlockFooter(filter_names(update));
            (footer.slot, key, Role::Content)
        },
        UpdateOneof::EntryUpdateParent(_) | UpdateOneof::Ping(_) | UpdateOneof::Pong(_) => {
            return None;
        },
    };
    Some(classified)
}

/// How far an incomplete slot can fall behind the newest complete slot before resume stops
/// waiting for it. Geyser sends an ancestor right after its child, so a real gap is a few
/// slots, and 32 is well inside the 150 slots Geyser keeps for replay by default.
const MAX_INCOMPLETE_LAG: u64 = 32;

/// The keys delivered for one slot, and whether its status at the commitment came.
#[derive(Debug, Default)]
struct SlotRecord {
    seen: HashSet<UpdateKey>,
    complete: bool,
}

/// The slots from the checkpoint up and what each one delivered. A resubscribe from the
/// checkpoint repeats them, and nothing older.
#[derive(Debug)]
pub(crate) struct ResumePoint {
    commitment: SlotStatus,
    slots: BTreeMap<u64, SlotRecord>,
    newest_complete: Option<u64>,
}

impl ResumePoint {
    pub(crate) fn new(commitment: SlotStatus) -> Self {
        Self {
            commitment,
            slots: BTreeMap::new(),
            newest_complete: None,
        }
    }

    /// The slot to subscribe from after a loss, inclusive: the oldest slot that is not
    /// complete, else the newest complete one.
    pub(crate) fn checkpoint(&self) -> Option<u64> {
        let incomplete = self.slots.iter().find(|(_, record)| !record.complete);

        incomplete.map(|(slot, _)| *slot).or(self.newest_complete)
    }

    /// Note `update` and say whether to deliver it. A repeat of a slot from the checkpoint up
    /// is dropped.
    pub(crate) fn deliver(&mut self, update: &SubscribeUpdate) -> bool {
        let Some((slot, key, role)) = classify(update, self.commitment) else {
            return true;
        };

        // A status of another level can come before the content, so it starts no record. A
        // slot that resume gave up on starts none either.
        if !self.slots.contains_key(&slot) && (role == Role::OtherStatus || self.gave_up(slot)) {
            return true;
        }

        let record = self.slots.entry(slot).or_default();
        let fresh = record.seen.insert(key);

        if role == Role::CommitmentStatus {
            record.complete = true;
            self.newest_complete = self.newest_complete.max(Some(slot));
            self.give_up_lagging();
        }

        self.forget_before_checkpoint();

        fresh
    }

    fn gave_up(&self, slot: u64) -> bool {
        self.newest_complete
            .is_some_and(|newest| slot < newest.saturating_sub(MAX_INCOMPLETE_LAG))
    }

    /// Treat each slot more than `MAX_INCOMPLETE_LAG` behind the newest complete slot as
    /// complete, so the checkpoint can move past it.
    fn give_up_lagging(&mut self) {
        let Some(newest) = self.newest_complete else {
            return;
        };

        let floor = newest.saturating_sub(MAX_INCOMPLETE_LAG);
        for (slot, record) in self.slots.range_mut(..floor) {
            if !record.complete {
                tracing::warn!(
                    slot = *slot,
                    newest_complete = newest,
                    "No status at the commitment came for this slot; resume stops waiting for it"
                );
                record.complete = true;
            }
        }
    }

    /// A resubscribe from the checkpoint never repeats a slot below it.
    fn forget_before_checkpoint(&mut self) {
        let Some(checkpoint) = self.checkpoint() else {
            return;
        };

        while let Some(oldest) = self.slots.first_entry()
            && *oldest.key() < checkpoint
        {
            oldest.remove();
        }
    }
}

#[cfg(test)]
mod tests {
    use yellowstone_grpc_proto::geyser::{
        SubscribeUpdateAccount, SubscribeUpdateAccountInfo, SubscribeUpdateBlock,
        SubscribeUpdateBlockFooter, SubscribeUpdateSlot, SubscribeUpdateTransaction,
        SubscribeUpdateTransactionInfo,
    };

    use super::*;

    fn transaction(slot: u64, signature: u8) -> SubscribeUpdate {
        SubscribeUpdate {
            update_oneof: Some(UpdateOneof::Transaction(SubscribeUpdateTransaction {
                slot,
                bank_id: 0,
                transaction: Some(SubscribeUpdateTransactionInfo {
                    signature: vec![signature; 64],
                    ..Default::default()
                }),
            })),
            ..Default::default()
        }
    }

    fn account(slot: u64, write_version: u64) -> SubscribeUpdate {
        SubscribeUpdate {
            update_oneof: Some(UpdateOneof::Account(SubscribeUpdateAccount {
                slot,
                is_startup: false,
                bank_id: None,
                account: Some(SubscribeUpdateAccountInfo {
                    pubkey: vec![1; 32],
                    write_version,
                    ..Default::default()
                }),
            })),
            ..Default::default()
        }
    }

    fn slot_status(slot: u64, status: SlotStatus) -> SubscribeUpdate {
        SubscribeUpdate {
            update_oneof: Some(UpdateOneof::Slot(SubscribeUpdateSlot {
                slot,
                status: status as i32,
                ..Default::default()
            })),
            ..Default::default()
        }
    }

    /// Slot 10 is cut after two of its three updates. The replay from 10 repeats them and
    /// delivers the third, then slot 11 starts a fresh set.
    #[test]
    fn replay_of_the_checkpoint_slot_delivers_only_what_was_missed() {
        let mut resume = ResumePoint::new(SlotStatus::SlotConfirmed);
        assert!(resume.deliver(&transaction(10, 1)));
        assert!(resume.deliver(&account(10, 5)));
        assert_eq!(resume.checkpoint(), Some(10));

        assert!(!resume.deliver(&transaction(10, 1)));
        assert!(!resume.deliver(&account(10, 5)));
        assert!(resume.deliver(&transaction(10, 2)));
        assert!(resume.deliver(&slot_status(10, SlotStatus::SlotConfirmed)));

        assert!(resume.deliver(&transaction(11, 1)));
        assert_eq!(resume.checkpoint(), Some(11));
        assert!(resume.deliver(&transaction(10, 1)));
    }

    /// A status that only the resume filter matched is tracked but not delivered. One the user
    /// also asked for is delivered under the user's key alone.
    #[test]
    fn resume_only_slot_status_is_tracked_and_dropped() {
        let mut resume = ResumePoint::new(SlotStatus::SlotConfirmed);
        let mut update = slot_status(10, SlotStatus::SlotConfirmed);
        update.filters = vec![RESUME_FILTER_KEY.to_owned()];
        assert!(resume.deliver(&update));
        assert!(!strip_resume_filter(&mut update));
        assert_eq!(resume.checkpoint(), Some(10));

        let mut update = slot_status(11, SlotStatus::SlotConfirmed);
        update.filters = vec!["user".to_owned(), RESUME_FILTER_KEY.to_owned()];
        assert!(strip_resume_filter(&mut update));
        assert_eq!(update.filters, ["user"]);
    }

    /// Each footer copy and each block copy of one bank reaches its own filters, once.
    #[test]
    fn footer_and_block_copies_per_filter_are_kept() {
        let footer = |filter: &str| SubscribeUpdate {
            filters: vec![filter.to_owned()],
            update_oneof: Some(UpdateOneof::BlockFooter(SubscribeUpdateBlockFooter {
                slot: 10,
                bank_id: 4,
                ..Default::default()
            })),
            ..Default::default()
        };
        let block = |filter: &str| SubscribeUpdate {
            filters: vec![filter.to_owned()],
            update_oneof: Some(UpdateOneof::Block(SubscribeUpdateBlock {
                slot: 10,
                ..Default::default()
            })),
            ..Default::default()
        };

        let mut resume = ResumePoint::new(SlotStatus::SlotConfirmed);
        assert!(resume.deliver(&footer("plain")));
        assert!(resume.deliver(&footer("certificates")));
        assert!(resume.deliver(&block("a")));
        assert!(resume.deliver(&block("b")));

        assert!(!resume.deliver(&footer("certificates")));
        assert!(!resume.deliver(&block("b")));
    }

    /// A resubscribe can reach another server, which gives the same bank another `bank_id`.
    #[test]
    fn footer_repeat_from_another_server_is_dropped() {
        let footer = |bank_id| SubscribeUpdate {
            filters: vec!["footers".to_owned()],
            update_oneof: Some(UpdateOneof::BlockFooter(SubscribeUpdateBlockFooter {
                slot: 10,
                bank_id,
                ..Default::default()
            })),
            ..Default::default()
        };

        let mut resume = ResumePoint::new(SlotStatus::SlotConfirmed);
        assert!(resume.deliver(&footer(101)));
        assert!(!resume.deliver(&footer(900)));
    }

    /// An interslot status for a slot ahead must not move the checkpoint past content that has
    /// not arrived, but a status at the stream's commitment does.
    #[test]
    fn only_a_status_at_the_commitment_moves_the_checkpoint() {
        let mut resume = ResumePoint::new(SlotStatus::SlotFinalized);
        assert!(resume.deliver(&transaction(10, 1)));
        assert!(resume.deliver(&slot_status(15, SlotStatus::SlotFirstShredReceived)));
        assert_eq!(resume.checkpoint(), Some(10));

        assert!(resume.deliver(&slot_status(10, SlotStatus::SlotFinalized)));
        assert!(resume.deliver(&slot_status(12, SlotStatus::SlotFinalized)));
        assert_eq!(resume.checkpoint(), Some(12));
    }

    /// Geyser can send a slot before an ancestor that had no status of its own. A loss inside
    /// the ancestor resumes from it, and the replay drops what both slots already delivered.
    #[test]
    fn checkpoint_holds_an_ancestor_sent_after_its_child() {
        let mut resume = ResumePoint::new(SlotStatus::SlotConfirmed);
        assert!(resume.deliver(&transaction(43, 1)));
        assert!(resume.deliver(&slot_status(43, SlotStatus::SlotConfirmed)));
        assert!(resume.deliver(&transaction(42, 1)));
        assert_eq!(resume.checkpoint(), Some(42));

        assert!(!resume.deliver(&transaction(42, 1)));
        assert!(resume.deliver(&transaction(42, 2)));
        assert!(resume.deliver(&slot_status(42, SlotStatus::SlotConfirmed)));
        assert!(!resume.deliver(&transaction(43, 1)));
        assert_eq!(resume.checkpoint(), Some(43));
    }

    /// A slot whose status never comes holds the checkpoint only until it is too far behind.
    #[test]
    fn checkpoint_gives_up_a_slot_whose_status_never_comes() {
        let mut resume = ResumePoint::new(SlotStatus::SlotConfirmed);
        assert!(resume.deliver(&transaction(10, 1)));

        let newest = 10 + MAX_INCOMPLETE_LAG;
        for slot in 11..=newest {
            assert!(resume.deliver(&slot_status(slot, SlotStatus::SlotConfirmed)));
        }
        assert_eq!(resume.checkpoint(), Some(10));

        assert!(resume.deliver(&slot_status(newest + 1, SlotStatus::SlotConfirmed)));
        assert_eq!(resume.checkpoint(), Some(newest + 1));

        assert!(resume.deliver(&transaction(10, 2)));
        assert_eq!(resume.checkpoint(), Some(newest + 1));
    }
}
