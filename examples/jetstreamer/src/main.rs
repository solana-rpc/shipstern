use std::time::Instant;

use anyhow::Result;
use clap::Parser as _;
use shipstern::{
    config::{BufferConfig, ShipsternConfig},
    Handler, HandlerResult, Pipeline, Runtime,
};
use shipstern_core::{instruction::InstructionUpdate, ParserId};
use shipstern_jetstream_source::{JetstreamSource, JetstreamSourceConfig, SlotRangeConfig};
use shipstern_spl_token_parser::{instruction::Instruction, InstructionParser, TokenProgram};
use tracing::info;

fn pk(pubkey: &shipstern_spl_token_parser::Pubkey) -> String { pubkey.to_string() }

fn pk_opt(pubkey: &Option<shipstern_spl_token_parser::Pubkey>) -> String {
    match pubkey {
        Some(p) => p.to_string(),
        None => "None".to_string(),
    }
}

/// Handler for SPL Token program instructions
#[derive(Debug)]
struct TokenInstructionLogger;

impl Handler<TokenProgram, InstructionUpdate> for TokenInstructionLogger {
    async fn handle(&self, value: &TokenProgram, _raw: &InstructionUpdate) -> HandlerResult<()> {
        let Some(ix) = value.instruction.as_ref() else {
            return Ok(());
        };

        match ix {
            Instruction::Transfer(t) => {
                info!(
                    instruction = "Transfer",
                    source = %pk(&t.accounts.source),
                    destination = %pk(&t.accounts.destination),
                    owner = %pk(&t.accounts.owner),
                    amount = t.args.amount,
                    "Token transfer instruction"
                );
            },

            Instruction::TransferChecked(t) => {
                info!(
                    instruction = "TransferChecked",
                    source = %pk(&t.accounts.source),
                    destination = %pk(&t.accounts.destination),
                    mint = %pk(&t.accounts.mint),
                    owner = %pk(&t.accounts.owner),
                    amount = t.args.amount,
                    decimals = t.args.decimals,
                    "Token transfer checked instruction"
                );
            },

            Instruction::MintTo(t) => {
                info!(
                    instruction = "MintTo",
                    mint = %pk(&t.accounts.mint),
                    account = %pk(&t.accounts.account),
                    mint_authority = %pk(&t.accounts.mint_authority),
                    amount = t.args.amount,
                    "Token mint instruction"
                );
            },

            Instruction::Burn(t) => {
                info!(
                    instruction = "Burn",
                    account = %pk(&t.accounts.account),
                    mint = %pk(&t.accounts.mint),
                    owner = %pk(&t.accounts.owner),
                    amount = t.args.amount,
                    "Token burn instruction"
                );
            },

            Instruction::InitializeMint(t) => {
                info!(
                    instruction = "InitializeMint",
                    mint = %pk(&t.accounts.mint),
                    decimals = t.args.decimals,
                    mint_authority = %pk(&t.args.mint_authority),
                    freeze_authority = %pk_opt(&t.args.freeze_authority),
                    "Token mint initialization"
                );
            },

            Instruction::InitializeAccount(t) => {
                info!(
                    instruction = "InitializeAccount",
                    account = %pk(&t.accounts.account),
                    mint = %pk(&t.accounts.mint),
                    owner = %pk(&t.accounts.owner),
                    "Token account initialization"
                );
            },

            Instruction::Approve(t) => {
                info!(
                    instruction = "Approve",
                    source = %pk(&t.accounts.source),
                    delegate = %pk(&t.accounts.delegate),
                    owner = %pk(&t.accounts.owner),
                    amount = t.args.amount,
                    "Token approval instruction"
                );
            },

            Instruction::CloseAccount(t) => {
                info!(
                    instruction = "CloseAccount",
                    account = %pk(&t.accounts.account),
                    destination = %pk(&t.accounts.destination),
                    owner = %pk(&t.accounts.owner),
                    "Token account close instruction"
                );
            },

            other => {
                info!(instruction = ?other, "Other token program instruction");
            },
        }

        Ok(())
    }
}

