# Shipstern Parser

This crate provides several account parsers, such as Token and TokenExtension. These parsers can be imported from this crate and used within shipstern.

## Installation

```bash

cargo add shipstern-parser

```

## Example

```rust

use std::path::PathBuf;

use clap::Parser as _;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
use shipstern::Pipeline;
use shipstern_yellowstone_grpc_source::YellowstoneGrpcSource;
use shipstern_parser::{
    token_extension_program::{
        AccountParser as TokenExtensionProgramAccParser,
        InstructionParser as TokenExtensionProgramIxParser,
    },
    token_program::{
        AccountParser as TokenProgramAccParser, InstructionParser as TokenProgramIxParser,
    },
};

fn main() {
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::from_default_env())
        .with(tracing_subscriber::fmt::layer())
        .init();

    let Opts { config } = Opts::parse();
    let config = std::fs::read_to_string(config).expect("Error reading config file");
    let config = toml::from_str(&config).expect("Error parsing config");

    shipstern::Runtime::builder()
        .account(Pipeline::new(TokenExtensionProgramAccParser, [Handler]))
        .account(Pipeline::new(TokenProgramAccParser, [Handler]))
        .instruction(Pipeline::new(TokenExtensionProgramIxParser, [Handler]))
        .instruction(Pipeline::new(TokenProgramIxParser, [Handler]))
        .metrics(prometheus::Registry::new())
        .build::<YellowstoneGrpcSource>(config)
        .run();
}
```
