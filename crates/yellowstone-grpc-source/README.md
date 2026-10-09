# Shipstern Sources

## Overview

Shipstern introduces a flexible `Source` system that allows you to connect to various data sources and stream their updates through a standardized interface. This feature is designed to be extensible, allowing you to create custom sources while maintaining a consistent API.

## Key Features

- 🔌 **Standardized Connection Interface**: Connect to any data source using a unified API
- 📡 **Asynchronous Updates**: Stream data updates through channels
- ⚙️ **Configurable**: Set up filters and source-specific configurations
- 🔄 **Extensible**: Create your own custom sources

## How It Works

The [`Source`](https://github.com/solana-rpc/shipstern/blob/main/crates/runtime/src/sources.rs) trait provides a standardized way to:
- Connect to external data sources
- Stream updates through a channel to the Shipstern runtime for processing
- Configure filters for data processing
- Manage source-specific configuration

## Runtime filter updates

The gRPC source keeps the `SubscribeRequest` sink that `yellowstone-grpc-client`
returns next to the update stream, so a caller can change the subscription
without tearing down the connection and losing messages during the reconnect.

The sink itself stays inside this crate. Callers describe the subscription in
`Filters`, keyed by parser ID, and the source builds the request, so the set
cannot drift from the registered parsers and the runtime stays source-agnostic
rather than gRPC-shaped. There is no way to send a raw `SubscribeRequest`, and
that is deliberate.

Take a `RuntimeHandle` before running, because `run` and `run_async` both
consume the runtime, then edit the live set through it from wherever the
change originates. Handles are cheap to clone and share one view of the
filters. Nothing on the handle awaits, so it works the same from async code
and from a plain thread beside a blocking `run()`:

```rust
let runtime = Runtime::<YellowstoneGrpcSource>::builder()
    .account(Pipeline::new(TokenProgramAccParser, [Handler]))
    .try_build(config)?;

let handle = runtime.handle();
tokio::spawn(runtime.run_async());

// Widen the account subscription with one more owner.
let extra = Prefilter::builder().account_owners([new_mint]).build()?;
handle.update_filters(|filters| filters.merge(TokenProgramAccParser.id(), extra))?;

// Inspect what is live.
let current = handle.filters();

// Replace it wholesale with a set built from scratch, or go back to the start.
handle.send_filter_update(Filters::new(rebuilt))?;
handle.reset_filters()?;
```

`Filters` offers `get`, `insert`, `merge` and `remove` keyed by parser ID for
building the next set. `merge` unions the field sets, and an empty set means
"match everything" on the wire, so merging into a match-all narrows it rather
than widening; use `insert` to replace one deliberately. Build anything that
starts from the current set through `update_filters`, which holds a lock across
the read and the send. Reading `filters()` and sending the result back does not,
so two callers doing that lose one of the two edits.

Each `Filters` sent replaces the whole subscription rather than adding to it,
which is what the server does with a mid-stream request, so send the complete
set every time.

The map keys are parser IDs, and a set naming a parser with no registered
pipeline is refused with `FilterUpdateError::UnknownParser` before anything is
sent, since the server would stream data the runtime then discards. Take the
keys from `Parser::id()`, except for instruction parsers: the runtime bundles
every one of them behind a single `InstructionPipeline`, so the set holds one
entry under `InstructionPipeline::ID` whose prefilter is the union of theirs.
Naming an individual instruction parser is refused as unknown, and widening
there widens the whole bundle. `handle.filters().parser_ids()` reports the keys
that exist. Only the newest set matters: two updates in quick
succession may reach the source as the second alone, and a set the sink rejects
is dropped with a warning while `filters()` still reports it. A stream that can
take filter updates does not reconnect, because the client cannot replay a
changed request.

The server applies the new set promptly once it has it, but two different lags
sit in front of that. The source awaits the runtime buffer inside the same
`select!` that watches for filter updates, so while that buffer is full the
update has not reached the sink at all and the server has not been told. Once
it has, you still see the change only when whatever is already queued drains.
Both delays are however far behind your pipeline already was rather than
properties of the update. Against a live endpoint, a consumer
running about 15 seconds behind kept receiving the old set for roughly that
long, and the first updates matching the new set arrived stale by the same
margin before catching up. A pipeline keeping pace sees the change almost at
once. A returned `send` means the request was handed off, not that the
subscription has changed.

A set the server refuses, by exceeding its configured filter limits for
example, comes back on the stream rather than on the sink, and it ends the run.
Validate against the provider's limits before sending one, and read
`filter_updates_sent` in the stream-error log to tell a refused update apart
from an unrelated server error.

`Runtime::handle` exists only for sources implementing `FilterUpdateSource`,
which today is gRPC alone, so a runtime on any other source has no handle to
take and the mistake is a compile error. An update fails with
`FilterUpdateError::Closed` once the runtime has stopped. Whether an update takes effect
also depends on the provider. Both `yellowstone-grpc-geyser` and `richat` apply
mid-stream requests to a live subscription, but a deployment can sit behind
infrastructure that does not forward them.

## Creating a Custom Source

Here's a step-by-step guide to creating your own source:

```rust
use async_trait::async_trait;
use tokio::sync::mpsc::Sender;
use shipstern::sources::Source;
use shipstern::config::YellowstoneConfig;
use shipstern_core::Filters;

#[derive(Debug)]
struct MyCustomSource {
    filters: Option<Filters>,
    config: Option<YellowstoneConfig>,
}

#[async_trait]
impl Source for MyCustomSource {
    async fn connect(
        &self,
        tx: Sender<Result<SubscribeUpdate, Status>>,
    ) -> Result<JoinSet<()>, crate::Error> {
        // Your connection logic here
        todo!()
    }

    fn name(&self) -> String {
        "my-custom-source".to_string()
    }

    // ... other required methods
}
```

## Required Methods

| Method | Description |
|--------|-------------|
| `connect` | Establishes connection to the data source and streams updates |
| `name` | Returns a unique identifier for the source |
| `set_filters_unchecked` | Sets filters for data processing |
| `set_config_unchecked` | Sets source-specific configuration |
| `get_filters` | Retrieves current filters |
| `get_config` | Retrieves current configuration |

## Optional Methods

The trait provides two optional methods with safe default implementations:

- `filters`: Safely sets filters if none are currently set
- `config`: Safely sets configuration if none is currently set

## Best Practices

1. **Naming**: Choose clear, descriptive names for your sources
2. **Error Handling**: Implement proper error handling in your `connect` method
3. **Resource Management**: Ensure proper cleanup of resources when the source is dropped
4. **Configuration**: Use the configuration system to make your source flexible
5. **Filtering**: Implement efficient filtering to reduce unnecessary data transfer

## Example Use Case

Here's a practical example of how to use a source:

```rust
shipstern::Runtime::builder()
    // Add the source to the runtime
    .source(YellowstoneGrpcSource::new())
    // We could call this multiple times to add concurrent Sources
    // .source(SolanaAccountsRpcSource::new())
    .account(Pipeline::new(TokenProgramAccParser, [Handler]))
    .account(Pipeline::new(TokenExtensionProgramAccParser, [Handler]))
    .instruction(Pipeline::new(TokenExtensionProgramIxParser, [Handler]))
    .instruction(Pipeline::new(TokenProgramIxParser, [Handler]))
    .build(config)
    .run();
```

## 🔮 Roadmap

### 📅 Planned Features
| Feature | Priority | Description |
|---------|----------|-------------|
| Source Testing Harness | High | Make it easy to test `Source` implementations and speed up contributions |
| Space for cleanup logic | Medium | Expose a method that can be used for Sources that need to cleanup resources |
| Support additional data sources | Medium | Add support for additional data sources |



## Contributing

We welcome contributions to expand the ecosystem of sources! When creating a new source:

1. Follow the trait implementation guidelines
2. Include comprehensive documentation
3. Consider adding example usage

## Support

If you need help or have questions, please open an issue on [GitHub](https://github.com/solana-rpc/shipstern) or also check other sources implementations in the repository.

---
