# shipstern-proc-macro

This crate provides a procedural macro for generating Shipstern parser modules from [Codama JSON IDL](https://github.com/codama-idl/codama).

## Usage

Add this crate to your `Cargo.toml` (usually as a `proc-macro` dependency):

```toml
[dependencies]
borsh = { version = "^1.0.0", features = ["derive"] }
shipstern-core = { version = "0.11.0" }
shipstern-parser = { version = "0.11.0" }
shipstern-proc-macro = { version = "0.11.0" }
```

`shipstern-core` is required: the generated code imports it directly.

Then, in your code:

```rust
use shipstern_proc_macro::include_shipstern_parser;

// Provide the path (relative to your crate root) to your Codama JSON IDL file.
include_shipstern_parser!("path/to/idl.json");
```

This macro will generate Rust modules containing type-safe account and instruction parsers for the specified Solana program. The input is a **Codama** JSON IDL, not a raw Anchor IDL. Output from Codama's own converter loads as it is, since `codama-nodes` defaults the collections its serializer omits. See [the Codama parser guide](../../docs/codama-parser-generation.md).

## Events and self-CPI events

Event parsing is gated behind the `program-events` feature. Without it the macro emits no event types, whatever the IDL declares:

```toml
shipstern-proc-macro = { version = "0.11.0", features = ["program-events"] }
```

Enabling it changes `InstructionParser::Output` from `Instructions` to `ProgramEventOutput`, which carries the decoded instruction alongside any events found on inner instructions. Because that is a public type, the feature propagates through Cargo's feature unification to every crate sharing the dependency.

See [the Codama parser guide](../../docs/codama-parser-generation.md) for how to declare a self-CPI event envelope in the IDL.

## Serde support

Generated types (structs, dispatch wrappers, and oneof enums) carry a `#[cfg_attr(feature = "serde", derive(::serde::Serialize, ::serde::Deserialize))]` attribute. To get `Serialize` / `Deserialize` impls, enable a `serde` feature in the crate where `include_shipstern_parser!` is invoked and have it pull in `serde`:

```toml
[dependencies]
serde = { version = "1", features = ["derive"], optional = true }

[features]
serde = ["dep:serde"]
```

This is handy for emitting parsed accounts and instructions as JSON — logging, ClickHouse ingestion, HTTP APIs, and so on. With the feature off, generated types stay Borsh-only and don't pull in serde.
