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
    // Servers number writes with their own counters, so the key has no `write_version`. The
    // signature is for a server that sends every write; yellowstone-grpc keeps one per pubkey.
    Account {
        pubkey: Vec<u8>,
        txn_signature: Option<Vec<u8>>,
    },
    Transaction(Vec<u8>),
    TransactionStatus(Vec<u8>),
    Entry(u64),
    // For lower status (e.g. Processed) a slot can have several banks. A resume on another
    // process renumbers banks, so those statuses repeat, and an id can alias another bank's.
    Slot {
        status: i32,
        bank_id: Option<u64>,
    },
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
    /// The status at the stream's commitment. On a confirmed or finalized stream yellowstone-grpc
    /// sends it after the slot's content, accounts too, so the slot is complete.
    CommitmentStatus { parent: Option<u64> },
    /// A status of another level. It can come before the slot's content.
    OtherStatus,
}

/// The slot an update belongs to, its key in that slot, and its role. A ping, a pong, a
/// parent update, a `Completed` or a `Dead` carries nothing to key.
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
            // A slot can die again after a replay, and complete again after a purge and repair,
            // so a second copy is new.
            if matches!(
                slot.status(),
                SlotStatus::SlotCompleted | SlotStatus::SlotDead
            ) {
                return None;
            }

            let role = if slot.status == commitment as i32 {
                Role::CommitmentStatus {
                    parent: slot.parent,
                }
            } else {
                Role::OtherStatus
            };
            let single_bank =
                [SlotStatus::SlotConfirmed, SlotStatus::SlotFinalized].contains(&slot.status());
            let key = UpdateKey::Slot {
                status: slot.status,
                // keep bank_id to identify different banks - i.e. for all "not single bank" status
                bank_id: slot.bank_id.filter(|_| !single_bank),
            };
            (slot.slot, key, role)
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

/// How far from the checkpoint a slot gets a record or a replay: above the oldest open slot,
/// and below for a complete slot or a late parent. It keeps the state bounded, and it is below
/// the 150 slots yellowstone-grpc keeps for replay by default.
const MAX_TRACKED_SLOTS: u64 = 32;

/// How far below the checkpoint a resubscribe starts and records keep their keys. A chosen margin
/// of one leader window (Agave `NUM_CONSECUTIVE_LEADER_SLOTS`). A named late parent moves the
/// start lower and keeps its own keys.
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
    /// The filter names each key went to. yellowstone-grpc sends a block once per `blocks` filter
    /// and a footer once per certificates group:
    /// <https://github.com/rpcpool/yellowstone-grpc/blob/b408d1fa01458b90818fc4ba99831a6b254cbf26/yellowstone-grpc-geyser/src/plugin/filter/filter.rs#L2208-L2410>
    seen: HashMap<UpdateKey, HashSet<String>>,
    state: SlotState,
    /// The parent that the slot's commitment status named, so a resubscribe can replay it.
    parent: Option<u64>,
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

/// The commitment of a stream that resumes. A processed stream has several banks in a slot, so
/// it cannot resume from a slot checkpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResumeCommitment {
    Confirmed,
    Finalized,
}

/// The slots from `REPEAT_WINDOW` below the checkpoint up with the keys each one delivered, the
/// named late parents below that with theirs, and the complete slots from `MAX_TRACKED_SLOTS`
/// below the checkpoint with only their state and parent.
#[derive(Debug)]
pub(crate) struct ResumePoint {
    commitment: SlotStatus,
    slots: BTreeMap<u64, SlotRecord>,
    newest_complete: Option<u64>,
    /// The last open slot reported as stuck, so each one warns once.
    warned_open_slot: Option<u64>,
}

impl ResumePoint {
    pub(crate) fn new(commitment: ResumeCommitment) -> Self {
        let commitment = match commitment {
            ResumeCommitment::Confirmed => SlotStatus::SlotConfirmed,
            ResumeCommitment::Finalized => SlotStatus::SlotFinalized,
        };

        Self {
            commitment,
            slots: BTreeMap::new(),
            newest_complete: None,
            warned_open_slot: None,
        }
    }

