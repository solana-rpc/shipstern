use std::{path::PathBuf, str::FromStr as _};

use clap::Parser as _;
use shipstern::{filter_pipeline::FilterPipeline, Handler, HandlerResult, Runtime};
use shipstern_core::{instruction::InstructionUpdate, KeyBytes, Prefilter};
use shipstern_preconf_source::PreconfSource;
use shipstern_spl_token_parser::{InstructionParser, TokenProgram};
use tracing::info;

/// The Orca Whirlpool program. A required key keeps the stream under the coverage share,
/// which the whole SPL Token program reaches within a minute.
const WHIRLPOOL: &str = "whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc";

#[derive(clap::Parser)]
#[command(version, author, about)]
struct Opts {
    #[arg(long, short)]
    config: PathBuf,
}

/// Logs each SPL Token instruction of a preconf with its slot, arrival index and signature.
#[derive(Debug)]
struct PreconfLogger;

impl Handler<TokenProgram, InstructionUpdate> for PreconfLogger {
    async fn handle(&self, value: &TokenProgram, raw: &InstructionUpdate) -> HandlerResult<()> {
        let shared = &raw.shared;
        info!(
            slot = shared.slot,
            index = shared.txn_index,
            signature = %shipstern_core::bs58::encode(&shared.signature).into_string(),
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
    // The error Debug holds the whole file, x-token included, so print only its message.
    let config =
        toml::from_str(&config).unwrap_or_else(|e| panic!("Error parsing config: {}", e.message()));

    let whirlpool = KeyBytes::<32>::from_str(WHIRLPOOL).expect("Error parsing the Whirlpool key");

    Runtime::builder()
        .instruction(FilterPipeline::new(
            InstructionParser,
            [PreconfLogger],
            Prefilter::builder().transaction_accounts([whirlpool]),
        ))
        .build::<PreconfSource>(config)
        .run_async()
        .await;
}
