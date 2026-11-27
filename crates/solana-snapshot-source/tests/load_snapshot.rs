use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{atomic::AtomicBool, Arc, RwLock},
};

use agave_fs::io_setup::IoSetupState;
use agave_snapshots::{snapshot_config::SnapshotConfig, SnapshotKind, SnapshotVersion};
use shipstern::sources::{FromConfig, SourceContext, SourceExitStatus, SourceTrait};
use shipstern_core::{Filters, Prefilter};
use shipstern_solana_snapshot_source::{
    SolanaSnapshot, SolanaSnapshotConfig, SolanaSnapshotSource,
};
use solana_account::ReadableAccount;
use solana_accounts_db::{
    accounts_db::AccountsDbConfig, utils::create_all_accounts_run_and_snapshot_dirs,
};
use solana_pubkey::Pubkey;
use solana_runtime::{
    bank::Bank,
    bank_forks::BankForks,
    genesis_utils::{create_genesis_config_with_leader, GenesisConfigInfo},
    runtime_config::RuntimeConfig,
    snapshot_bank_utils::bank_to_full_snapshot_archive,
    snapshot_package::SnapshotPackage,
    snapshot_utils::serialize_snapshot,
};
use tokio::sync::mpsc;
use yellowstone_grpc_proto::geyser::subscribe_update::UpdateOneof;

const LAMPORTS_PER_SOL: u64 = 1_000_000_000;

/// Slot of the bank in the archive. Not 0, so a load that falls back to genesis fails.
const ARCHIVE_SLOT: u64 = 3;

const RECIPIENT: Pubkey = Pubkey::new_from_array([7; 32]);

/// Writes genesis.bin to `ledger` and returns bank forks with a bank at slot 0 whose
/// storages go to `accounts_dir`.
fn genesis_bank(
    ledger: &Path,
    accounts_dir: PathBuf,
) -> (GenesisConfigInfo, Arc<RwLock<BankForks>>) {
    let genesis = create_genesis_config_with_leader(
        1_000 * LAMPORTS_PER_SOL,
        &Pubkey::new_from_array([1; 32]),
        1_000 * LAMPORTS_PER_SOL,
    );
    genesis
        .genesis_config
        .write(ledger)
        .expect("write genesis.bin");

    let bank0 = Bank::new_from_genesis(
        &genesis.genesis_config,
        Arc::new(RuntimeConfig::default()),
        vec![accounts_dir],
        None,
        AccountsDbConfig::default(),
        None,
        None,
        Arc::new(AtomicBool::new(false)),
        None,
        None,
    );
    bank0.fill_bank_with_ticks_for_tests();

    // A transfer needs the fork graph that BankForks gives the program cache.
    (genesis, BankForks::new_rw_arc(bank0))
}

/// Returns a complete child bank at `slot` that paid 1 SOL to `RECIPIENT`.
fn child_bank(
    bank_forks: &RwLock<BankForks>,
    parent: Arc<Bank>,
    slot: u64,
    genesis: &GenesisConfigInfo,
) -> Arc<Bank> {
    let leader = *parent.leader();
    let child = Bank::new_from_parent(parent, leader, slot);
    let bank = bank_forks
        .write()
        .expect("write the bank forks")
        .insert(child)
        .clone_without_scheduler();

    bank.transfer(LAMPORTS_PER_SOL, &genesis.mint_keypair, &RECIPIENT)
        .expect("transfer to the recipient");
    bank.fill_bank_with_ticks_for_tests();
    bank.set_block_id(Some(bank.last_blockhash()));

    bank
}

fn snapshot_config(snapshots_dir: &Path) -> SnapshotConfig {
    SnapshotConfig {
        full_snapshot_archives_dir: snapshots_dir.to_path_buf(),
        incremental_snapshot_archives_dir: snapshots_dir.to_path_buf(),
        bank_snapshots_dir: snapshots_dir.to_path_buf(),
        // Registered io_uring buffers count against RLIMIT_MEMLOCK; parallel tests exceed it (ENOMEM).
        use_registered_io_uring_buffers: false,
        // O_DIRECT fails on tmpfs temp dirs.
        use_direct_io: false,
        ..SnapshotConfig::default()
    }
}

fn recipient_lamports(snapshot: &SolanaSnapshot) -> u64 {
    snapshot
        .bank
        .get_account(&RECIPIENT)
        .expect("the loaded bank has the recipient account")
        .lamports()
}

/// A ledger dir whose only snapshot is a full archive at `ARCHIVE_SLOT`.
fn ledger_with_full_archive() -> tempfile::TempDir {
    let ledger = tempfile::tempdir().expect("create the ledger dir");
    let snapshots_dir = ledger.path().join("snapshots");
    std::fs::create_dir_all(&snapshots_dir).expect("create the snapshots dir");

    let bank_accounts = tempfile::tempdir().expect("create the bank accounts dir");
    let (genesis, bank_forks) = genesis_bank(ledger.path(), bank_accounts.path().to_path_buf());

    let bank0 = bank_forks.read().expect("read the bank forks").root_bank();
    let bank = child_bank(&bank_forks, bank0, ARCHIVE_SLOT, &genesis);
    bank_to_full_snapshot_archive(&snapshot_config(&snapshots_dir), &bank)
        .expect("write the full archive");

    ledger
}

