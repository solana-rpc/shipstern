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
let runtime = Runtime::builder()
    .account(Pipeline::new(TokenProgramAccParser, [Handler]))
    .try_build::<YellowstoneGrpcSource>(config)?;

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
succession may reach the source as the second alone, and a set rejected while
the source is between connections is retried once the stream recovers, but the
sender is not told either way. With auto-reconnect off there is nothing to
recover into, so such a set is dropped with a warning while `filters()` still
reports it.

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
example, comes back on the stream rather than on the sink. What happens next
depends on the code. A code the client treats as terminal ends the run. A
recoverable one does not: the sink records a request into its reconnect state
as soon as the local channel takes it, before the server has seen it, so the
client reconnects and resubscribes with the same refused set, and the loop
repeats. `ResourceExhausted` is recoverable and is exactly what a "too many
filters" rejection can carry, so a set the provider will not accept can leave
the runtime reconnecting indefinitely rather than either applying or stopping.
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

A source is a value the runtime is handed. Implement `SourceTrait` with one
method, `connect`. It receives a `SourceContext` carrying the filter set
derived from the registered pipelines and the channel to send updates on,
streams until the stream ends, and returns how it ended:

```rust
use async_trait::async_trait;
use shipstern::sources::{SourceContext, SourceExitStatus, SourceTrait};

#[derive(Debug)]
struct MySource {
    endpoint: String,
}

#[async_trait]
impl SourceTrait for MySource {
    async fn connect(&self, ctx: SourceContext) -> Result<SourceExitStatus, shipstern::Error> {
        let SourceContext { filters, tx, .. } = ctx;

        // Open the stream for `filters` and forward each update into `tx`.
        Ok(SourceExitStatus::Completed)
    }
}
```

The context is `#[non_exhaustive]`, so destructure it with a trailing `..`.
New fields can then be added without breaking your source.

Hand an instance to the builder with `try_build_with`. The builder is not
tied to a source until then, so the source type is inferred:

```rust
Runtime::builder()
    .account(Pipeline::new(TokenProgramAccParser, [Handler]))
    .try_build_with(MySource { endpoint }, buffer_config)?
    .run_async()
    .await;
```

### Building from a config document

Implement `FromConfig` as well and the runtime can construct the source from
its section of a `ShipsternConfig`, which is what `try_build::<MySource>` does.
The turbofish names the source, since a config type alone does not. This is
how every source in this repository is wired up so a TOML file or CLI flags
can select it:

```rust
use shipstern::sources::FromConfig;

impl FromConfig for MySource {
    type Config = MyConfig;

    fn from_config(config: Self::Config) -> Self { Self { endpoint: config.endpoint } }
}

let config: ShipsternConfig<MyConfig> = toml::from_str(&text)?;

Runtime::builder()
    .account(Pipeline::new(TokenProgramAccParser, [Handler]))
    .try_build::<MySource>(config)?
    .run();
```

### Applying filter updates

Implement `FilterUpdateSource` and read `ctx.filter_updates` in `connect`,
applying each set published there to the live subscription, as the gRPC source
does. That is what makes `Runtime::handle` available to callers. Sources that
cannot change a subscription mid-stream ignore the slot and skip the trait.

### Picking a source at startup

`Box<dyn SourceTrait>` is a source too, and it is what a bare `Runtime` holds,
so one binary can choose between sources from its config and build once:

```rust
let source: Box<dyn SourceTrait> = match config.kind {
    Kind::Live => Box::new(YellowstoneGrpcSource::from_config(config.grpc)),
    Kind::Backfill => Box::new(JetstreamSource::from_config(config.jetstream)),
};

let runtime: Runtime = Runtime::builder()
    .account(Pipeline::new(TokenProgramAccParser, [Handler]))
    .try_build_with(source, config.buffer)?;
```

Box a `dyn FilterUpdateSource` instead to keep `Runtime::handle`.

### Best Practices

1. **Exit status**: Return an `Err` when the source fails, and the `SourceExitStatus` that describes how the stream ended otherwise. A stream loop that `break`s with its result can end on `StreamError` or `Error` instead, and the runtime stops with an error for those too.
2. **Backpressure**: `tx.send(..).await` fails once the runtime has stopped. Treat that as a signal to return with `ReceiverDropped`, not as an error.
3. **Filters**: Translate the whole `Filters` set into the narrowest subscription the provider supports, so the runtime discards as little as possible.
4. **State**: Anything the source needs beyond its config, a shared client for instance, can live on the struct since the caller constructs it.

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
