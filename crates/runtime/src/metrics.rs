//! Metrics interface and backend support for the Shipstern runtime.

use std::sync::LazyLock;

use prometheus::{IntCounter, Opts, Registry};
use yellowstone_grpc_proto::geyser::subscribe_update::UpdateOneof;

use crate::handler::PipelineErrors;

// TRANSACTIONS COUNTERS
pub(crate) static SHIPSTERN_TRANSACTIONS_RECEIVED: LazyLock<IntCounter> = LazyLock::new(|| {
    IntCounter::with_opts(Opts::new(
        "shipstern_transactions_received",
        "Total transactions received",
    ))
    .unwrap()
});
pub(crate) static SHIPSTERN_TRANSACTIONS_SUCCESSFUL: LazyLock<IntCounter> = LazyLock::new(|| {
    IntCounter::with_opts(Opts::new(
        "shipstern_transactions_successful",
        "Total successfully processed transactions",
    ))
    .unwrap()
});
pub(crate) static SHIPSTERN_TRANSACTIONS_PARSING_ERRORS: LazyLock<IntCounter> =
    LazyLock::new(|| {
        IntCounter::with_opts(Opts::new(
            "shipstern_transactions_parsing_errors",
            "Total parsing errors",
        ))
        .unwrap()
    });
pub(crate) static SHIPSTERN_TRANSACTIONS_HANDLER_ERRORS: LazyLock<IntCounter> =
    LazyLock::new(|| {
        IntCounter::with_opts(Opts::new(
            "shipstern_transactions_handler_errors",
            "Total handler errors",
        ))
        .unwrap()
    });

// ACCOUNTS COUNTERS
pub(crate) static SHIPSTERN_ACCOUNTS_RECEIVED: LazyLock<IntCounter> = LazyLock::new(|| {
    IntCounter::with_opts(Opts::new(
        "shipstern_accounts_received",
        "Total accounts received",
    ))
    .unwrap()
});
pub(crate) static SHIPSTERN_ACCOUNTS_SUCCESSFUL: LazyLock<IntCounter> = LazyLock::new(|| {
    IntCounter::with_opts(Opts::new(
        "shipstern_accounts_successful",
        "Total successfully processed accounts",
    ))
    .unwrap()
});
pub(crate) static SHIPSTERN_ACCOUNTS_PARSING_ERRORS: LazyLock<IntCounter> = LazyLock::new(|| {
    IntCounter::with_opts(Opts::new(
        "shipstern_accounts_parsing_errors",
        "Total parsing errors",
    ))
    .unwrap()
});
pub(crate) static SHIPSTERN_ACCOUNTS_HANDLER_ERRORS: LazyLock<IntCounter> = LazyLock::new(|| {
    IntCounter::with_opts(Opts::new(
        "shipstern_accounts_handler_errors",
        "Total handler errors",
    ))
    .unwrap()
});

// BLOCK METAS COUNTERS
pub(crate) static SHIPSTERN_BLOCK_METAS_RECEIVED: LazyLock<IntCounter> = LazyLock::new(|| {
    IntCounter::with_opts(Opts::new(
        "shipstern_block_metas_received",
        "Total block metas received",
    ))
    .unwrap()
});
pub(crate) static SHIPSTERN_BLOCK_METAS_SUCCESSFUL: LazyLock<IntCounter> = LazyLock::new(|| {
    IntCounter::with_opts(Opts::new(
        "shipstern_block_metas_successful",
        "Total successfully processed block metas",
    ))
    .unwrap()
});
pub(crate) static SHIPSTERN_BLOCK_METAS_PARSING_ERRORS: LazyLock<IntCounter> =
    LazyLock::new(|| {
        IntCounter::with_opts(Opts::new(
            "shipstern_block_metas_parsing_errors",
            "Total block metas parsing errors",
        ))
        .unwrap()
    });
pub(crate) static SHIPSTERN_BLOCK_METAS_HANDLER_ERRORS: LazyLock<IntCounter> =
    LazyLock::new(|| {
        IntCounter::with_opts(Opts::new(
            "shipstern_block_metas_handler_errors",
            "Total block metas handler errors",
        ))
        .unwrap()
    });

