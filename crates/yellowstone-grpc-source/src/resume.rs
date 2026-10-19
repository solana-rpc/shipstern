//! Slot-checkpoint resume for a confirmed or finalized stream.
//!
//! A slot at those commitments never changes once delivered, so a lost stream is recovered
//! by subscribing again from the last slot seen and dropping that slot's repeats. Everything
//! after it is new by construction.

use std::collections::HashSet;

use yellowstone_grpc_proto::geyser::{subscribe_update::UpdateOneof, SlotStatus, SubscribeUpdate};

/// What tells two updates of one slot apart.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum UpdateKey {
    Account { pubkey: Vec<u8>, write_version: u64 },
    Transaction(Vec<u8>),
    TransactionStatus(Vec<u8>),
    Entry(u64),
    Slot(i32),
    BlockMeta,
    Block,
    BlockFooter(u64),
}

/// The slot an update belongs to, its key in that slot, and whether it moves the checkpoint.
/// A slot status of another level can name a slot whose content has not arrived, so only a
/// status at `commitment` moves it. A ping, a pong or a parent update has no slot.
fn classify(update: &SubscribeUpdate, commitment: SlotStatus) -> Option<(u64, UpdateKey, bool)> {
    let classified = match update.update_oneof.as_ref()? {
        UpdateOneof::Account(account) => {
            let info = account.account.as_ref()?;
            let key = UpdateKey::Account {
                pubkey: info.pubkey.clone(),
                write_version: info.write_version,
            };
            (account.slot, key, true)
        },
        UpdateOneof::Transaction(tx) => {
            let signature = tx.transaction.as_ref()?.signature.clone();
            (tx.slot, UpdateKey::Transaction(signature), true)
        },
        UpdateOneof::TransactionStatus(tx) => {
            let key = UpdateKey::TransactionStatus(tx.signature.clone());
            (tx.slot, key, true)
        },
        UpdateOneof::Entry(entry) => (entry.slot, UpdateKey::Entry(entry.index), true),
        UpdateOneof::Slot(slot) => {
            let at_commitment = slot.status == commitment as i32;
            (slot.slot, UpdateKey::Slot(slot.status), at_commitment)
        },
        UpdateOneof::BlockMeta(meta) => (meta.slot, UpdateKey::BlockMeta, true),
        UpdateOneof::Block(block) => (block.slot, UpdateKey::Block, true),
        UpdateOneof::BlockFooter(footer) => {
            (footer.slot, UpdateKey::BlockFooter(footer.bank_id), true)
        },
        UpdateOneof::EntryUpdateParent(_) | UpdateOneof::Ping(_) | UpdateOneof::Pong(_) => {
            return None;
        },
    };
    Some(classified)
}

/// The last slot seen and the keys delivered for it. A resubscribe from that slot repeats
/// them, and nothing else, so only this one set is kept.
#[derive(Debug)]
pub(crate) struct ResumePoint {
    commitment: SlotStatus,
    slot: Option<u64>,
    seen: HashSet<UpdateKey>,
}

impl ResumePoint {
    pub(crate) fn new(commitment: SlotStatus) -> Self {
        Self {
            commitment,
            slot: None,
            seen: HashSet::new(),
        }
    }

    /// The slot to subscribe from after a loss, inclusive.
    pub(crate) fn checkpoint(&self) -> Option<u64> { self.slot }

    /// Note `update` and say whether to deliver it. A repeat of the checkpoint slot is dropped.
    pub(crate) fn deliver(&mut self, update: &SubscribeUpdate) -> bool {
        let Some((slot, key, moves_checkpoint)) = classify(update, self.commitment) else {
            return true;
        };
        match self.slot {
            Some(current) if slot == current => self.seen.insert(key),
            // Behind the checkpoint, or ahead of it without moving it: nothing to compare to.
            Some(current) if slot < current || !moves_checkpoint => true,
            _ => {
                self.slot = Some(slot);
                self.seen.clear();
                self.seen.insert(key);
                true
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use yellowstone_grpc_proto::geyser::{
        SubscribeUpdateAccount, SubscribeUpdateAccountInfo, SubscribeUpdateSlot,
        SubscribeUpdateTransaction, SubscribeUpdateTransactionInfo,
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

    /// An interslot status for a slot ahead must not move the checkpoint past content that has
    /// not arrived, but a status at the stream's commitment does.
    #[test]
    fn only_a_status_at_the_commitment_moves_the_checkpoint() {
        let mut resume = ResumePoint::new(SlotStatus::SlotFinalized);
        assert!(resume.deliver(&transaction(10, 1)));
        assert!(resume.deliver(&slot_status(15, SlotStatus::SlotFirstShredReceived)));
        assert_eq!(resume.checkpoint(), Some(10));

        assert!(resume.deliver(&slot_status(12, SlotStatus::SlotFinalized)));
        assert_eq!(resume.checkpoint(), Some(12));
    }
}
