#![deny(
    clippy::disallowed_methods,
    clippy::suspicious,
    clippy::style,
    clippy::clone_on_ref_ptr
)]
#![warn(clippy::pedantic)]
#![allow(clippy::module_name_repetitions)]

use std::path::PathBuf;

use clap::Parser as _;
use opentelemetry::trace::TracerProvider;
use shipstern::{Handler, HandlerResult, Pipeline, Runtime};
use shipstern_spl_token_parser::{AccountParser, InstructionParser};
use shipstern_yellowstone_grpc_source::YellowstoneGrpcSource;
use tracing_subscriber::{layer::SubscriberExt, EnvFilter, Layer as _};

#[derive(clap::Parser)]
#[command(version, author, about)]
pub struct Opts {
    #[arg(long, short)]
    config: PathBuf,
}

#[derive(Debug)]
pub struct Logger;

impl<V: std::fmt::Debug + Sync, R: Sync> Handler<V, R> for Logger {
    async fn handle(&self, value: &V, _raw: &R) -> HandlerResult<()> {
        println!("{value:?}");
        Ok(())
    }
}

#[rustfmt::skip]
#[allow(clippy::too_many_lines)]
fn main() {
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .expect("Failed to install rustls crypto provider");

    let span_exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_tonic()
        .build()
        .expect("Error building span exporter!");

    let tracer_provider = opentelemetry_sdk::trace::SdkTracerProvider::builder()
        .with_batch_exporter(span_exporter)
        .build();

    let log_exporter = opentelemetry_otlp::LogExporter::builder()
        .with_tonic()
        .build()
        .expect("Error building log exporter!");
    // let log_exporter = opentelemetry_stdout::LogExporter::default();

    let log_provider = opentelemetry_sdk::logs::SdkLoggerProvider::builder()
        .with_batch_exporter(log_exporter)
        .build();

    let tracer = tracer_provider.tracer("shipstern_tracer");
    let _ = opentelemetry::global::set_tracer_provider(tracer_provider.clone());

    let traces_layer = tracing_opentelemetry::layer().with_tracer(tracer);
    let log_layer = opentelemetry_appender_tracing::layer::OpenTelemetryTracingBridge::new(&log_provider);

    // Export only Shipstern's own spans and events, so the exporters' own gRPC traffic is not
    // traced back into them. `RUST_LOG` overrides the default.
    let filter = || {
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("shipstern=debug"))
    };

    let subscriber = tracing_subscriber::Registry::default()
        .with(traces_layer.with_filter(filter()))
        .with(log_layer.with_filter(filter()));
    tracing::subscriber::set_global_default(subscriber).unwrap();

    let Opts { config } = Opts::parse();
    let config = std::fs::read_to_string(config).expect("Error reading config file");
    let config = toml::from_str(&config).expect("Error parsing config");

    let result = Runtime::builder()
        .account(Pipeline::new(AccountParser, [Logger]))
        .instruction(Pipeline::new(InstructionParser, [Logger]))
        .build::<YellowstoneGrpcSource>(config)
        .try_run();

    // The batch exporters hold unsent data until they are shut down.
    if let Err(err) = tracer_provider.shutdown() {
        eprintln!("Error shutting down the tracer provider: {err}");
    }
    if let Err(err) = log_provider.shutdown() {
        eprintln!("Error shutting down the log provider: {err}");
    }

    if let Err(err) = result {
        eprintln!("Runtime stopped: {err}");
        std::process::exit(1);
    }
}
