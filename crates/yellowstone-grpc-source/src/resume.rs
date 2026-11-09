//! Slot-checkpoint resume for a confirmed or finalized stream.
//!
//! A slot at those commitments never changes once delivered, so a lost stream subscribes again
//! a few slots before a checkpoint that waits for open slots, and drops the repeats.

use std::collections::{hash_map::Entry, BTreeMap, HashMap, HashSet};

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
    // Each server numbers writes with its own counter, so the key has no `write_version`. Geyser
    // keeps one write per pubkey in a slot; the signature is for a server that sends every write.
    Account {
        pubkey: Vec<u8>,
        txn_signature: Option<Vec<u8>>,
    },
    Transaction(Vec<u8>),
    TransactionStatus(Vec<u8>),
    Entry(u64),
    Slot(i32),
    BlockMeta,
    Block,
    // Each server numbers its banks in its own way, so the key has no `bank_id`. A confirmed
    // slot has only one bank.
    BlockFooter,
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
                txn_signature: info.txn_signature.clone(),
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
        UpdateOneof::Block(block) => (block.slot, UpdateKey::Block, Role::Content),
        UpdateOneof::BlockFooter(footer) => (footer.slot, UpdateKey::BlockFooter, Role::Content),
        UpdateOneof::EntryUpdateParent(_) | UpdateOneof::Ping(_) | UpdateOneof::Pong(_) => {
            return None;
        },
    };
    Some(classified)
}

/// How far above the oldest open slot a slot gets a record, so the state stays bounded when a
/// status never comes. It is below the 150 slots Geyser keeps for replay by default.
const MAX_TRACKED_SLOTS: u64 = 32;

/// How far below the checkpoint a resubscribe starts and records drop repeats, for a parent sent
/// after its child. It equals Agave `NUM_CONSECUTIVE_LEADER_SLOTS`. A later parent is not replayed.
const REPEAT_WINDOW: u64 = 4;

/// How far a slot has come.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum SlotState {
    /// Neither open nor complete. The record only drops repeats.
    #[default]
    Idle,
    /// Content came at or above the checkpoint. The slot holds the checkpoint until its status
    /// at the commitment comes.
    Open,
    /// Its status at the commitment came.
    Complete,
}

/// The keys delivered for one slot, and how far the slot has come.
#[derive(Debug, Default)]
struct SlotRecord {
    /// The filter names each key went to. The server sends one block per `blocks` filter and
    /// one footer per certificates group, each under its own names.
    /// Geyser v16.2.0: <https://github.com/rpcpool/yellowstone-grpc/blob/b408d1fa01458b90818fc4ba99831a6b254cbf26/yellowstone-grpc-geyser/src/plugin/filter/filter.rs#L2208-L2410>
    seen: HashMap<UpdateKey, HashSet<String>>,
    state: SlotState,
}

impl SlotRecord {
    /// Remove from `filters` each name whose filter already got `key`, and note the rest. False
    /// when no name is left. Without names, a key that came before is a repeat.
    fn mark_seen(&mut self, key: UpdateKey, filters: &mut Vec<String>) -> bool {
        let delivered = match self.seen.entry(key) {
            Entry::Vacant(entry) => {
                entry.insert(filters.iter().cloned().collect());
                return true;
            },
            Entry::Occupied(entry) => entry.into_mut(),
        };

        filters.retain(|name| delivered.insert(name.clone()));
        !filters.is_empty()
    }
}

/// The slots from `REPEAT_WINDOW` below the checkpoint up, where a resubscribe starts, and the
/// keys each one delivered.
#[derive(Debug)]
pub(crate) struct ResumePoint {
    commitment: SlotStatus,
    slots: BTreeMap<u64, SlotRecord>,
    newest_complete: Option<u64>,
    /// The last open slot reported as stuck, so each one warns once.
    warned_open_slot: Option<u64>,
}

impl ResumePoint {
    pub(crate) fn new(commitment: SlotStatus) -> Self {
        Self {
            commitment,
            slots: BTreeMap::new(),
            newest_complete: None,
            warned_open_slot: None,
        }
    }

    /// Where to subscribe from after a loss: `REPEAT_WINDOW` below the checkpoint, but not below
    /// `configured`, since nothing before it was delivered.
    pub(crate) fn resubscribe_slot(&self, configured: Option<u64>) -> Option<u64> {
        let Some(checkpoint) = self.checkpoint() else {
            return configured;
        };

        Some(checkpoint.saturating_sub(REPEAT_WINDOW)).max(configured)
    }