// BLOCK FOOTERS COUNTERS
pub(crate) static SHIPSTERN_BLOCK_FOOTERS_RECEIVED: LazyLock<IntCounter> = LazyLock::new(|| {
    IntCounter::with_opts(Opts::new(
        "shipstern_block_footers_received",
        "Total block footers received",
    ))
    .unwrap()
});
pub(crate) static SHIPSTERN_BLOCK_FOOTERS_SUCCESSFUL: LazyLock<IntCounter> = LazyLock::new(|| {
    IntCounter::with_opts(Opts::new(
        "shipstern_block_footers_successful",
        "Total successfully processed block footers",
    ))
    .unwrap()
});
pub(crate) static SHIPSTERN_BLOCK_FOOTERS_PARSING_ERRORS: LazyLock<IntCounter> =
    LazyLock::new(|| {
        IntCounter::with_opts(Opts::new(
            "shipstern_block_footers_parsing_errors",
            "Total block footers parsing errors",
        ))
        .unwrap()
    });
pub(crate) static SHIPSTERN_BLOCK_FOOTERS_HANDLER_ERRORS: LazyLock<IntCounter> =
    LazyLock::new(|| {
        IntCounter::with_opts(Opts::new(
            "shipstern_block_footers_handler_errors",
            "Total block footers handler errors",
        ))
        .unwrap()
    });

// BLOCK COUNTERS
pub(crate) static SHIPSTERN_BLOCK_RECEIVED: LazyLock<IntCounter> = LazyLock::new(|| {
    IntCounter::with_opts(Opts::new(
        "shipstern_block_received",
        "Total block received",
    ))
    .unwrap()
});
pub(crate) static SHIPSTERN_BLOCK_SUCCESSFUL: LazyLock<IntCounter> = LazyLock::new(|| {
    IntCounter::with_opts(Opts::new(
        "shipstern_block_successful",
        "Total successfully processed block",
    ))
    .unwrap()
});
pub(crate) static SHIPSTERN_BLOCK_PARSING_ERRORS: LazyLock<IntCounter> = LazyLock::new(|| {
    IntCounter::with_opts(Opts::new(
        "shipstern_block_parsing_errors",
        "Total block parsing errors",
    ))
    .unwrap()
});
pub(crate) static SHIPSTERN_BLOCK_HANDLER_ERRORS: LazyLock<IntCounter> = LazyLock::new(|| {
    IntCounter::with_opts(Opts::new(
        "shipstern_block_handler_errors",
        "Total block handler errors",
    ))
    .unwrap()
});

// INSTRUCTIONS COUNTERS
pub(crate) static SHIPSTERN_INSTRUCTIONS_SUCCESSFUL: LazyLock<IntCounter> = LazyLock::new(|| {
    IntCounter::with_opts(Opts::new(
        "shipstern_instructions_successful",
        "Total successfully processed instructions",
    ))
    .unwrap()
});
pub(crate) static SHIPSTERN_INSTRUCTIONS_PARSING_ERRORS: LazyLock<IntCounter> =
    LazyLock::new(|| {
        IntCounter::with_opts(Opts::new(
            "shipstern_instructions_parsing_errors",
            "Total parsing errors",
        ))
        .unwrap()
    });
pub(crate) static SHIPSTERN_INSTRUCTIONS_HANDLER_ERRORS: LazyLock<IntCounter> =
    LazyLock::new(|| {
        IntCounter::with_opts(Opts::new(
            "shipstern_instructions_handler_errors",
            "Total handler errors",
        ))
        .unwrap()
    });

// SLOTS COUNTERS
pub(crate) static SHIPSTERN_SLOTS_RECEIVED: LazyLock<IntCounter> = LazyLock::new(|| {
    IntCounter::with_opts(Opts::new(
        "shipstern_slots_received",
        "Total slots received",
    ))
    .unwrap()
});
pub(crate) static SHIPSTERN_SLOTS_SUCCESSFUL: LazyLock<IntCounter> = LazyLock::new(|| {
    IntCounter::with_opts(Opts::new(
        "shipstern_slots_successful",
        "Total successfully processed slots",
    ))
    .unwrap()
});
pub(crate) static SHIPSTERN_SLOTS_PARSING_ERRORS: LazyLock<IntCounter> = LazyLock::new(|| {
    IntCounter::with_opts(Opts::new(
        "shipstern_slots_parsing_errors",
        "Total slots parsing errors",
    ))
    .unwrap()
});
pub(crate) static SHIPSTERN_SLOTS_HANDLER_ERRORS: LazyLock<IntCounter> = LazyLock::new(|| {
    IntCounter::with_opts(Opts::new(
        "shipstern_slots_handler_errors",
        "Total slots handler errors",
    ))
    .unwrap()
});

