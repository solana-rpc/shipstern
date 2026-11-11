//!
//! Which bank of a slot is committed. Block machine 0.11 finalizes the ancestors of a finalized
//! slot by guessing the sole bank it has seen for each, which can be a loser whose winner has not
//! arrived yet. So the coordinator decides itself: a bank is committed by its own commitment
//! status, or by a committed child whose parent blockhash names it. The block machine gets only
//! these commitments, and a finalized one only once its whole ancestry is known.
//!

use std::collections::{btree_map::Entry, BTreeMap};

use solana_clock::Slot;
use solana_commitment_config::CommitmentLevel;
use yellowstone_block_machine::event::{BlockMetaEvInfo, SlotStatusKind};

use crate::types::{BankKey, CoordinatorError};

/// A bank's place in the chain, from its BlockMeta.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Identity {
    parent_slot: Slot,
    blockhash: [u8; 32],
    parent_blockhash: [u8; 32],
}

/// A commitment to pass to the block machine.
pub(crate) struct Forward {
    pub key: BankKey,
    pub parent_slot: Option<Slot>,
    pub status: SlotStatusKind,
}

#[derive(Default)]
pub(crate) struct Commitments {
    identities: BTreeMap<BankKey, Identity>,
    levels: BTreeMap<BankKey, CommitmentLevel>,
    /// The parent a bank's own status named, for a bank whose BlockMeta has not come yet.
    status_parents: BTreeMap<BankKey, Slot>,
    forwarded: BTreeMap<BankKey, CommitmentLevel>,
}

/// How many slots behind the flush frontier identities and commitments stay.
const FLUSHED_RETENTION: Slot = 512;

const fn rank(level: CommitmentLevel) -> u8 {
    match level {
        CommitmentLevel::Processed => 0,
        CommitmentLevel::Confirmed => 1,
        CommitmentLevel::Finalized => 2,
    }
}

const fn conflict(key: BankKey) -> CoordinatorError {
    CoordinatorError::ConflictingBlockIdentity {
        slot: key.slot,
        bank_id: key.bank_id,
    }
}

impl Commitments {
    /// Keep the bank's identity. A repeated BlockMeta must not change it.
    pub fn note_identity(&mut self, meta: &BlockMetaEvInfo) -> Result<(), CoordinatorError> {
        let key = BankKey::new(meta.slot, meta.bank_id);
        let identity = Identity {
            parent_slot: meta.parent_slot,
            blockhash: meta.blockhash,
            parent_blockhash: meta.parent_blockhash,
        };
        if *self.identities.entry(key).or_insert(identity) != identity {
            return Err(conflict(key));
        }
        Ok(())
    }

    /// Commit `key` at `level`. Returns whether that raised its level. One slot has at most one
    /// committed bank.
    pub fn record(
        &mut self,
        key: BankKey,
        level: CommitmentLevel,
        parent_slot: Option<Slot>,
    ) -> Result<bool, CoordinatorError> {
        if self
            .levels
            .range(BankKey::slot_range(key.slot))
            .any(|(&other, _)| other != key)
        {
            return Err(conflict(key));
        }
        if let Some(parent_slot) = parent_slot {
            self.status_parents.entry(key).or_insert(parent_slot);
        }
        let known = self.levels.entry(key).or_insert(CommitmentLevel::Processed);
        if rank(level) <= rank(*known) {
            return Ok(false);
        }
        *known = level;
        Ok(true)
    }

    pub fn contains(&self, key: BankKey) -> bool { self.levels.contains_key(&key) }

    /// How many banks above `frontier` the ledger tracks, and the oldest one's slot. A stream
    /// that commits or describes banks that never flush must still reach the pending-bank limit.
    /// An invalidated or losing bank is resolved, so it does not count.
    pub fn unflushed(
        &self,
        frontier: Option<Slot>,
        is_invalidated: impl Fn(BankKey) -> bool,
    ) -> (usize, Option<Slot>) {
        let from = BankKey::first(frontier.map_or(0, |last| last.saturating_add(1)));
        let mut keys: Vec<BankKey> = self
            .identities
            .range(from..)
            .map(|(&key, _)| key)
            // Another bank of the slot committed, so this one lost, tombstone or not.
            .filter(|&key| {
                !is_invalidated(key)
                    && self
                        .levels
                        .range(BankKey::slot_range(key.slot))
                        .all(|(&committed, _)| committed == key)
            })
            .chain(self.levels.range(from..).map(|(&key, _)| key))
            .collect();
        keys.sort_unstable();
        keys.dedup();
        (keys.len(), keys.first().map(|key| key.slot))
    }

    /// Whether the block machine's status for `key` at `level` is one the coordinator verified.
    pub fn allows(&self, key: BankKey, level: CommitmentLevel) -> bool {
        self.levels
            .get(&key)
            .is_some_and(|&known| rank(level) <= rank(known))
    }

    /// A cleared bank can no longer be a parent.
    pub fn forget(&mut self, key: BankKey) { self.identities.remove(&key); }

