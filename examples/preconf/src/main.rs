use std::path::PathBuf;

use clap::Parser as _;
use shipstern::{Handler, HandlerResult, Pipeline, Runtime};
use shipstern_core::instruction::InstructionUpdate;
use shipstern_preconf_source::PreconfSource;
use shipstern_spl_token_parser::{InstructionParser, TokenProgram};
use tracing::info;

#[derive(clap::Parser)]
#[command(version, author, about)]
struct Opts {
    #[arg(long, short)]
    config: PathBuf,
}

/// Logs the fields a preconf carries: the slot, the arrival index and the instruction.
#[derive(Debug)]
struct PreconfLogger;

impl Handler<TokenProgram, InstructionUpdate> for PreconfLogger {
    async fn handle(&self, value: &TokenProgram, raw: &InstructionUpdate) -> HandlerResult<()> {
        let shared = &raw.shared;
        info!(
            slot = shared.slot,
            index = shared.txn_index,
            signature = %bs58::encode(&shared.signature).into_string(),
            instruction = ?value,
            "preconfirmed token instruction"
        );
        Ok(())
    }
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .expect("Failed to install rustls crypto provider");

    let Opts { config } = Opts::parse();
    let config = std::fs::read_to_string(config).expect("Error reading config file");
    let config = toml::from_str(&config).expect("Error parsing config");

    Runtime::builder()
        .instruction(Pipeline::new(InstructionParser, [PreconfLogger]))
        .build::<PreconfSource>(config)
        .run_async()
        .await;
}
