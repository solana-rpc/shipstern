# Yellowstone Fumarole Source Example

This example demonstrates how to consume Solana Geyser updates from the
[Yellowstone Fumarole](https://github.com/rpcpool/yellowstone-fumarole) service
using the `YellowstoneFumaroleSource` integration for Shipstern.

It wires two simple pipelines (SPL Token account + instruction) and logs parsed
values to stdout.

## Prerequisites

- Access to an active Yellowstone Fumarole endpoint.
- Possession of a valid `x-token`.
- An existing **persistent subscriber** registered in Fumarole (see details below).

## Create a persistent subscriber (required)

Fumarole uses _persistent subscribers_ to provide reliable/resumable streams. Before running this example, create a subscriber using the **Fumarole CLI** (`fume`) and then set that same name in Shipstern config as `subscriber-name`.

1. Install the CLI:

```sh
cargo install yellowstone-fumarole-cli
```

2. Create a CLI config file (YAML), for example `~/.fumarole/config.yaml`:

```yaml
endpoint: https://fumarole.endpoint.rpcpool.com
x-token: 00000000-0000-0000-0000-000000000000
```

3. Test the connection:

```sh
fume test-config
```

4. Create a persistent subscriber (at the tip of the log):

```sh
fume create --name my_consumer_group
```

That `--name` value (`my_consumer_group`) is what you will set as `subscriber-name` in the Shipstern
TOML config below.

## Run the example

1. Create a config file (for example `examples/fumarole/config.toml`):

```toml
[source]
endpoint = "https://YOUR-FUMAROLE-ENDPOINT"
x-token = "YOUR_X_TOKEN"              # optional
subscriber-name = "my_consumer_group" # required

# Optional:
# commitment-level = "processed"      # or "confirmed" / "finalized"
# max-decoding-message-size = 33554432 # bytes (default: usize::MAX)
# accept-compression = "zstd"          # "zstd" (default) or "gzip"

# Optional runtime buffer tuning:
# [buffer]
# jobs = 8
# sources-channel-size = 100
```

2. Run:

```bash
cargo run -p shipstern-example-fumarole -- --config ./Shipstern.toml
```

## What the example does

The entrypoint is `examples/fumarole/src/main.rs`:

- Reads a TOML config file from `--config`.
- Builds a Shipstern runtime with `Runtime::builder()` and `.build::<YellowstoneFumaroleSource>(config)`.
- Registers:
  - an instruction pipeline using `shipstern_spl_token_parser::InstructionParser`
  - an account pipeline using `shipstern_spl_token_parser::AccountParser`
- Logs parsed updates to stdout via a simple `Logger` handler.

## Source configuration options (Fumarole)

The `[source]` table is deserialized into
`shipstern_yellowstone_fumarole_source::FumaroleConfig`.

All keys are `kebab-case`.

| Key                         | Type                     | Required | Description                                                                                         |
| --------------------------- | ------------------------ | -------- | --------------------------------------------------------------------------------------------------- |
| `endpoint`                  | `string`                 | Yes      | The Yellowstone Fumarole server endpoint.                                                           |
| `x-token`                   | `string`                 | No       | Token used for authentication.                                                                      |
| `subscriber-name`           | `string`                 | Yes      | Persistent subscriber identity (consumer group name). Use a stable value for resuming.              |
| `commitment-level`          | `string`                 | No       | Overrides the `SubscribeRequest` commitment. Possible: `"processed"`, `"confirmed"`, `"finalized"`. |
| `max-decoding-message-size` | `int`                    | No       | Max decoded message size in bytes (default: `usize::MAX`).                                          |
| `accept-compression`        | `"zstd"` &#124; `"gzip"` | No       | Request/response compression.                                                                       |

### Prometheus-related fields

`FumaroleConfig` also defines (behind the crate feature `prometheus`) these keys:

- `metrics-endpoint`
- `metrics-job-name`
- `metrics-interval`

In the current `YellowstoneFumaroleSource` implementation, these are **declared
but not wired** (the source does not start a push loop itself). If you want
metrics, use Shipstern runtime metrics (see `examples/prometheus`) and push them
from your app.
