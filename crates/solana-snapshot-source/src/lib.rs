use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{atomic::AtomicBool, Arc},
};

use agave_snapshots::{
    paths::{
        get_highest_full_snapshot_archive_info, get_highest_incremental_snapshot_archive_info,
    },
    snapshot_archive_info::SnapshotArchiveInfoGetter,
    snapshot_config::{SnapshotConfig, SnapshotUsage},
};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use shipstern::{
    sources::{FromConfig, SourceContext, SourceExitStatus, SourceTrait},
    Error as ShipsternError,
};
use shipstern_core::{Filters, Pubkey};
use solana_account::ReadableAccount;
use solana_accounts_db::{
    accounts_db::AccountsDbConfig,
    is_loadable::IsLoadable,
    utils::{create_all_accounts_run_and_snapshot_dirs, move_and_async_delete_path_contents},
};
use solana_genesis_utils::open_genesis_config;
use solana_pubkey::Pubkey as SolanaPubkey;
use solana_runtime::{
    bank::Bank, runtime_config::RuntimeConfig, snapshot_bank_utils, snapshot_utils,
};
use tokio::sync::mpsc;
use yellowstone_grpc_proto::geyser::{
    subscribe_update::UpdateOneof, SlotStatus, SubscribeUpdate, SubscribeUpdateAccount,
    SubscribeUpdateAccountInfo, SubscribeUpdateSlot,
};

const MAX_GENESIS_ARCHIVE_UNPACKED_SIZE: u64 = 10485760;

pub struct SolanaSnapshot {
    pub bank: Arc<Bank>,
    pub slot: u64,
}

impl SolanaSnapshot {
    pub fn load_ledger<P: Into<PathBuf>>(ledger_path: P) -> Result<Self, ShipsternError> {
        let ledger_path = ledger_path.into();

        tracing::info!("Opening genesis config from {:?}", ledger_path);
        let genesis_config = open_genesis_config(&ledger_path, MAX_GENESIS_ARCHIVE_UNPACKED_SIZE)
            .map_err(|e| {
            ShipsternError::Io(std::io::Error::other(format!(
                "Failed to open genesis config: {e}"
            )))
        })?;

        // `base_working_path` and `memlock_budget_size` were dropped from
        // `AccountsDbConfig` in accounts-db 4.x.
        let accounts_db_config = AccountsDbConfig {
            skip_initial_hash_calc: true,
            ..AccountsDbConfig::default()
        };

        let snapshot_path = ledger_path.join("snapshots");
        let account_path = ledger_path.join("accounts");
        let snapshot_config = SnapshotConfig {
            usage: SnapshotUsage::LoadOnly,
            full_snapshot_archives_dir: snapshot_path.clone(),
            incremental_snapshot_archives_dir: snapshot_path.clone(),
            bank_snapshots_dir: snapshot_path,
            ..SnapshotConfig::default()
        };

        let (account_run_paths, _account_snapshot_paths) =
            create_all_accounts_run_and_snapshot_dirs(&[account_path]).map_err(|e| {
                ShipsternError::Io(std::io::Error::other(format!(
                    "Failed to create account paths: {e}"
                )))
            })?;
        let account_paths = account_run_paths;

        let Some(full_snapshot_archive_info) =
            get_highest_full_snapshot_archive_info(&snapshot_config.full_snapshot_archives_dir)
        else {
            return Err(ShipsternError::Io(std::io::Error::other(format!(
                "No loadable snapshot found under {:?}",
                snapshot_config.full_snapshot_archives_dir
            ))));
        };

        let incremental_snapshot_archive_info = get_highest_incremental_snapshot_archive_info(
            &snapshot_config.incremental_snapshot_archives_dir,
            full_snapshot_archive_info.slot(),
        );

        let latest_snapshot_archive_slot = std::cmp::max(
            full_snapshot_archive_info.slot(),
            incremental_snapshot_archive_info
                .as_ref()
                .map(SnapshotArchiveInfoGetter::slot)
                .unwrap_or(0),
        );

        // The same branch as solana-ledger `try_load_bank_forks_from_snapshot` with `WhenNewest`.
        let fastboot_snapshot =
            snapshot_utils::get_highest_loadable_bank_snapshot(&snapshot_config)
                .filter(|bank_snapshot| bank_snapshot.slot >= latest_snapshot_archive_slot);

        let bank = if let Some(fastboot_snapshot) = fastboot_snapshot {
            tracing::info!(
                "Loading bank from snapshot dir {:?}",
                fastboot_snapshot.snapshot_dir
            );
            snapshot_bank_utils::bank_from_snapshot_dir(
                &account_paths,
                &fastboot_snapshot,
                &genesis_config,
                &RuntimeConfig::default(),
                None,  // debug_keys
                None,  // leader_for_tests
                None,  // limit_load_slot_count_from_snapshot
                false, // verify_index
                accounts_db_config,
                None, // accounts_update_notifier
                Arc::new(AtomicBool::new(false)),
            )
            .map_err(|e| {
                ShipsternError::Io(std::io::Error::other(format!(
                    "Failed to load bank from snapshot dir {:?}: {e}",
                    fastboot_snapshot.snapshot_path()
                )))
            })?
        } else {
            // The archive unpacks into the run dirs, so the state of an earlier run goes first.
            {
                snapshot_utils::purge_all_bank_snapshots(&snapshot_config.bank_snapshots_dir);

                for account_path in &account_paths {
                    move_and_async_delete_path_contents(account_path);
                }

                snapshot_utils::wipe_account_snapshot_dirs(&account_paths);
            }

            tracing::info!("Loading bank from snapshot archives");
            snapshot_bank_utils::bank_from_snapshot_archives(
                &account_paths,
                &full_snapshot_archive_info,
                incremental_snapshot_archive_info.as_ref(),
                &snapshot_config,
                &genesis_config,
                &RuntimeConfig::default(),
                None,  // debug_keys
                None,  // leader_for_tests
                None,  // limit_load_slot_count_from_snapshot
                true,  // accounts_db_skip_shrink
                false, // accounts_db_force_initial_clean
                false, // verify_index
                accounts_db_config,
                None, // accounts_update_notifier
                Arc::new(AtomicBool::new(false)),
            )
            .map_err(|e| {
                let incremental_snapshot_archive =
                    incremental_snapshot_archive_info.as_ref().map_or_else(
                        || "none".to_string(),
                        |info| info.path().display().to_string(),
                    );

                ShipsternError::Io(std::io::Error::other(format!(
                    "Failed to load bank from snapshot archives: {e}, full snapshot archive: {}, \
                     incremental snapshot archive: {incremental_snapshot_archive}",
                    full_snapshot_archive_info.path().display(),
                )))
            })?
        };

        let slot = bank.slot();

        Ok(Self {
            bank: Arc::new(bank),
            slot,
        })
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, clap::Args)]
pub struct SolanaSnapshotConfig {
    /// The path to the ledger directory.
    ///
    /// The directory should have the following structure:
    ///
    /// ```text
    /// ~/ledger$ tree
    /// .
    /// ├── genesis.bin
    /// ├── genesis.tar.bz2
    /// └── snapshots
    ///     ├── snapshot-[...].tar.zst
    ///     └── incremental-snapshot-[...].tar.zst
    /// ```
    ///
    /// If the directory also holds a newer bank snapshot, as a validator's ledger directory
    /// can, the source loads that bank snapshot, as solana-ledger does.
    ///
    /// Note: Require a manual clean after with `rm -rf accounts accounts_hash_cache/`
    pub ledger_path: PathBuf,
    pub channel_size: usize,
}