#[derive(Clone, Copy, Debug)]
pub(crate) enum UpdateType {
    Account,
    Transaction,
    BlockMeta,
    BlockFooter,
    Block,
    Instruction,
    Unknown,
    Slot,
}

impl From<&UpdateOneof> for UpdateType {
    fn from(update_type: &UpdateOneof) -> Self {
        match update_type {
            UpdateOneof::Account(_) => UpdateType::Account,
            UpdateOneof::Transaction(_) => UpdateType::Transaction,
            UpdateOneof::BlockMeta(_) => UpdateType::BlockMeta,
            UpdateOneof::BlockFooter(_) => UpdateType::BlockFooter,
            UpdateOneof::Block(_) => UpdateType::Block,
            UpdateOneof::Slot(_) => UpdateType::Slot,
            _ => UpdateType::Unknown,
        }
    }
}

pub(crate) fn increment_processed_updates(
    handle_result: &Result<(), PipelineErrors>,
    update_type: UpdateType,
) {
    match handle_result {
        Ok(()) => match update_type {
            UpdateType::Account => SHIPSTERN_ACCOUNTS_SUCCESSFUL.inc(),
            UpdateType::Transaction => SHIPSTERN_TRANSACTIONS_SUCCESSFUL.inc(),
            UpdateType::Instruction => SHIPSTERN_INSTRUCTIONS_SUCCESSFUL.inc(),
            UpdateType::BlockMeta => SHIPSTERN_BLOCK_METAS_SUCCESSFUL.inc(),
            UpdateType::BlockFooter => SHIPSTERN_BLOCK_FOOTERS_SUCCESSFUL.inc(),
            UpdateType::Block => SHIPSTERN_BLOCK_SUCCESSFUL.inc(),
            UpdateType::Slot => SHIPSTERN_SLOTS_SUCCESSFUL.inc(),
            UpdateType::Unknown => (),
        },
        Err(PipelineErrors::Parse(_)) => match update_type {
            UpdateType::Account => SHIPSTERN_ACCOUNTS_PARSING_ERRORS.inc(),
            UpdateType::Transaction => SHIPSTERN_TRANSACTIONS_PARSING_ERRORS.inc(),
            UpdateType::Instruction => SHIPSTERN_INSTRUCTIONS_PARSING_ERRORS.inc(),
            UpdateType::BlockMeta => SHIPSTERN_BLOCK_METAS_PARSING_ERRORS.inc(),
            UpdateType::BlockFooter => SHIPSTERN_BLOCK_FOOTERS_PARSING_ERRORS.inc(),
            UpdateType::Block => SHIPSTERN_BLOCK_PARSING_ERRORS.inc(),
            UpdateType::Slot => SHIPSTERN_SLOTS_PARSING_ERRORS.inc(),
            UpdateType::Unknown => (),
        },
        Err(PipelineErrors::Handlers(_)) => match update_type {
            UpdateType::Account => SHIPSTERN_ACCOUNTS_HANDLER_ERRORS.inc(),
            UpdateType::Transaction => SHIPSTERN_TRANSACTIONS_HANDLER_ERRORS.inc(),
            UpdateType::Instruction => SHIPSTERN_INSTRUCTIONS_HANDLER_ERRORS.inc(),
            UpdateType::BlockMeta => SHIPSTERN_BLOCK_METAS_HANDLER_ERRORS.inc(),
            UpdateType::BlockFooter => SHIPSTERN_BLOCK_FOOTERS_HANDLER_ERRORS.inc(),
            UpdateType::Block => SHIPSTERN_BLOCK_HANDLER_ERRORS.inc(),
            UpdateType::Slot => SHIPSTERN_SLOTS_HANDLER_ERRORS.inc(),
            UpdateType::Unknown => (),
        },
        _ => (),
    }
}

