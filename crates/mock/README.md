# Shipstern Mock

Shipstern Mock provides tools for testing Shipstern parsers without needing a live Solana node.
It supports offline fixtures, account replay, and instruction replay — helping you validate parsing logic quickly and reliably using devnet data.

## Features

-     🔌 Offline Testing
  Run parser unit tests without connecting to a live Solana node.
- 🗂 Fixture Management
  Load real Solana devnet accounts or transactions as JSON fixtures, reusable across tests.
- 🧪 Replay Support
  Replay devnet account updates and transaction instructions into your custom parsers.
- 🚀 Faster Development
  Build and debug parsing pipelines locally, with repeatable fixture-based tests.

Fixtures are read from `./fixtures` in the crate under test. A missing fixture is
fetched from Solana Devnet, or from the RPC endpoint in `RPC_ENDPOINT`, and
saved there.

## Installation

```
cargo add shipstern-mock
```

Example Usage

```rust
#[cfg(test)]
mod tests {
    use shipstern_mock::{account_fixture, tx_fixture};
    use shipstern_spl_token_extensions_parser::InstructionParser as TokenExtensionsIxParser;
    use shipstern_spl_token_parser::{account, AccountParser as TokenAccParser, TokenProgramState};

    #[tokio::test]
    async fn test_account_parsing() {
        let parser = TokenAccParser;
        let account = account_fixture!("3SmPYPvZfEmroktLiJsgaNENuPEud3Z52zSfLQ1zJdkK", &parser);

        let TokenProgramState {
            account: Some(account::Account::Mint(mint)),
        } = account
        else {
            panic!("Unexpected account state");
        };

        assert_eq!(mint.decimals, 10);
    }

    #[tokio::test]
    async fn test_instruction_parsing() {
        let parser = TokenExtensionsIxParser;
        let ixs = tx_fixture!(
            "44gWEyKUkeUabtJr4eT3CQEkFGrD4jMdwUV6Ew5MR5K3RGizs9iwbkb5Q4T3gnAaSgHxn3ERQ8g5YTXuLP1FrWnt",
            &parser
        );

        let Some(first_ix) = ixs.get(0) else {
            panic!("No instructions found");
        };

        tracing::info!("Parsed instruction: {:?}", first_ix);
    }
}
```