#[test]
fn loads_the_bank_of_a_full_snapshot_archive() {
    let ledger = ledger_with_full_archive();

    // The second load starts on a ledger dir that the first load already used.
    for run in 1..=2 {
        let snapshot = SolanaSnapshot::load_ledger(ledger.path())
            .unwrap_or_else(|err| panic!("load {run} fails: {err:?}"));

        assert_eq!(snapshot.slot, ARCHIVE_SLOT, "load {run}");
        assert_eq!(
            recipient_lamports(&snapshot),
            LAMPORTS_PER_SOL,
            "load {run}"
        );
    }
}

/// solana-ledger loads a local bank snapshot dir at or above the newest archive slot and keeps it.
/// The equal slot matters too: an archive load would purge that bank snapshot dir.
#[test]
fn prefers_a_local_bank_snapshot_at_or_above_the_archive() {
    for (bank_snapshot_slot, recipient_sol) in [(ARCHIVE_SLOT, 1), (ARCHIVE_SLOT + 2, 2)] {
        let ledger = tempfile::tempdir().expect("create the ledger dir");
        let snapshots_dir = ledger.path().join("snapshots");
        std::fs::create_dir_all(&snapshots_dir).expect("create the snapshots dir");

        // A bank snapshot dir lists storages in the run dir, and the source reads that run dir.
        let (account_run_paths, _) =
            create_all_accounts_run_and_snapshot_dirs(&[ledger.path().join("accounts")])
                .expect("create the accounts dirs");
        let [account_run_path] = account_run_paths.as_slice() else {
            panic!("one run dir per accounts dir, got {account_run_paths:?}");
        };

        let (genesis, bank_forks) = genesis_bank(ledger.path(), account_run_path.clone());

        {
            let bank0 = bank_forks.read().expect("read the bank forks").root_bank();
            let bank3 = child_bank(&bank_forks, bank0, ARCHIVE_SLOT, &genesis);
            bank_to_full_snapshot_archive(&snapshot_config(&snapshots_dir), &bank3)
                .expect("write the full archive");

            let bank = if bank_snapshot_slot == ARCHIVE_SLOT {
                bank3
            } else {
                child_bank(&bank_forks, bank3, bank_snapshot_slot, &genesis)
            };
            bank.squash();
            bank.force_flush_accounts_cache();

            let status_cache_slot_deltas = bank
                .status_cache
                .read()
                .expect("read the status cache")
                .root_slot_deltas();
            let package = SnapshotPackage::new(
                SnapshotKind::Fastboot,
                &bank,
                bank.get_snapshot_storages(None),
                status_cache_slot_deltas,
            );
            serialize_snapshot(
                &snapshots_dir,
                SnapshotVersion::default(),
                package.bank_snapshot_package,
                &package.snapshot_storages,
                true, // should_finalize: mark the bank snapshot loadable
                &IoSetupState::default(),
            )
            .expect("write the bank snapshot dir");
        }
        drop(bank_forks);

        let snapshot = SolanaSnapshot::load_ledger(ledger.path())
            .unwrap_or_else(|err| panic!("load at {bank_snapshot_slot} fails: {err:?}"));

        assert_eq!(snapshot.slot, bank_snapshot_slot);
        assert_eq!(
            recipient_lamports(&snapshot),
            recipient_sol * LAMPORTS_PER_SOL,
            "bank snapshot at {bank_snapshot_slot}"
        );
        assert!(
            snapshots_dir.join(bank_snapshot_slot.to_string()).is_dir(),
            "the load keeps the bank snapshot dir at {bank_snapshot_slot}"
        );
    }
}

fn source_for(ledger: &Path, channel_size: usize) -> SolanaSnapshotSource {
    SolanaSnapshotSource::from_config(SolanaSnapshotConfig {
        ledger_path: ledger.to_path_buf(),
        channel_size,
    })
}

/// Accounts owned by the system program, which the genesis and the transfer create.
fn system_owned_accounts() -> Filters {
    let prefilter = Prefilter::builder()
        .account_owners([Pubkey::default()])
        .build()
        .expect("build the prefilter");

    Filters::new(HashMap::from([("system".to_string(), prefilter)]))
}

#[tokio::test(flavor = "multi_thread")]
async fn a_closed_receiver_before_the_slot_update_is_receiver_dropped() {
    let ledger = ledger_with_full_archive();
    let (tx, rx) = mpsc::channel(8);
    drop(rx);

    let result = source_for(ledger.path(), 8)
        .connect(SourceContext::new(system_owned_accounts(), tx))
        .await;

    assert!(
        matches!(result, Ok(SourceExitStatus::ReceiverDropped)),
        "got {result:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_receiver_dropped_mid_scan_is_receiver_dropped() {
    let ledger = ledger_with_full_archive();
    let (tx, mut rx) = mpsc::channel(1);
    let source = source_for(ledger.path(), 1);

    let connect = tokio::spawn(async move {
        source
            .connect(SourceContext::new(system_owned_accounts(), tx))
            .await
    });

    let first = rx.recv().await.expect("the slot update arrives");
    assert!(matches!(
        first.expect("no stream error").update_oneof,
        Some(UpdateOneof::Slot(_))
    ));
    drop(rx);

    let result = connect.await.expect("the source task does not panic");

    assert!(
        matches!(result, Ok(SourceExitStatus::ReceiverDropped)),
        "got {result:?}"
    );
}
