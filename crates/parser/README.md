# Shipstern Parser

Shared helpers for Shipstern parser crates and for the code that
`include_shipstern_parser!` generates. It holds the parser `Error` type,
`check_min_accounts_req`, the `Pubkey` conversion helpers and a `prelude` that
re-exports the `shipstern-core` parser types.

This crate has no parsers of its own. For ready-made parsers, use the program
crates:

- `shipstern-spl-token-parser` for the SPL Token program
- `shipstern-spl-token-extensions-parser` for Token-2022
- `shipstern-stake-pool-parser` for the SPL Stake Pool program
- `shipstern-bpf-loader-parser` for the upgradeable BPF loader

To build a parser from an Anchor or Codama IDL, see `shipstern-proc-macro`.

## Installation

```bash
cargo add shipstern-parser
```