/// A `Source` implementation for the Solana Snapshot API.
#[derive(Debug)]
pub struct SolanaSnapshotSource {
    config: SolanaSnapshotConfig,
}

#[derive(Clone)]
struct FilterOwnerKeyLookup(Arc<HashMap<Pubkey, Vec<String>>>);

impl FilterOwnerKeyLookup {
    fn new(filters: &Filters) -> Self {
        let mut lookup: HashMap<Pubkey, Vec<String>> = HashMap::new();

        for (key, parser_filter) in filters.parsers_filters.iter() {
            if let Some(account_filter) = parser_filter.account.as_ref() {
                for owner in &account_filter.owners {
                    lookup.entry(*owner).or_default().push(key.clone());
                }
            }
        }

        Self(Arc::new(lookup))
    }

    fn lookup_by_owner(&self, owner: &Pubkey) -> Option<Vec<String>> { self.0.get(owner).cloned() }

    fn owners(&self) -> Vec<Pubkey> { self.0.keys().copied().collect() }
}

impl FromConfig for SolanaSnapshotSource {
    type Config = SolanaSnapshotConfig;

    fn from_config(config: Self::Config) -> Self { Self { config } }
}

#[async_trait]
impl SourceTrait for SolanaSnapshotSource {
    async fn connect(&self, ctx: SourceContext) -> Result<SourceExitStatus, ShipsternError> {
        let SourceContext { filters, tx, .. } = ctx;

        let filter_owner_key_lookup = FilterOwnerKeyLookup::new(&filters);
        let solana_snapshot = SolanaSnapshot::load_ledger(self.config.ledger_path.clone())?;
        let snapshot_slot = solana_snapshot.slot;

        let slot_filters = filters
            .parsers_filters
            .iter()
            .filter_map(|(key, parser_filter)| parser_filter.slot.map(|_| key.to_string()))
            .collect::<Vec<String>>();

        tx.send(Ok(SubscribeUpdate {
            filters: slot_filters,
            created_at: None,
            update_oneof: Some(UpdateOneof::Slot(SubscribeUpdateSlot {
                slot: snapshot_slot,
                parent: None,
                status: SlotStatus::SlotFinalized.into(),
                dead_error: None,
                bank_id: Some(snapshot_slot),
            })),
        }))
        .await
        .map_err(|_| ShipsternError::ServerHangup)?;

        let channel_size = self.config.channel_size;
        let (sync_tx, mut sync_rx) = mpsc::channel::<Event>(channel_size);

        let tx_sender = tx.clone();
        let sender_handle = tokio::spawn(async move {
            while let Some(event) = sync_rx.recv().await {
                match event {
                    Event::AccountUpdate {
                        account_update,
                        filters,
                    } => {
                        if let Err(err) = tx_sender
                            .send(Ok(SubscribeUpdate {
                                filters,
                                created_at: None,
                                update_oneof: Some(UpdateOneof::Account(account_update)),
                            }))
                            .await
                        {
                            tracing::error!("Error snapshot sending account update: {:?}", err);
                        }
                    },
                    Event::SnapshotFinished => break,
                }
            }
        });

        tracing::info!("Scanning accounts for accounts owned by program ids");
        let scan_sync_tx = sync_tx.clone();
        let filter_owner_key_lookup = filter_owner_key_lookup.clone();
        let bank = solana_snapshot.bank.clone();

        let scan_handle = tokio::spawn(async move {
            tokio::task::spawn_blocking(move || {
                let owners = filter_owner_key_lookup.owners();
                let program_ids = owners
                    .iter()
                    .map(|owner| SolanaPubkey::new_from_array(**owner))
                    .collect::<Vec<_>>();
                let mut is_closed = false;
                let mut error: Option<ShipsternError> = None;

                // runtime 4.x made `AccountsDb::scan_accounts` private; `Bank`
                // exposes the same traversal through `scan_all_accounts`.
                bank.scan_all_accounts(|option| {
                    if is_closed {
                        return;
                    }
                    if let Some((pubkey, account, _slot)) = option {
                        if !account.is_loadable() {
                            return;
                        }

                        let owner = account.owner();
                        if !program_ids.contains(owner) {
                            return;
                        }

                        let event = Event::AccountUpdate {
                            account_update: SubscribeUpdateAccount {
                                account: Some(SubscribeUpdateAccountInfo {
                                    pubkey: pubkey.to_bytes().to_vec(),
                                    lamports: account.lamports(),
                                    owner: account.owner().to_bytes().to_vec(),
                                    executable: account.executable(),
                                    rent_epoch: account.rent_epoch(),
                                    data: account.data().to_vec(),
                                    write_version: 0,
                                    txn_signature: None,
                                }),
                                slot: snapshot_slot,
                                is_startup: true,
                                bank_id: None,
                            },
                            filters: filter_owner_key_lookup
                                .lookup_by_owner(&Pubkey::from(owner.to_bytes()))
                                .unwrap_or_default(),
                        };

                        if let Err(err) = scan_sync_tx.blocking_send(event) {
                            is_closed = true;
                            error = Some(ShipsternError::Io(std::io::Error::other(format!(
                                "Error sending account update: {err:?}"
                            ))));
                        }
                    }
                })
                .map_err(|e| {
                    ShipsternError::Io(std::io::Error::other(format!("Scan failed: {e}")))
                })?;

                if let Some(error) = error {
                    Err(error)
                } else {
                    Ok(())
                }
            })
            .await
            .map_err(|_| ShipsternError::Io(std::io::Error::other("Scan thread panicked")))
        });

        let scan_result = scan_handle
            .await
            .map_err(|_| ShipsternError::Io(std::io::Error::other("Scan task panicked")))?
            .and_then(std::convert::identity);

        if let Err(error) = scan_result {
            drop(sync_tx);
            let _ = sender_handle.await;

            return Err(error);
        }

        tracing::info!("Snapshot source finished");

        sync_tx
            .send(Event::SnapshotFinished)
            .await
            .map_err(|_| ShipsternError::ServerHangup)?;
        drop(sync_tx);
        let _ = sender_handle.await;

        Ok(SourceExitStatus::Completed)
    }
}
enum Event {
    AccountUpdate {
        account_update: SubscribeUpdateAccount,
        filters: Vec<String>,
    },
    SnapshotFinished,
}