/// Increment accounts, transactions or block total updates received
///  based on the update type.
pub(crate) fn increment_received_updates(update_type: UpdateType) {
    match update_type {
        UpdateType::Account => SHIPSTERN_ACCOUNTS_RECEIVED.inc(),
        UpdateType::Transaction => SHIPSTERN_TRANSACTIONS_RECEIVED.inc(),
        UpdateType::BlockMeta => SHIPSTERN_BLOCK_METAS_RECEIVED.inc(),
        UpdateType::BlockFooter => SHIPSTERN_BLOCK_FOOTERS_RECEIVED.inc(),
        UpdateType::Block => SHIPSTERN_BLOCK_RECEIVED.inc(),
        UpdateType::Slot => SHIPSTERN_SLOTS_RECEIVED.inc(),
        _ => (),
    }
}

/// Register the metrics with the provided registry.
/// This function is idempotent - if metrics are already registered, it will not panic.
pub fn register_metrics(registry: &Registry) {
    // Try to register each metric, but ignore "AlreadyReg" errors
    let _ = registry.register(Box::new(SHIPSTERN_TRANSACTIONS_RECEIVED.clone()));
    let _ = registry.register(Box::new(SHIPSTERN_TRANSACTIONS_SUCCESSFUL.clone()));
    let _ = registry.register(Box::new(SHIPSTERN_TRANSACTIONS_PARSING_ERRORS.clone()));
    let _ = registry.register(Box::new(SHIPSTERN_TRANSACTIONS_HANDLER_ERRORS.clone()));

    let _ = registry.register(Box::new(SHIPSTERN_ACCOUNTS_RECEIVED.clone()));
    let _ = registry.register(Box::new(SHIPSTERN_ACCOUNTS_SUCCESSFUL.clone()));
    let _ = registry.register(Box::new(SHIPSTERN_ACCOUNTS_PARSING_ERRORS.clone()));
    let _ = registry.register(Box::new(SHIPSTERN_ACCOUNTS_HANDLER_ERRORS.clone()));

    let _ = registry.register(Box::new(SHIPSTERN_BLOCK_METAS_RECEIVED.clone()));
    let _ = registry.register(Box::new(SHIPSTERN_BLOCK_METAS_SUCCESSFUL.clone()));
    let _ = registry.register(Box::new(SHIPSTERN_BLOCK_METAS_PARSING_ERRORS.clone()));
    let _ = registry.register(Box::new(SHIPSTERN_BLOCK_METAS_HANDLER_ERRORS.clone()));
    let _ = registry.register(Box::new(SHIPSTERN_BLOCK_FOOTERS_RECEIVED.clone()));
    let _ = registry.register(Box::new(SHIPSTERN_BLOCK_FOOTERS_SUCCESSFUL.clone()));
    let _ = registry.register(Box::new(SHIPSTERN_BLOCK_FOOTERS_PARSING_ERRORS.clone()));
    let _ = registry.register(Box::new(SHIPSTERN_BLOCK_FOOTERS_HANDLER_ERRORS.clone()));

    let _ = registry.register(Box::new(SHIPSTERN_BLOCK_RECEIVED.clone()));
    let _ = registry.register(Box::new(SHIPSTERN_BLOCK_SUCCESSFUL.clone()));
    let _ = registry.register(Box::new(SHIPSTERN_BLOCK_PARSING_ERRORS.clone()));
    let _ = registry.register(Box::new(SHIPSTERN_BLOCK_HANDLER_ERRORS.clone()));

    let _ = registry.register(Box::new(SHIPSTERN_INSTRUCTIONS_SUCCESSFUL.clone()));
    let _ = registry.register(Box::new(SHIPSTERN_INSTRUCTIONS_PARSING_ERRORS.clone()));
    let _ = registry.register(Box::new(SHIPSTERN_INSTRUCTIONS_HANDLER_ERRORS.clone()));

    let _ = registry.register(Box::new(SHIPSTERN_SLOTS_RECEIVED.clone()));
    let _ = registry.register(Box::new(SHIPSTERN_SLOTS_SUCCESSFUL.clone()));
    let _ = registry.register(Box::new(SHIPSTERN_SLOTS_PARSING_ERRORS.clone()));
    let _ = registry.register(Box::new(SHIPSTERN_SLOTS_HANDLER_ERRORS.clone()));
}
