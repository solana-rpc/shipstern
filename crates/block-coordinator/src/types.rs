use std::fmt;

use smallvec::SmallVec;
use solana_clock::Slot;
use solana_hash::Hash;
use yellowstone_grpc_proto::geyser::SubscribeUpdate;

/// Block metadata that always transitions together.
#[derive(Debug, Clone)]
pub struct BlockMetadata {
    pub parent_slot: Slot,
    pub blockhash: Hash,
    pub expected_tx_count: u64,
}

/// Reason a slot was discarded by the coordinator.
#[derive(Debug, Clone, Copy)]
pub enum DiscardReason {
    Dead,
    Forked,
    Incomplete,
}

impl fmt::Display for DiscardReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            Self::Dead => "dead",
            Self::Forked => "forked",
            Self::Incomplete => "incomplete",
        };
        f.write_str(label)
    }
}

/// Coordinator invariants and unrecoverable errors.
#[derive(Debug)]
pub enum CoordinatorError {
    ConfirmedBankDiscarded {
        slot: Slot,
        bank_id: u64,
    },
    ConflictingConfirmedBank {
        slot: Slot,
        existing: u64,
        new: u64,
    },
    TwoGateInvariantViolation {
        slot: Slot,
        last_flushed: Option<Slot>,
    },
    ReadySlotMissingMetadata {
        slot: Slot,
    },
    InstructionOutputChannelClosed {
        slot: Slot,
    },
    AccountOutputChannelClosed {
        slot: Slot,
    },
}

impl fmt::Display for CoordinatorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ConfirmedBankDiscarded { slot, bank_id } => {
                write!(f, "confirmed bank {bank_id} discarded in slot {slot}")
            },
            Self::ConflictingConfirmedBank {
                slot,
                existing,
                new,
            } => write!(f, "slot {slot} confirmed bank {new} after bank {existing}"),
            Self::TwoGateInvariantViolation { slot, last_flushed } => write!(
                f,
                "Two-gate invariant violated: slot {slot} <= last_flushed {last_flushed:?}"
            ),
            Self::ReadySlotMissingMetadata { slot } => {
                write!(f, "Ready slot missing metadata: slot {slot}")
            },
            Self::InstructionOutputChannelClosed { slot } => {
                write!(
                    f,
                    "Instruction output channel closed while sending slot {slot}"
                )
            },
            Self::AccountOutputChannelClosed { slot } => {
                write!(f, "Account output channel closed while sending slot {slot}")
            },
        }
    }
}

impl std::error::Error for CoordinatorError {}

/// Categorizes parse outcomes for stats tracking.
#[derive(Debug, Clone, Copy)]
pub enum ParseStatsKind {
    InstructionFiltered,
    InstructionError,
    AccountFiltered,
    AccountError,
    TransactionStatusFailed,
    TransactionStatusSucceeded,
}

///
/// Messages from handlers back to the coordinator.
///
/// `bank_id` comes from the update the handler parsed. Records stay per bank until
/// one bank of the slot is confirmed.
///
pub enum CoordinatorMessage<R> {
    /// A parsed instruction record ready to buffer (sorted by tx_index, ix_path).
    InstructionParsed {
        slot: Slot,
        bank_id: u64,
        key: InstructionRecordSortKey,
        record: R,
    },
    /// A parsed account record ready to buffer, sorted by ingress_seq:pubkey.
    AccountParsed {
        slot: Slot,
        bank_id: u64,
        key: AccountRecordSortKey,
        record: R,
    },
    /// Signal that a transaction has been fully parsed by the handler.
    /// Coordinator counts these to determine when a slot is fully parsed.
    TransactionParsed { slot: Slot, bank_id: u64 },
    /// A parse stat event (filtered or error) for aggregate tracking.
    ParseStats {
        slot: Slot,
        bank_id: u64,
        kind: ParseStatsKind,
    },
}

impl<R> CoordinatorMessage<R> {
    pub fn slot(&self) -> Slot {
        match self {
            Self::InstructionParsed { slot, .. }
            | Self::AccountParsed { slot, .. }
            | Self::TransactionParsed { slot, .. }
            | Self::ParseStats { slot, .. } => *slot,
        }
    }
}

/// Sort key for records within a slot.
/// Ordered by transaction index, then instruction path (depth-first CPI order).
/// SmallVec sorts lexicographically: `[0] < [0,0] < [0,1] < [1]`, which matches
/// depth-first execution order. Inline storage for up to 4 elements avoids
/// heap allocation (Solana CPI depth is capped at 4).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct InstructionRecordSortKey {
    tx_index: u64,
    ix_path: SmallVec<[usize; 4]>,
}

impl InstructionRecordSortKey {
    pub fn new(tx_index: u64, ix_path: Vec<usize>) -> Self {
        Self {
            tx_index,
            ix_path: SmallVec::from_vec(ix_path),
        }
    }
}

