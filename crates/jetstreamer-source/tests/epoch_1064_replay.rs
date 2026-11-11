use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    time::Duration,
};

use shipstern::sources::{FromConfig, SourceContext, SourceTrait};
use shipstern_core::{BlockMetaPrefilter, Filters, Prefilter};
use shipstern_jetstream_source::{
    init_process_env, JetstreamSource, JetstreamSourceConfig, SlotRangeConfig,
};
use tokio::sync::mpsc;
use yellowstone_grpc_proto::{
    geyser::{subscribe_update::UpdateOneof, SubscribeUpdateBlockMeta},
    solana::storage::confirmed_block::RewardType,
};

/// First slot of epoch 1064. From Alpenglow on, the first block of an epoch
/// burns the validator admission ticket as `VATDebit` rewards.
const EPOCH_1064_FIRST_SLOT: u64 = 1064 * 432_000;

/// Slots to replay. A backfill runs a range, so the replay must go on past the first block.
/// Each channel holds one update per slot, because nothing reads them until the replay ends.
const REPLAY_SLOTS: u64 = 10;

/// `VATDebit` rewards in the first block, as mainnet `getBlock` returns them.
const VAT_DEBIT_REWARDS: usize = 657;

/// A firehose that cannot decode a reward type retries the slot with no limit
/// and reports no error, so this limit is the failure signal.
const REPLAY_BUDGET: Duration = Duration::from_secs(120);

#[test]
#[ignore = "network: replays epoch 1064 from files.old-faithful.net; run with --ignored"]
fn replay_from_epoch_1064_goes_past_the_vat_debit_block() {
    let (skipped_tx, mut skipped_rx) = mpsc::channel(REPLAY_SLOTS as usize);

    let config = JetstreamSourceConfig {
        archive_url: "https://files.old-faithful.net".to_string(),
        range: SlotRangeConfig {
            slot_start: Some(EPOCH_1064_FIRST_SLOT),
            slot_end: Some(EPOCH_1064_FIRST_SLOT + REPLAY_SLOTS),
            epoch: None,
        },
        threads: 4,
        network: "mainnet".to_string(),
        compact_index_base_url: "https://files.old-faithful.net".to_string(),
        network_capacity_mb: 1000,
        sequential: false,
        reverse: false,
        buffer_window_bytes: None,
        stats_interval_slots: 0,
        possible_leader_skipped_tx: Some(skipped_tx),
        shutdown_signal_tx: None,
    };

    // SAFETY: the only other thread is the libtest main thread, and it does not read
    // or write the environment. The Tokio runtime starts below.
    unsafe { init_process_env(&config) };

    let filters = Filters::new(HashMap::from([("block-meta".to_string(), Prefilter {
        block_meta: Some(BlockMetaPrefilter {}),
        ..Prefilter::default()
    })]));

    let runtime = tokio::runtime::Runtime::new().expect("build the Tokio runtime");

    let replay = runtime.block_on(async {
        let (tx, mut rx) = mpsc::channel(REPLAY_SLOTS as usize);
        let source = JetstreamSource::from_config(config);
        let connect = source.connect(SourceContext::new(filters, tx));
        let exit = tokio::time::timeout(REPLAY_BUDGET, connect).await?;

        let mut updates = Vec::new();
        while let Ok(update) = rx.try_recv() {
            updates.push(update);
        }

        let mut skipped = BTreeSet::new();
        while let Ok(event) = skipped_rx.try_recv() {
            skipped.insert(event.slot);
        }

        Ok::<_, tokio::time::error::Elapsed>((exit, updates, skipped))
    });

    // A firehose that retries forever keeps its tasks alive, so do not wait for them.
    runtime.shutdown_background();

    let Ok((exit, updates, skipped)) = replay else {
        panic!("the replay did not end in {REPLAY_BUDGET:?}");
    };
    exit.expect("the replay ends with no error");

    let block_metas: BTreeMap<u64, SubscribeUpdateBlockMeta> = updates
        .into_iter()
        .map(
            |update| match update.expect("the update is not an error").update_oneof {
                Some(UpdateOneof::BlockMeta(meta)) => (meta.slot, meta),
                other => panic!("expected a BlockMeta update, got {other:?}"),
            },
        )
        .collect();

    let replayed: BTreeSet<u64> = block_metas.keys().copied().chain(skipped).collect();
    let range: BTreeSet<u64> =
        (EPOCH_1064_FIRST_SLOT..EPOCH_1064_FIRST_SLOT + REPLAY_SLOTS).collect();
    assert_eq!(replayed, range, "each slot is a block or a skipped slot");
    assert!(
        block_metas.keys().any(|slot| *slot > EPOCH_1064_FIRST_SLOT),
        "no block after slot {EPOCH_1064_FIRST_SLOT}: {:?}",
        block_metas.keys()
    );

    let first_block = block_metas
        .get(&EPOCH_1064_FIRST_SLOT)
        .expect("a block meta update for the first slot");
    let vat_debits: Vec<i64> = first_block
        .rewards
        .iter()
        .flat_map(|rewards| &rewards.rewards)
        .filter(|reward| reward.reward_type == RewardType::VatDebit as i32)
        .map(|reward| reward.lamports)
        .collect();

    assert_eq!(vat_debits.len(), VAT_DEBIT_REWARDS);
    assert!(
        vat_debits.iter().all(|lamports| *lamports < 0),
        "a VATDebit reward debits lamports: {vat_debits:?}"
    );
}