    /// Drop what both outputs flushed through, keeping recent blocks so a late child can still
    /// be checked against the parent that flushed.
    pub fn prune_through(&mut self, slot: Slot) {
        let first_kept = BankKey::first(slot.saturating_sub(FLUSHED_RETENTION).saturating_add(1));
        self.identities = self.identities.split_off(&first_kept);
        self.levels = self.levels.split_off(&first_kept);
        self.status_parents = self.status_parents.split_off(&first_kept);
        self.forwarded = self.forwarded.split_off(&first_kept);
    }

    ///
    /// Commit the parents that committed children name by blockhash, and return the commitments
    /// the block machine has not had yet. Every confirm goes first, so the block machine picks
    /// each slot's bank before a finalize roots anything.
    ///
    /// `before_stream` tells whether a slot came before the stream, so its block is not needed.
    pub fn reconcile(
        &mut self,
        frontier: Option<Slot>,
        before_stream: impl Fn(Slot) -> bool,
        is_invalidated: impl Fn(BankKey) -> bool,
    ) -> Result<Vec<Forward>, CoordinatorError> {
        let pending = |slot: Slot| frontier.is_none_or(|last| slot > last);
        let mut queue: Vec<(BankKey, CommitmentLevel)> = self
            .levels
            .iter()
            .filter(|(key, _)| pending(key.slot))
            .map(|(&key, &level)| (key, level))
            .collect();
        while let Some((key, level)) = queue.pop() {
            let Some(identity) = self.identities.get(&key).copied() else {
                continue;
            };
            // Nothing to commit for a parent from before the stream.
            if before_stream(identity.parent_slot) {
                continue;
            }
            let committed = self
                .levels
                .range(BankKey::slot_range(identity.parent_slot))
                .next()
                .map(|(&parent, _)| parent);
            let parent = match committed {
                // A committed parent, flushed or not, must be the block the child names.
                Some(parent) => {
                    let Some(parent_identity) = self.identities.get(&parent) else {
                        continue;
                    };
                    if parent_identity.blockhash != identity.parent_blockhash {
                        return Err(conflict(key));
                    }
                    parent
                },
                None if !pending(identity.parent_slot) => continue,
                None => {
                    let mut named = self
                        .identities
                        .range(BankKey::slot_range(identity.parent_slot))
                        .filter(|(candidate, candidate_identity)| {
                            candidate_identity.blockhash == identity.parent_blockhash
                                && !is_invalidated(**candidate)
                        })
                        .map(|(&candidate, _)| candidate);
                    let (Some(parent), None) = (named.next(), named.next()) else {
                        continue;
                    };
                    parent
                },
            };
            if self.record(parent, level, None)? && pending(parent.slot) {
                queue.push((parent, level));
            }
        }

        let mut forwards = Vec::new();
        let keys: Vec<BankKey> = self.levels.keys().copied().collect();
        for &key in keys.iter().filter(|key| pending(key.slot)) {
            if let Entry::Vacant(forwarded) = self.forwarded.entry(key) {
                forwarded.insert(CommitmentLevel::Confirmed);
                forwards.push(self.forward(key, SlotStatusKind::Confirmed));
            }
        }
        // Finality of a bank that already flushed at confirmed still roots the block machine's
        // fork graph, which only shrinks behind a root.
        for key in keys {
            if self.levels.get(&key) == Some(&CommitmentLevel::Finalized)
                && self.forwarded.get(&key) != Some(&CommitmentLevel::Finalized)
                && self.ancestry_known(key, frontier, &before_stream)
            {
                self.forwarded.insert(key, CommitmentLevel::Finalized);
                forwards.push(self.forward(key, SlotStatusKind::Finalized));
            }
        }
        Ok(forwards)
    }

    fn forward(&self, key: BankKey, status: SlotStatusKind) -> Forward {
        Forward {
            key,
            parent_slot: self
                .identities
                .get(&key)
                .map(|identity| identity.parent_slot)
                .or_else(|| self.status_parents.get(&key).copied()),
            status,
        }
    }

    /// Every ancestor of `key` back to the flushed chain, or to before the stream, is committed
    /// and linked by blockhash.
    fn ancestry_known(
        &self,
        mut key: BankKey,
        frontier: Option<Slot>,
        before_stream: &impl Fn(Slot) -> bool,
    ) -> bool {
        loop {
            let Some(identity) = self.identities.get(&key) else {
                return false;
            };
            if frontier.is_some_and(|last| identity.parent_slot <= last)
                || before_stream(identity.parent_slot)
            {
                return true;
            }
            let Some((&parent, _)) = self
                .levels
                .range(BankKey::slot_range(identity.parent_slot))
                .next()
            else {
                return false;
            };
            if self.identities.get(&parent).is_none_or(|parent_identity| {
                parent_identity.blockhash != identity.parent_blockhash
            }) {
                return false;
            }
            key = parent;
        }
    }
}