#[derive(clap::Parser)]
#[command(version, about = "Jetstream historical replay with SPL Token parsing")]
struct Opts {
    /// Epoch to replay (e.g., 885)
    #[arg(long)]
    epoch: Option<u64>,

    /// Start slot (alternative to epoch)
    #[arg(long, conflicts_with = "epoch")]
    slot_start: Option<u64>,

    /// End slot (requires slot_start)
    #[arg(long, requires = "slot_start")]
    slot_end: Option<u64>,

    /// Number of parallel threads
    #[arg(long, default_value = "3")]
    threads: usize,

    /// Archive URL
    #[arg(long, default_value = "https://api.old-faithful.net")]
    archive_url: String,

    /// Emit a firehose progress stats line every N slots (0 disables)
    #[arg(long, default_value = "10000")]
    stats_interval_slots: u64,

    /// Single firehose worker thread with parallel ripget downloads.
    /// `--threads` then configures ripget range concurrency.
    #[arg(long)]
    sequential: bool,

    /// Replay epochs from newest to oldest. Slots within an epoch still
    /// arrive in ascending order. Implies sequential mode upstream.
    #[arg(long)]
    reverse: bool,

    /// Ripget hot/cold window in bytes for sequential mode. Defaults to a
    /// bounded value when unset; see JetstreamSourceConfig::buffer_window_bytes.
    #[arg(long)]
    buffer_window_bytes: Option<u64>,
}

/// Entry point: set env vars while the process is still single-threaded,
/// then hand off to the async runtime.
fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let opts = Opts::parse();

    let range = SlotRangeConfig {
        slot_start: opts.slot_start,
        slot_end: opts.slot_end,
        epoch: if opts.slot_start.is_some() {
            None
        } else {
            opts.epoch.or(Some(885))
        },
    };

    let config = JetstreamSourceConfig {
        archive_url: opts.archive_url,
        range,
        threads: opts.threads,
        network: "mainnet".to_string(),
        compact_index_base_url: "https://files.old-faithful.net".to_string(),
        network_capacity_mb: 100000,
        sequential: opts.sequential,
        reverse: opts.reverse,
        buffer_window_bytes: opts.buffer_window_bytes,
        stats_interval_slots: opts.stats_interval_slots,
        possible_leader_skipped_tx: None,
        shutdown_signal_tx: None,
    };

    // SAFETY: Called from main() before the Tokio runtime is created.
    // This binary must not spawn any other threads before this point.
    unsafe { shipstern_jetstream_source::init_process_env(&config) };

    tokio::runtime::Runtime::new()?.block_on(run(config))
}

async fn run(config: JetstreamSourceConfig) -> Result<()> {
    info!("Starting Jetstream replay with SPL Token parsing");
    info!(archive_url = %config.archive_url, "Configuration");
    info!(
        slot_start = ?config.range.slot_start,
        slot_end = ?config.range.slot_end,
        epoch = ?config.range.epoch,
        threads = config.threads,
        "Source configuration"
    );

    let (start_slot, end_slot) = config
        .range
        .to_slot_range()
        .map_err(|e| anyhow::anyhow!("Failed to resolve slot range: {}", e))?;
    info!(start_slot, end_slot, "Resolved slot range");

    let shipstern_config = ShipsternConfig {
        source: config,
        buffer: BufferConfig::default(),
    };

    info!("Building Shipstern runtime with SPL Token instruction parser");
    let pipeline = Pipeline::new(InstructionParser, [TokenInstructionLogger]);
    info!("Created pipeline with ID: {}", pipeline.id());

    let runtime = Runtime::builder()
        .instruction(pipeline)
        .build::<JetstreamSource>(shipstern_config);

    let start_time = Instant::now();

    info!("Starting Shipstern runtime...");
    runtime.try_run_async().await?;

    let processing_time = start_time.elapsed().as_secs_f64();
    info!(
        processing_time_secs = processing_time,
        "Jetstream replay completed"
    );

    Ok(())
}