/// Sort key for account records within a slot.
/// Ordered by write_version (geyser-assigned version number for
/// deterministic ordering), then pubkey (discriminate multiple accounts
/// in the same update). Slot is implicit (it's the buffer's key).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct AccountRecordSortKey {
    write_version: u64,
    pubkey: [u8; 32],
}

impl AccountRecordSortKey {
    pub fn new(write_version: u64, pubkey: [u8; 32]) -> Self {
        Self {
            write_version,
            pubkey,
        }
    }
}

/// Input messages from the source to the coordinator.
/// Wraps raw gRPC events plus synthetic messages injected by the source.
pub enum CoordinatorInput {
    /// A raw geyser SubscribeUpdate (Entry, Slot, BlockMeta).
    GeyserUpdate(Box<SubscribeUpdate>),
    /// A raw Account event was seen on the geyser stream for this bank.
    /// Lightweight signal — only the slot and bank, no protobuf payload.
    AccountEventSeen { slot: Slot, bank_id: u64 },
}

/// Wraps a slot number with a deterministic ANSI color for log readability.
/// Consecutive slots get different colors so interleaved events are easy to follow.
pub struct ColorSlot(pub Slot);

impl fmt::Display for ColorSlot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        const COLORS: [&str; 6] = [
            "\x1b[91m", // red
            "\x1b[92m", // green
            "\x1b[93m", // yellow
            "\x1b[94m", // blue
            "\x1b[95m", // magenta
            "\x1b[96m", // cyan
        ];
        let color = COLORS[(self.0 % COLORS.len() as u64) as usize];
        write!(f, "{}{}\x1b[0m", color, self.0)
    }
}

/// Account commitment configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum AccountCommitAt {
    /// Accounts flush when the slot is confirmed (same timing as instructions).
    #[default]
    Confirmed,
    /// Accounts flush when the slot is finalized.
    Finalized,
}

/// How accounts are processed and output.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub enum AccountMode {
    /// Accounts in the same processed subscription. Commit triggered by a specific event.
    Processed { commit_at: AccountCommitAt },
    /// Separate finalized subscription, pass-through writes (no buffering).
    #[default]
    FinalizedPassthrough,
}

/// An instruction slot ready for downstream consumption (e.g., Kafka write).
pub struct InstructionSlot<R> {
    pub slot: Slot,
    pub parent_slot: Slot,
    pub blockhash: Hash,
    pub executed_transaction_count: u64,
    pub records: Vec<R>,
    pub filtered_instruction_count: u64,
    pub failed_instruction_count: u64,
    pub transaction_status_failed_count: u64,
    pub transaction_status_succeeded_count: u64,
}

/// An account slot ready for downstream consumption.
pub struct AccountSlot<R> {
    pub slot: Slot,
    pub records: Vec<R>,
    pub decoded_account_count: u64,
    pub filtered_account_count: u64,
    pub failed_account_count: u64,
}

/// Cloneable handle for handlers to send messages to the coordinator.
#[derive(Clone)]
pub struct CoordinatorHandle<R> {
    tx: tokio::sync::mpsc::Sender<CoordinatorMessage<R>>,
}

impl<R: Send> CoordinatorHandle<R> {
    pub fn new(tx: tokio::sync::mpsc::Sender<CoordinatorMessage<R>>) -> Self { Self { tx } }

    pub async fn send_instruction_parsed(
        &self,
        slot: Slot,
        bank_id: u64,
        key: InstructionRecordSortKey,
        record: R,
    ) -> Result<(), tokio::sync::mpsc::error::SendError<CoordinatorMessage<R>>> {
        self.tx
            .send(CoordinatorMessage::InstructionParsed {
                slot,
                bank_id,
                key,
                record,
            })
            .await
    }

    pub async fn send_account_parsed(
        &self,
        slot: Slot,
        bank_id: u64,
        key: AccountRecordSortKey,
        record: R,
    ) -> Result<(), tokio::sync::mpsc::error::SendError<CoordinatorMessage<R>>> {
        self.tx
            .send(CoordinatorMessage::AccountParsed {
                slot,
                bank_id,
                key,
                record,
            })
            .await
    }

    pub async fn send_transaction_parsed(
        &self,
        slot: Slot,
        bank_id: u64,
    ) -> Result<(), tokio::sync::mpsc::error::SendError<CoordinatorMessage<R>>> {
        self.tx
            .send(CoordinatorMessage::TransactionParsed { slot, bank_id })
            .await
    }

    pub async fn send_parse_stats(
        &self,
        slot: Slot,
        bank_id: u64,
        kind: ParseStatsKind,
    ) -> Result<(), tokio::sync::mpsc::error::SendError<CoordinatorMessage<R>>> {
        self.tx
            .send(CoordinatorMessage::ParseStats {
                slot,
                bank_id,
                kind,
            })
            .await
    }
}
