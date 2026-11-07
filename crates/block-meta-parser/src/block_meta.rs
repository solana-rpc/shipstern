use std::borrow::Cow;

use shipstern_core::{ParseResult, Parser, Prefilter, ProgramParser, Pubkey};
use shipstern_proc_macro::shipstern;
use yellowstone_grpc_proto::geyser::SubscribeUpdateBlockMeta;

///
/// One reward entry of a block.
///
/// `reward_type` is the raw proto value of `RewardType`. A `RewardType::VatDebit` entry has
/// negative `lamports`. The same pubkey then also has a `RewardType::Voting` entry after it,
/// so the first `post_balance` is an intermediate balance.
///
#[shipstern]
#[derive(Clone, PartialEq)]
pub struct Reward {
    pub pubkey: String,
    pub lamports: i64,
    pub post_balance: u64,
    pub reward_type: i32,
    pub commission: String,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct Rewards {
    pub rewards: Vec<Reward>,
    pub num_partitions: ::core::option::Option<u64>,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct BlockMetaUpdate {
    pub slot: u64,
    pub blockhash: String,
    pub rewards: ::core::option::Option<Rewards>,
    pub block_time: ::core::option::Option<i64>,
    pub block_height: ::core::option::Option<u64>,
    pub parent_slot: u64,
    pub parent_blockhash: String,
    pub executed_transaction_count: u64,
    pub entries_count: u64,
    /// The bank of this block. Bank IDs are local to a connection. On the gRPC source, join with
    /// the slot and footer updates of the same stream on (`slot`, `bank_id`).
    pub bank_id: u64,
}

#[derive(Debug, Clone, Copy)]
pub struct BlockMetaParser;

impl Parser for BlockMetaParser {
    type Input = SubscribeUpdateBlockMeta;
    type Output = BlockMetaUpdate;

    fn id(&self) -> Cow<'static, str> { "yellowstone::BlockMetaParser".into() }

    fn prefilter(&self) -> Prefilter { Prefilter::builder().block_metas().build().unwrap() }

    async fn parse(&self, block_meta: &SubscribeUpdateBlockMeta) -> ParseResult<Self::Output> {
        let rewards = block_meta.rewards.as_ref().map(|r| Rewards {
            rewards: r
                .rewards
                .iter()
                .map(|reward| Reward {
                    pubkey: reward.pubkey.clone(),
                    lamports: reward.lamports,
                    post_balance: reward.post_balance,
                    reward_type: reward.reward_type,
                    commission: reward.commission.clone(),
                })
                .collect(),
            num_partitions: r.num_partitions.map(|n| n.num_partitions),
        });

        Ok(BlockMetaUpdate {
            slot: block_meta.slot,
            blockhash: block_meta.blockhash.clone(),
            rewards,
            block_time: block_meta.block_time.map(|t| t.timestamp),
            block_height: block_meta.block_height.map(|h| h.block_height),
            parent_slot: block_meta.parent_slot,
            parent_blockhash: block_meta.parent_blockhash.clone(),
            executed_transaction_count: block_meta.executed_transaction_count,
            entries_count: block_meta.entries_count,
            bank_id: block_meta.bank_id,
        })
    }
}

impl ProgramParser for BlockMetaParser {
    #[inline]
    fn program_id(&self) -> Pubkey {
        "B111111111111111111111111111111111111111112"
            .parse()
            .unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn parse_keeps_bank_id() {
        let block_meta = SubscribeUpdateBlockMeta {
            slot: 7,
            bank_id: 3,
            ..Default::default()
        };

        let parsed = BlockMetaParser.parse(&block_meta).await.unwrap();

        assert_eq!((parsed.slot, parsed.bank_id), (7, 3));
    }
}