    /// Where to subscribe from after a loss: `REPEAT_WINDOW` below the checkpoint or at an older
    /// parent that is not complete, but not below `configured`, since nothing before it came.
    pub(crate) fn resubscribe_slot(&self, configured: Option<u64>) -> Option<u64> {
        let Some(checkpoint) = self.checkpoint() else {
            return configured;
        };

        let floor = checkpoint.saturating_sub(MAX_TRACKED_SLOTS);

        // No status came yet, so the open slot's parent is not known and can still come late.
        if self.newest_complete.is_none() {
            return Some(floor).max(configured);
        }

        // Skipped slots can put a parent that comes after its child below the window. Below the
        // floor a complete parent has no record left.
        let start = self
            .slots
            .values()
            .filter_map(|record| record.parent)
            .filter(|parent| {
                *parent >= floor
                    && self
                        .slots
                        .get(parent)
                        .is_none_or(|record| record.state != SlotState::Complete)
            })
            .fold(checkpoint.saturating_sub(REPEAT_WINDOW), u64::min);

        Some(start).max(configured)
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
                    max_tracked_slots = MAX_TRACKED_SLOTS,
                    "No status at the commitment came for this open slot; newer slots past the \
                     cap are not recorded, and the checkpoint waits for it"
                );
                self.warned_open_slot = Some(open);
            }

            return true;
        }

        let record = self.slots.entry(slot).or_default();
        // yellowstone-grpc sends a slot's content before its status, so content or a commitment
        // status for a complete slot whose keys are gone is a repeat.
        if record.state == SlotState::Complete && record.seen.is_empty() {
            return role == Role::OtherStatus;
        }

        let fresh = record.mark_seen(key, &mut update.filters);

        // Below the checkpoint no slot opens, so a late copy never moves the checkpoint back.
        let below = checkpoint.is_some_and(|checkpoint| slot < checkpoint);
        match role {
            Role::Content if !below && record.state == SlotState::Idle => {
                record.state = SlotState::Open;
            },
            Role::CommitmentStatus { parent } => {
                record.state = SlotState::Complete;
                // A repeat from a server that names no parent must not erase a known one.
                record.parent = parent.or(record.parent);
                if !below {
                    self.newest_complete = self.newest_complete.max(Some(slot));
                }

                // Only a commitment status moves the checkpoint up or completes a slot below it.
                self.forget_below_window();
            },
            Role::Content | Role::OtherStatus => {},
        }

        fresh
    }

    /// Whether `slot`, which has no record, gets one. Below the checkpoint the limit is
    /// `REPEAT_WINDOW`, or `MAX_TRACKED_SLOTS` for a commitment status, which completes the slot,
    /// and for a named parent, which a resubscribe replays.
    fn may_record(&self, slot: u64, role: Role, checkpoint: Option<u64>) -> bool {
        let Some(checkpoint) = checkpoint else {
            // The status at the commitment can stall at the start while statuses of another
            // level keep coming.
            return role != Role::OtherStatus || self.slots.len() <= MAX_TRACKED_SLOTS as usize;
        };

        // Below the checkpoint, a slot that a resubscribe sends again gets a record, and so does a
        // complete slot, so that a late copy of it is dropped.
        if slot < checkpoint {
            let below = checkpoint - slot;
            return below <= REPEAT_WINDOW
                || (below <= MAX_TRACKED_SLOTS
                    && (matches!(role, Role::CommitmentStatus { .. })
                        || self.is_named_parent(slot)));
        }

        let Some(open) = self.oldest_open() else {
            // Content opens a slot that caps later records, and a commitment status frees old ones.
            return role != Role::OtherStatus || slot - checkpoint <= MAX_TRACKED_SLOTS;
        };

        slot.saturating_sub(open) <= MAX_TRACKED_SLOTS
    }

    fn is_named_parent(&self, slot: u64) -> bool {
        self.slots
            .values()
            .any(|record| record.parent == Some(slot))
    }

    /// Below the window a complete slot keeps its state and parent but not its keys, so a late
    /// copy is still dropped. A named parent that is not complete keeps its keys for its replay.
    /// Other records there, and all below `MAX_TRACKED_SLOTS`, go.
    fn forget_below_window(&mut self) {
        let Some(checkpoint) = self.checkpoint() else {
            return;
        };

        let window = checkpoint.saturating_sub(REPEAT_WINDOW);
        let floor = checkpoint.saturating_sub(MAX_TRACKED_SLOTS);
        let parents: HashSet<u64> = self
            .slots
            .values()
            .filter_map(|record| record.parent)
            .collect();
        self.slots.retain(|slot, record| {
            if *slot >= window {
                return true;
            }
            if *slot < floor {
                return false;
            }
            if record.state != SlotState::Complete {
                return parents.contains(slot);
            }

            record.seen = HashMap::new();
            true
        });
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use yellowstone_grpc_proto::geyser::{
        SubscribeUpdateAccount, SubscribeUpdateAccountInfo, SubscribeUpdateBlock,
        SubscribeUpdateBlockFooter, SubscribeUpdateSlot, SubscribeUpdateTransaction,
        SubscribeUpdateTransactionInfo,
    };

    use super::*;

    pub(crate) fn transaction(slot: u64, signature: u8) -> SubscribeUpdate {
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

    pub(crate) fn slot_status(slot: u64, status: SlotStatus) -> SubscribeUpdate {
        SubscribeUpdate {
            update_oneof: Some(UpdateOneof::Slot(SubscribeUpdateSlot {
                slot,
                status: status as i32,
                ..Default::default()
            })),
            ..Default::default()
        }
    }

    fn confirmed(slot: u64, parent: u64) -> SubscribeUpdate {
        let mut update = slot_status(slot, SlotStatus::SlotConfirmed);
        if let Some(UpdateOneof::Slot(status)) = update.update_oneof.as_mut() {
            status.parent = Some(parent);
        }
        update
    }

    /// Slot 10 is cut after two of its three updates. The replay repeats them and delivers the
    /// third, and content of slot 11 does not move the checkpoint past 10.
    #[test]
    fn replay_of_the_checkpoint_slot_delivers_only_what_was_missed() {
        let mut resume = ResumePoint::new(ResumeCommitment::Confirmed);
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
        let mut resume = ResumePoint::new(ResumeCommitment::Confirmed);
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

        let mut resume = ResumePoint::new(ResumeCommitment::Confirmed);
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

        let mut resume = ResumePoint::new(ResumeCommitment::Confirmed);
        assert!(resume.deliver(&mut footer(101)));
        assert!(!resume.deliver(&mut footer(900)));
    }

    /// An interslot status for a slot ahead must not move the checkpoint past content that has
    /// not arrived, but a status at the stream's commitment does.
    #[test]
    fn only_a_status_at_the_commitment_moves_the_checkpoint() {
        let mut resume = ResumePoint::new(ResumeCommitment::Finalized);
        assert!(resume.deliver(&mut transaction(10, 1)));
        assert!(resume.deliver(&mut slot_status(15, SlotStatus::SlotFirstShredReceived)));
        assert_eq!(resume.checkpoint(), Some(10));

        assert!(resume.deliver(&mut slot_status(10, SlotStatus::SlotFinalized)));
        assert!(resume.deliver(&mut slot_status(12, SlotStatus::SlotFinalized)));
        assert_eq!(resume.checkpoint(), Some(12));
    }

    /// yellowstone-grpc can send a parent after its child is complete. After a loss inside the
    /// parent, the source subscribes again from below it, and the replay drops what it delivered.
    #[test]
    fn resubscribe_covers_a_parent_sent_after_its_child() {
        let mut resume = ResumePoint::new(ResumeCommitment::Confirmed);
        assert!(resume.deliver(&mut transaction(43, 1)));
        assert!(resume.deliver(&mut slot_status(43, SlotStatus::SlotConfirmed)));
        assert!(resume.deliver(&mut transaction(42, 1)));
        assert_eq!(resume.checkpoint(), Some(43));
        assert!(resume.resubscribe_slot(None).is_some_and(|slot| slot <= 42));

        assert!(!resume.deliver(&mut transaction(42, 1)));
    }

    /// A slot whose status never comes holds the checkpoint, so a later reconnect fails with
    /// `OutOfRange` instead of skipping it. Slots more than `MAX_TRACKED_SLOTS` above are not kept.
    #[test]
    fn checkpoint_holds_a_slot_whose_status_never_comes() {
        let mut resume = ResumePoint::new(ResumeCommitment::Confirmed);
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
        let mut resume = ResumePoint::new(ResumeCommitment::Confirmed);
        assert!(resume.deliver(&mut account_with_signature(10, 5, 7)));
        assert!(!resume.deliver(&mut account_with_signature(10, 9_000_005, 7)));
    }

    /// Writes of one account by distinct transactions are distinct updates.
    #[test]
    fn account_writes_with_distinct_signatures_are_kept() {
        let mut resume = ResumePoint::new(ResumeCommitment::Confirmed);
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

        let mut resume = ResumePoint::new(ResumeCommitment::Confirmed);
        assert!(resume.deliver(&mut footer(&["a"])));

        let mut repeat = footer(&["a", "b"]);
        assert!(resume.deliver(&mut repeat));
        assert_eq!(repeat.filters, ["b"]);
    }

    /// Below the window, a late copy of a complete slot is dropped and a status of another level
    /// passes. A slot there that never completed, such as a late parent, is still delivered.
    #[test]
    fn late_copy_below_the_window_does_not_reopen_a_complete_slot() {
        let mut resume = ResumePoint::new(ResumeCommitment::Confirmed);
        for slot in (10..=20).filter(|slot| *slot != 14) {
            assert!(resume.deliver(&mut transaction(slot, 1)));
            assert!(resume.deliver(&mut slot_status(slot, SlotStatus::SlotConfirmed)));
        }

        assert!(!resume.deliver(&mut transaction(17, 1)));
        for slot in [15, 12] {
            assert!(!resume.deliver(&mut transaction(slot, 1)));
            assert_eq!(resume.checkpoint(), Some(20));
        }
        assert!(resume.deliver(&mut slot_status(12, SlotStatus::SlotFinalized)));

        assert!(resume.deliver(&mut transaction(14, 1)));
        assert_eq!(resume.checkpoint(), Some(20));
    }

    /// Skipped leaders can leave more than `MAX_TRACKED_SLOTS` slots with no status.
    #[test]
    fn checkpoint_moves_past_a_run_of_skipped_slots() {
        let mut resume = ResumePoint::new(ResumeCommitment::Confirmed);
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
        let mut resume = ResumePoint::new(ResumeCommitment::Confirmed);
        let mut processed = slot_status(10, SlotStatus::SlotProcessed);
        assert!(resume.deliver(&mut processed));
        assert!(resume.deliver(&mut slot_status(10, SlotStatus::SlotConfirmed)));

        assert!(!resume.deliver(&mut processed));
    }

    /// Each bank of a slot has its own processed status, so the second is not a repeat. A slot
    /// has one confirmed bank, so its status repeats whatever id another server gives it.
    #[test]
    fn processed_statuses_of_two_banks_in_one_slot_are_kept() {
        let status_of = |status, bank_id| {
            let mut update = slot_status(10, status);
            if let Some(UpdateOneof::Slot(slot)) = update.update_oneof.as_mut() {
                slot.bank_id = Some(bank_id);
            }
            update
        };

        let mut resume = ResumePoint::new(ResumeCommitment::Confirmed);
        assert!(resume.deliver(&mut status_of(SlotStatus::SlotProcessed, 7)));
        assert!(resume.deliver(&mut status_of(SlotStatus::SlotProcessed, 8)));
        assert!(!resume.deliver(&mut status_of(SlotStatus::SlotProcessed, 8)));

        assert!(resume.deliver(&mut status_of(SlotStatus::SlotConfirmed, 8)));
        assert!(!resume.deliver(&mut status_of(SlotStatus::SlotConfirmed, 900)));
    }

    /// `Dead` names no bank. A slot that dies, replays and dies again sends a second `Dead`,
    /// which is not a repeat.
    #[test]
    fn second_dead_of_a_slot_is_delivered() {
        let mut resume = ResumePoint::new(ResumeCommitment::Confirmed);
        assert!(resume.deliver(&mut slot_status(10, SlotStatus::SlotConfirmed)));
        assert!(resume.deliver(&mut slot_status(11, SlotStatus::SlotDead)));
        assert!(resume.deliver(&mut slot_status(11, SlotStatus::SlotDead)));
    }

    /// Content that keeps coming while the statuses stop must not grow the state.
    #[test]
    fn content_without_commitment_status_keeps_state_bounded() {
        let mut resume = ResumePoint::new(ResumeCommitment::Confirmed);
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
        let mut resume = ResumePoint::new(ResumeCommitment::Confirmed);
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
        let mut resume = ResumePoint::new(ResumeCommitment::Finalized);
        for slot in 0..10_000 {
            assert!(resume.deliver(&mut slot_status(slot, SlotStatus::SlotProcessed)));
        }

        assert_eq!(resume.checkpoint(), None);
        assert!(resume.slots.len() <= MAX_TRACKED_SLOTS as usize + 1);
    }

    /// Skipped slots can put a parent more than `REPEAT_WINDOW` below its child, also for the first
    /// slot of the stream. The resubscribe goes back to it, and its replay drops the repeats.
    #[test]
    fn resubscribe_covers_a_parent_across_skipped_slots() {
        let mut resume = ResumePoint::new(ResumeCommitment::Confirmed);
        assert!(resume.deliver(&mut transaction(45, 1)));
        assert!(resume.deliver(&mut confirmed(45, 40)));
        assert!(resume.resubscribe_slot(None).is_some_and(|slot| slot <= 40));

        // A repeat from a server that names no parent must not erase the parent.
        assert!(!resume.deliver(&mut slot_status(45, SlotStatus::SlotConfirmed)));
        assert!(resume.resubscribe_slot(None).is_some_and(|slot| slot <= 40));

        assert!(resume.deliver(&mut slot_status(39, SlotStatus::SlotConfirmed)));
        assert!(resume.deliver(&mut transaction(40, 1)));
        assert!(!resume.deliver(&mut transaction(40, 1)));
        assert!(resume.deliver(&mut confirmed(40, 39)));
        assert_eq!(resume.resubscribe_slot(None), Some(45 - REPEAT_WINDOW));
    }

    /// A parent counts down to `MAX_TRACKED_SLOTS` below the checkpoint and no further.
    #[test]
    fn resubscribe_counts_a_parent_down_to_the_floor() {
        for (slot, parent, start) in [(100, 68, 68), (100, 67, 96), (1, 0, 0)] {
            let mut resume = ResumePoint::new(ResumeCommitment::Confirmed);
            assert!(resume.deliver(&mut confirmed(slot, parent)));
            assert_eq!(resume.resubscribe_slot(None), Some(start));
        }
    }

    /// Before the first status, the parent of the open slot is not known and can come late. A
    /// loss then replays the full tracked range, but not from before the configured start.
    #[test]
    fn resubscribe_before_the_first_status_covers_an_unknown_parent() {
        let mut resume = ResumePoint::new(ResumeCommitment::Confirmed);
        assert!(resume.deliver(&mut transaction(45, 1)));
        assert_eq!(resume.resubscribe_slot(None), Some(45 - MAX_TRACKED_SLOTS));
        assert_eq!(resume.resubscribe_slot(Some(43)), Some(43));
    }

    /// Each status names the slot before it as parent. A complete parent below the record floor
    /// has no record, so it must not pull the resubscribe below the window.
    #[test]
    fn a_long_stream_resubscribes_from_the_window() {
        let mut resume = ResumePoint::new(ResumeCommitment::Confirmed);
        for slot in 1000..=1100 {
            assert!(resume.deliver(&mut transaction(slot, 1)));
            assert!(resume.deliver(&mut confirmed(slot, slot - 1)));
        }
        assert_eq!(resume.resubscribe_slot(None), Some(1100 - REPEAT_WINDOW));

        for slot in 1100 - REPEAT_WINDOW..=1100 {
            assert!(!resume.deliver(&mut transaction(slot, 1)));
            assert!(!resume.deliver(&mut confirmed(slot, slot - 1)));
        }
    }
}