    /// The oldest open slot or the newest complete one, whichever is older. A slot between them
    /// can still come.
    fn checkpoint(&self) -> Option<u64> {
        self.oldest_open()
            .into_iter()
            .chain(self.newest_complete)
            .min()
    }

    fn oldest_open(&self) -> Option<u64> {
        self.slots
            .iter()
            .find(|(_, record)| record.state == SlotState::Open)
            .map(|(slot, _)| *slot)
    }

    /// Note `update`, remove the names of the filters that already got it, and say whether to
    /// deliver it. An update that gets no record is always delivered, so it can repeat.
    pub(crate) fn deliver(&mut self, update: &mut SubscribeUpdate) -> bool {
        let Some((slot, key, role)) = classify(update, self.commitment) else {
            return true;
        };

        let checkpoint = self.checkpoint();
        if !self.slots.contains_key(&slot) && !self.may_record(slot, role, checkpoint) {
            // Above the cap, content or a commitment status shows that the open slot is stuck. On a
            // finalized stream, statuses of another level run this far ahead with no stuck slot.
            if role != Role::OtherStatus
                && let Some(open) = self.oldest_open()
                && slot > open
                && self.warned_open_slot != Some(open)
            {
                tracing::warn!(
                    open_slot = open,
                    slot,
                    "No status at the commitment came for this open slot; slots more than 32 \
                     above it are not recorded, and the checkpoint waits for it"
                );
                self.warned_open_slot = Some(open);
            }

            return true;
        }

        let record = self.slots.entry(slot).or_default();
        let fresh = record.mark_seen(key, &mut update.filters);

        // Below the checkpoint a record only drops repeats, so a late copy never moves it back.
        if checkpoint.is_some_and(|checkpoint| slot < checkpoint) {
            return fresh;
        }

        match role {
            Role::Content if record.state == SlotState::Idle => {
                record.state = SlotState::Open;
            },
            Role::CommitmentStatus => {
                record.state = SlotState::Complete;
                self.newest_complete = self.newest_complete.max(Some(slot));
                // Only a commitment status moves the checkpoint up.
                self.forget_below_window();
            },
            Role::Content | Role::OtherStatus => {},
        }

        fresh
    }

    /// Whether `slot`, which has no record, gets one. A slot gets none more than `REPEAT_WINDOW`
    /// below the checkpoint or `MAX_TRACKED_SLOTS` above the oldest open slot.
    fn may_record(&self, slot: u64, role: Role, checkpoint: Option<u64>) -> bool {
        let Some(checkpoint) = checkpoint else {
            // The status at the commitment can stall at the start while statuses of another
            // level keep coming.
            return role != Role::OtherStatus || self.slots.len() <= MAX_TRACKED_SLOTS as usize;
        };

        // Below the checkpoint, only a slot that a resubscribe sends again gets a record.
        if slot < checkpoint {
            return checkpoint - slot <= REPEAT_WINDOW;
        }

        let Some(open) = self.oldest_open() else {
            // Content opens a slot that caps later records, and a commitment status frees old ones.
            return role != Role::OtherStatus || slot - checkpoint <= MAX_TRACKED_SLOTS;
        };

        slot.saturating_sub(open) <= MAX_TRACKED_SLOTS
    }

