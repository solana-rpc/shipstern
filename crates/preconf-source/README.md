# shipstern-preconf-source

A source for Triton preconfirmations (preconfs) from two feeds. The Harmonic block builder
reports a transaction the moment it executes it into the slot under construction, before any shred
exists. A Jito BAM node reports a transaction after the leader executed and committed it, so its
result is the leader's. This source streams both into a Shipstern pipeline through the same
`SubscribeUpdate` that the other sources deliver, so existing transaction and instruction parsers
work unchanged.

## What a preconfirmation is not

A preconf is the block producer's statement, not the cluster's. Slots get skipped, blocks land on
abandoned forks, and a builder can restart a slot. Treat a preconf as the earliest signal and
check settlement elsewhere. Handlers must be idempotent per signature: a restarted slot delivers
its transactions again, and fast leader handover can send one transaction twice.

## Configuration

```toml
[source]
endpoint = "https://preconfs.rpcpool.com"   # default
x-token = "<token with the feeds enabled>"
harmonic-regions = ["fra", "ams"]           # one stream per region, nearby regions only
bam-regions = ["fra", "ams"]                # same rule; either list may be empty, not both
timeout = 10                                # seconds for the TCP connect, and again for TLS
auto-reconnect = true                       # default; resubscribe a dropped stream
# reconnect-max-retries = 10                # unset never gives up
```

Harmonic regions: `ams`, `ewr`, `fra`, `lon`, `tyo`, `sgp`, `slc`. BAM regions: `ams`, `dfw`,
`dub`, `ewr`, `fra`, `hkg`, `iad`, `lax`, `lon`, `pit`, `sea`, `sin`, `slc`, `sqq`, `tyo`.
Regional streams carry different slots, so a far region adds latency, not coverage. A region named
twice in one list opens one stream; the same region in both lists opens one stream per feed. Each
feed covers only the slots whose leader runs that client, so both feeds together give the most
coverage. Triton enables each feed on the token separately. A region of a feed the token lacks
gets `UNAUTHENTICATED` at subscribe, which stops the source before any stream runs.

```rust,ignore
let runtime = Runtime::builder()
    .instruction(Pipeline::new(MyParser, [handler]))
    .build::<shipstern_preconf_source::PreconfSource>(config);
```

## How a preconf maps to a `SubscribeUpdateTransaction`

| Field | Value |
|---|---|
| `filters` | the filter names the feed matched, which are the parser IDs |
| `slot` | the slot the builder is filling |
| `bank_id` | `0`, the feed has none |
| `transaction` | decoded from the bytes the block will carry; legacy, v0 and v1 messages |
| `is_vote` | computed from the message, the feed has no vote flag |
| `index` | arrival order inside the slot, from 0, not the block position |
| `meta.err` | `None` for a success; `Some` with empty bytes for a failure, the cause is unknown |
| other `meta` fields | absent: no fee, balances, logs, inner instructions or loaded addresses |
| `created_at` | unset, the feed sends no time |

Consequences:

- Log events and self-CPI events never fire, because there are no logs. Top-level instructions
  parse.
- A v0 transaction that uses an address lookup table fails every instruction parser, because the
  loaded addresses are unknown before execution.
- A transaction whose result the feed does not know is dropped, because `err: None` would read as
  a success. The first drop logs a warning, and the count is logged when the source stops.
- A transaction that does not decode is skipped with a warning and counted. It does not stop the
  source.
- A payload the crate does not know is ignored and counted, so a newer server schema shows in
  the exit summary.
- The source sends no slot updates. Harmonic slot framing drives only the `index` counter and
  the restart count. BAM has no framing: each transaction names its slot, and the `index` counts
  from 0 at the first transaction of a slot.
- The BAM fields `node`, `sequence`, `bundle_position` and `is_revert_on_error` have no place in a
  `SubscribeUpdateTransaction`. The source logs them at debug.

## How a prefilter maps to the feed filter

| Prefilter | Feed filter |
|---|---|
| `accounts_include` and the cuckoo set | `account_include` |
| `accounts_required` | `account_required` |
| `accounts_exclude` | `account_exclude` |
| `signature` | `signature` |
| `failed: Some(false)` (the default) | `execution_results = [SUCCESS]` |
| `failed: Some(true)` | `execution_results = [EXECUTION_FAILURE, FEES_ONLY]` |
| `failed: None` | no result filter; unknown results are dropped by the source |
| `vote` | applied by the source per parser, after decoding |
| `token_accounts` | refused, the feed matches account keys only |

The feed matches the static account keys of a transaction. A key that a v0 transaction loads
from a lookup table never matches, unlike on Dragon's Mouth.

The feed refuses a filter with no include, required or signature list, the full feed cannot be
subscribed, and the limits are 64 filters per stream, 64 bytes per filter name and 10000 accounts
per list. The source checks these before it dials and names the parser at fault, or the count.
A parser with no transaction prefilter gets nothing from this source; the source logs a warning
for each one at connect.

## Exit

A connect or subscribe failure stops the runtime with the gRPC code: `UNAUTHENTICATED` for a
token without the feed, `INVALID_ARGUMENT` for a filter the feed refuses, `FAILED_PRECONDITION`
for a region it does not serve. A status outside the retry set below ends the source with
`SourceExitStatus::StreamError`, for example `PERMISSION_DENIED` when the feed is disabled
under a live stream, or `NOT_FOUND` and `UNIMPLEMENTED`. A stream that fails for good stops
the source. While a stream resubscribes, the other streams keep delivering, so the run is
partial until it returns. There is no resume: the feed has no `from_slot`.

Over the coverage share (33% of the feed per account and region over 60 s) the feed withholds
transactions and sends a clip count, which the source logs at warn.

## Reconnect

With `auto-reconnect` on (the default), a stream that ends or fails with `UNAVAILABLE`,
`DATA_LOSS`, `RESOURCE_EXHAUSTED`, `INTERNAL`, `UNKNOWN`, `ABORTED`, `CANCELLED` or
`DEADLINE_EXCEEDED` is subscribed again on the same channel. The wait starts at 100 ms,
doubles after each failed attempt and stays at 10 s; a delivered transaction or slot start
starts it over, a ping or a clip does not. A subscribe that does not answer within `timeout`
seconds counts as `DEADLINE_EXCEEDED`. `reconnect-max-retries` bounds the consecutive failed
attempts. Ten attempts end 42.7 s after the drop, inside the 60 s coverage cooloff, so a bound
under 12 cannot survive one. Unset never gives up, because the feed has no resume and a late
reconnect is as good as an early one; Triton's own client defaults to the same. The first
subscribe at connect is never retried.

What a reconnect loses: the transactions between the drop and the new stream, and on Harmonic
the open slots of that stream, which get no end; framing restarts at the next slot start. BAM
resumes the slot in progress, so its indexes keep counting. Each attempt logs a warning with
the feed, the code, the attempt and the wait. The exit summary counts the subscribes that
succeeded. Reconnect state is log-only until source metrics land. An outage of minutes, for
example the Alpenglow switch on mainnet (Triton blog, 2026-10-14: both feeds off during the
switch), is bridged when the feeds return.

## Manual run

1. Get the feeds enabled on a Triton token.
2. Set `x-token` and one or two nearby regions per feed.
3. Run a pipeline with a narrow transaction prefilter, for example one program. A broad filter
   hits the coverage share within a minute.
4. Compare the signatures with a Dragon's Mouth stream at processed commitment. The landing
   rates and lead times are not measured yet.