    /// A resubscribe never repeats a slot more than `REPEAT_WINDOW` below the checkpoint.
    fn forget_below_window(&mut self) {
        let Some(checkpoint) = self.checkpoint() else {
            return;
        };

        let floor = checkpoint.saturating_sub(REPEAT_WINDOW);
        while let Some(oldest) = self.slots.first_entry()
            && *oldest.key() < floor
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

    fn account_with_signature(slot: u64, write_version: u64, signature: u8) -> SubscribeUpdate {
        let mut update = account(slot, write_version);
        if let Some(UpdateOneof::Account(account)) = update.update_oneof.as_mut()
            && let Some(info) = account.account.as_mut()
        {
            info.txn_signature = Some(vec![signature; 64]);
        }
        update
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

    /// Slot 10 is cut after two of its three updates. The replay repeats them and delivers the
    /// third, and content of slot 11 does not move the checkpoint past 10.
    #[test]
    fn replay_of_the_checkpoint_slot_delivers_only_what_was_missed() {
        let mut resume = ResumePoint::new(SlotStatus::SlotConfirmed);
        assert!(resume.deliver(&mut transaction(10, 1)));
        assert!(resume.deliver(&mut account(10, 5)));
        assert_eq!(resume.checkpoint(), Some(10));

        assert!(!resume.deliver(&mut transaction(10, 1)));
        assert!(!resume.deliver(&mut account(10, 5)));
        assert!(resume.deliver(&mut transaction(10, 2)));
        assert!(resume.deliver(&mut slot_status(10, SlotStatus::SlotConfirmed)));

        assert!(resume.deliver(&mut transaction(11, 1)));
        assert_eq!(resume.checkpoint(), Some(10));
        assert!(!resume.deliver(&mut transaction(10, 1)));
    }

    /// A status that only the resume filter matched is tracked but not delivered. One the user
    /// also asked for is delivered under the user's key alone.
    #[test]
    fn resume_only_slot_status_is_tracked_and_dropped() {
        let mut resume = ResumePoint::new(SlotStatus::SlotConfirmed);
        let mut update = slot_status(10, SlotStatus::SlotConfirmed);
        update.filters = vec![RESUME_FILTER_KEY.to_owned()];
        assert!(resume.deliver(&mut update));
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
        assert!(resume.deliver(&mut footer("plain")));
        assert!(resume.deliver(&mut footer("certificates")));
        assert!(resume.deliver(&mut block("a")));
        assert!(resume.deliver(&mut block("b")));

        assert!(!resume.deliver(&mut footer("certificates")));
        assert!(!resume.deliver(&mut block("b")));
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
        assert!(resume.deliver(&mut footer(101)));
        assert!(!resume.deliver(&mut footer(900)));
    }

    /// An interslot status for a slot ahead must not move the checkpoint past content that has
    /// not arrived, but a status at the stream's commitment does.
    #[test]
    fn only_a_status_at_the_commitment_moves_the_checkpoint() {
        let mut resume = ResumePoint::new(SlotStatus::SlotFinalized);
        assert!(resume.deliver(&mut transaction(10, 1)));
        assert!(resume.deliver(&mut slot_status(15, SlotStatus::SlotFirstShredReceived)));
        assert_eq!(resume.checkpoint(), Some(10));

        assert!(resume.deliver(&mut slot_status(10, SlotStatus::SlotFinalized)));
        assert!(resume.deliver(&mut slot_status(12, SlotStatus::SlotFinalized)));
        assert_eq!(resume.checkpoint(), Some(12));
    }

    /// Geyser can send a parent after its child is complete. After a loss inside the parent, the
    /// source subscribes again from below it, and the replay drops what the parent delivered.
    #[test]
    fn resubscribe_covers_a_parent_sent_after_its_child() {
        let mut resume = ResumePoint::new(SlotStatus::SlotConfirmed);
        assert!(resume.deliver(&mut transaction(43, 1)));
        assert!(resume.deliver(&mut slot_status(43, SlotStatus::SlotConfirmed)));
        assert!(resume.deliver(&mut transaction(42, 1)));
        assert_eq!(resume.checkpoint(), Some(43));
        assert!(resume.resubscribe_slot(None).is_some_and(|slot| slot <= 42));

        assert!(!resume.deliver(&mut transaction(42, 1)));
    }

    /// A slot whose status never comes holds the checkpoint, so a later reconnect fails with
    /// `OutOfRange` instead of skipping the slot. Slots more than `MAX_TRACKED_SLOTS` above it
    /// are not recorded.
    #[test]
    fn checkpoint_holds_a_slot_whose_status_never_comes() {
        let mut resume = ResumePoint::new(SlotStatus::SlotConfirmed);
        assert!(resume.deliver(&mut transaction(10, 1)));

        let newest = 10 + MAX_TRACKED_SLOTS;
        for slot in 11..=newest + 1 {
            assert!(resume.deliver(&mut slot_status(slot, SlotStatus::SlotConfirmed)));
        }
        assert_eq!(resume.checkpoint(), Some(10));
        assert_eq!(resume.slots.len(), MAX_TRACKED_SLOTS as usize + 1);

        assert!(resume.deliver(&mut transaction(10, 2)));
        assert_eq!(resume.checkpoint(), Some(10));
    }

    /// Each server numbers account writes with its own counter.
    #[test]
    fn account_repeat_with_another_write_version_is_dropped() {
        let mut resume = ResumePoint::new(SlotStatus::SlotConfirmed);
        assert!(resume.deliver(&mut account_with_signature(10, 5, 7)));
        assert!(!resume.deliver(&mut account_with_signature(10, 9_000_005, 7)));
    }

    /// Writes of one account by distinct transactions are distinct updates.
    #[test]
    fn account_writes_with_distinct_signatures_are_kept() {
        let mut resume = ResumePoint::new(SlotStatus::SlotConfirmed);
        assert!(resume.deliver(&mut account_with_signature(10, 5, 7)));
        assert!(resume.deliver(&mut account_with_signature(10, 6, 8)));
    }

    /// A filter added across a reconnect gets the repeat, and the old filter does not.
    #[test]
    fn repeat_reaches_only_a_filter_added_on_reconnect() {
        let footer = |filters: &[&str]| SubscribeUpdate {
            filters: filters.iter().map(|name| (*name).to_owned()).collect(),
            update_oneof: Some(UpdateOneof::BlockFooter(SubscribeUpdateBlockFooter {
                slot: 10,
                ..Default::default()
            })),
            ..Default::default()
        };

        let mut resume = ResumePoint::new(SlotStatus::SlotConfirmed);
        assert!(resume.deliver(&mut footer(&["a"])));

        let mut repeat = footer(&["a", "b"]);
        assert!(resume.deliver(&mut repeat));
        assert_eq!(repeat.filters, ["b"]);
    }

    /// A late repeat of a complete slot below the window has no record left, so it is delivered,
    /// but it never opens the slot again.
    #[test]
    fn late_copy_below_the_window_does_not_reopen_a_complete_slot() {
        let mut resume = ResumePoint::new(SlotStatus::SlotConfirmed);
        for slot in 10..=20 {
            assert!(resume.deliver(&mut transaction(slot, 1)));
            assert!(resume.deliver(&mut slot_status(slot, SlotStatus::SlotConfirmed)));
        }

        assert!(!resume.deliver(&mut transaction(17, 1)));
        assert_eq!(resume.checkpoint(), Some(20));
        for slot in [15, 12] {
            assert!(resume.deliver(&mut transaction(slot, 1)));
            assert_eq!(resume.checkpoint(), Some(20));
        }
    }

    /// Skipped leaders can leave more than `MAX_TRACKED_SLOTS` slots with no status.
    #[test]
    fn checkpoint_moves_past_a_run_of_skipped_slots() {
        let mut resume = ResumePoint::new(SlotStatus::SlotConfirmed);
        for slot in [100, 141, 142, 300] {
            assert!(resume.deliver(&mut transaction(slot, 1)));
            assert!(resume.deliver(&mut slot_status(slot, SlotStatus::SlotConfirmed)));
        }

        assert_eq!(resume.checkpoint(), Some(300));
        assert!(!resume.deliver(&mut transaction(300, 1)));
    }

    /// On a slots-only stream a processed status comes before the slot has a record, and the
    /// replay sends it again.
    #[test]
    fn processed_status_before_confirmation_is_dropped_on_replay() {
        let mut resume = ResumePoint::new(SlotStatus::SlotConfirmed);
        let mut processed = slot_status(10, SlotStatus::SlotProcessed);
        assert!(resume.deliver(&mut processed));
        assert!(resume.deliver(&mut slot_status(10, SlotStatus::SlotConfirmed)));

        assert!(!resume.deliver(&mut processed));
    }

    /// Content that keeps coming while the statuses stop must not grow the state.
    #[test]
    fn content_without_commitment_status_keeps_state_bounded() {
        let mut resume = ResumePoint::new(SlotStatus::SlotConfirmed);
        for slot in 0..100 {
            assert!(resume.deliver(&mut transaction(slot, 1)));
        }

        assert_eq!(resume.checkpoint(), Some(0));
        assert!(resume.slots.len() <= MAX_TRACKED_SLOTS as usize + 1);
    }

    /// The resubscribe starts `REPEAT_WINDOW` below the checkpoint, but never before the
    /// configured start.
    #[test]
    fn resubscribe_starts_a_window_below_the_checkpoint() {
        let mut resume = ResumePoint::new(SlotStatus::SlotConfirmed);
        assert_eq!(resume.resubscribe_slot(Some(98)), Some(98));

        assert!(resume.deliver(&mut transaction(100, 1)));
        assert!(resume.deliver(&mut slot_status(100, SlotStatus::SlotConfirmed)));
        assert_eq!(resume.resubscribe_slot(None), Some(100 - REPEAT_WINDOW));
        assert_eq!(resume.resubscribe_slot(Some(98)), Some(98));
    }

    /// Statuses of another level that come while finality stalls at the start must not grow
    /// the state.
    #[test]
    fn statuses_of_another_level_before_a_checkpoint_keep_state_bounded() {
        let mut resume = ResumePoint::new(SlotStatus::SlotFinalized);
        for slot in 0..10_000 {
            assert!(resume.deliver(&mut slot_status(slot, SlotStatus::SlotProcessed)));
        }

        assert_eq!(resume.checkpoint(), None);
        assert!(resume.slots.len() <= MAX_TRACKED_SLOTS as usize + 1);
    }
}
