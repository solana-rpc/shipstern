# shipstern-preconf-source

A source for Triton preconfirmations (preconfs) from two feeds:

- Harmonic: the block builder reports a transaction when it selects it for the slot under
  construction and executes it against its own state. This is before the leader executes it and
  before any shred exists.
- BAM: a Jito BAM node reports a transaction after the leader executed and committed it. So its
  result is the leader's result.

The source sends both as the same `SubscribeUpdate` that the other sources send, so existing
transaction and instruction parsers work with no change.

## What a preconfirmation is not

A preconf is the block producer's statement, not the cluster's. Slots get skipped, blocks land on
abandoned forks, and a leader can restart a slot. Treat a preconf as an early signal, and check
settlement elsewhere. It usually arrives ahead of Geyser when the client runs near the feed region.

Handlers must be idempotent per signature: a restarted slot delivers its transactions again. The
new frame can also leave out a transaction that the first frame carried. That transaction stays
delivered, and the source cannot retract it. On Harmonic, the source logs a restart at warn. BAM
has no slot framing, so the source cannot see a BAM restart.

## Configuration

```toml
[source]
x-token = "<token with the feeds enabled>"
harmonic-regions = ["fra", "ams"]
bam-regions = ["fra", "ams"]
```

| Key | Default | Meaning |
|---|---|---|
| `endpoint` | `https://preconfs.rpcpool.com` | the Triton preconfs endpoint |
| `x-token` | unset | the Triton token; Triton enables each feed on it separately |
| `harmonic-regions` | `[]` | Harmonic regions, one stream each |
| `bam-regions` | `[]` | BAM regions, one stream each; at least one of the two lists needs a region |
| `timeout` | `10` | seconds for the TCP connect, for TLS and for each subscribe |
| `auto-reconnect` | `true` | subscribe a dropped stream again, see Reconnect |
| `reconnect-max-retries` | unset | resubscribes in a row before the source stops; one that works also counts until a transaction or slot start arrives or the stream stays up 60 s; unset has no limit; `0` never resubscribes; use 12 or more to outlast a cooloff, see Reconnect |

Harmonic regions: `ams`, `ewr`, `fra`, `lon`, `tyo`, `sgp`, `slc`. BAM regions: `ams`, `dfw`,
`dub`, `ewr`, `fra`, `hkg`, `iad`, `lax`, `lon`, `pit`, `sea`, `sin`, `slc`, `sqq`, `tyo`.

Each regional stream carries the slots of the leaders that the region serves. A far region adds
slots, but they arrive late. A region named twice in one list opens one stream. The same region
in both lists opens one stream per feed.

Each feed covers only the slots whose leader runs that client, so both feeds together give the
most coverage. Some slots carry nothing and get no clip: the slots of a Harmonic leader with its
scheduler off, and the slots of a BAM leader that opted out of preconfs. With `failed: Some(_)`,
the feed also does not send the transactions of a BAM node that does not report results, and
sends no clip for them.

Triton counts messages and stream slots per subscription and feed, prices both, and bills the
greater amount. Each open stream adds every slot its region produces, matched or empty.

```rust,ignore
let runtime = Runtime::builder()
    .instruction(Pipeline::new(MyParser, [handler]))
    .build::<shipstern_preconf_source::PreconfSource>(config);
```

## How a preconf maps to a `SubscribeUpdateTransaction`

| Field | Value |
|---|---|
| `filters` | the filter names the feed matched: the ID of each transaction parser, and `InstructionPipeline` for all instruction parsers together |
| `slot` | the slot the feed names |
| `bank_id` (proto 14) | 0, the feed has no bank |
| `transaction` | decoded from the bytes the block will carry; legacy, v0 and v1 messages |
| `is_vote` | computed from the message, the feed has no vote flag |
| `index` | arrival order inside the slot, from 0, counted per stream, not the block position; no repeat within 64 slots of the newest |
| `meta.err` | `None` for a success; `Some` with empty bytes for a failure; the cause is unknown, so only `is_some()` has meaning |
| other `meta` fields | absent: no fee, balances, logs, inner instructions or loaded addresses |
| `created_at` | unset, the feed sends no time |

Consequences:

- Log events never fire, because there are no logs. Self-CPI events never fire, because there
  are no inner instructions. Top-level instructions parse.
- A v0 transaction whose instructions use an account from a lookup table fails every instruction
  parser, because the feed does not resolve lookup tables. The runtime logs one `Handler failed`
  error for each such transaction, with `Invalid account key index` in `err`.
- A transaction whose result the feed does not know is dropped, because `err: None` would read as
  a success. With `failed: Some(_)`, as by default, the feed does not send it and sends no clip,
  so the source does not see or count it. With `failed: None`, the source drops it, warns at the
  first drop on each stream, and counts it as `unknown_result`.
- A transaction that does not decode is skipped with a warning and counted. It does not stop the
  source.
- The source ignores a payload the crate does not know and counts it as `unknown_payload`, so a
  newer server schema shows in the exit summary.
- The source sends no slot updates. The Yellowstone slot statuses report a validator's view of a
  slot: its bank, its shreds and its commitment. The builder reports none of these. So a handler
  cannot know when a slot is complete.
- Harmonic slot starts and ends only set the `out_of_frame` and `restarts` counts. A new start
  for a slot that ended counts as a restart. A slot end also lets the source forget the index of
  each slot more than 64 slots below it. The `index` counts every delivered transaction of a
  slot, also across a restart. BAM has no slot starts or ends. Each BAM transaction names its
  slot, and the `index` counts from 0 at the first transaction of a slot.
- The Harmonic fields `region` and `seq` have no place in a `SubscribeUpdateTransaction`. The
  source logs them at debug with the signature. `seq` is the batch number, from 0 in each slot.
- The BAM fields `node`, `sequence`, `bundle_position` and `is_revert_on_error` have no place in a
  `SubscribeUpdateTransaction`. The source logs them at debug with the signature, the last one as
  `revert_on_error`.
- A gap in `sequence` is not proof of a loss. The node gives one number to each batch it
  schedules, for all the leaders it serves. All transactions of one bundle share a number. These
  batches leave gaps: batches that no filter of the stream matches, batches the feed clips,
  batches of a leader that opted out of preconfs, and batches sent during a reconnect.

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
| `vote` | applied by the source per filter name, after decoding |
| `token_accounts` | refused, the feed matches account keys only |

The feed matches the static account keys of a transaction. A key that a v0 transaction loads
from a lookup table never matches, unlike on Dragon's Mouth.

The feed refuses a filter with no include, required or signature list, so a client cannot
subscribe to the full feed. The feed allows 64 filters per stream, 64 bytes per filter name and
10000 accounts per list. The source checks these rules before it dials. The error names the
parser (`InstructionPipeline` for any instruction parser) or the count. The source sends no
signer or instruction selectors.

A parser with no transaction prefilter gets nothing from this source. The source logs a warning
for each such parser at connect.

## Filter updates

The source does not implement `FilterUpdateSource`, so `Runtime::handle` is not available for
it. The feed cannot change the filters of an open stream. To change them, a client opens a
stream with the new filters and then closes the old stream. A Harmonic stream starts at the next
slot start, so the old stream must stay open until then. While both streams are open, a
transaction can arrive twice. A later version of the source can do this swap.

## Exit

| Case | The source returns |
|---|---|
| no region in either list | `Error::Other` with `ConnectError::NoRegion`, before the dial |
| no transaction prefilter, or one the feed cannot take | `Error::Other` with a `FilterError`, before the dial |
| no scheme, or a scheme other than `https` or `http` | `Error::Other` with `ConnectError::Scheme`, before the dial |
| a token on an `http` endpoint whose host is not `127.0.0.1`, `localhost` or `[::1]` | `Error::Other` with `ConnectError::PlainTextToken`, before the dial |
| a token that is not a valid header | `Error::Other` with `ConnectError::Token`, before the dial |
| a bad endpoint, or a TCP or TLS failure | `Error::Other` with `ConnectError::Transport` |
| the first subscribe of a stream fails, with any code | `Error::YellowstoneStatus` with the code; never retried |
| a live stream or a resubscribe fails with a code that stops, or the retries run out | `SourceExitStatus::StreamError` with the code |
| a live stream ends and is not subscribed again | `SourceExitStatus::StreamEnded` |
| the runtime drops the receiver | `SourceExitStatus::ReceiverDropped` |

The runtime returns each `Err` as it is. It turns `StreamError` into `Error::YellowstoneStatus`
and `StreamEnded` into `Error::ServerHangup`.

| Code | Cause | After the first subscribe, with `auto-reconnect` on |
|---|---|---|
| `UNAUTHENTICATED` | no token, an unknown token, or a token without the feed | stop |
| `INVALID_ARGUMENT` | the feed refuses the request, for example a filter over a limit | stop |
| `FAILED_PRECONDITION` | the feed does not serve the region | stop |
| `PERMISSION_DENIED` | the feed was disabled under a live stream | stop |
| `RESOURCE_EXHAUSTED` | the account was over its coverage share for 30 s, a subscribe came inside the 60 s cooloff that follows, or the source fell behind | subscribe again |
| `DATA_LOSS` | the source fell so far behind that the server dropped its position | subscribe again |
| `UNAVAILABLE` | the point of presence is restarting | subscribe again |
| `DEADLINE_EXCEEDED` | a subscribe did not answer within `timeout` seconds | subscribe again |
| `INTERNAL`, `UNKNOWN`, `ABORTED`, `CANCELLED` | | subscribe again |
| any other code, for example `NOT_FOUND` or `UNIMPLEMENTED` | | stop |

With `auto-reconnect` off, every code stops. There is no resume: the feed has no `from_slot`.

## Coverage share

Each account gets at most 33% of what the feed publishes in a region, counted over a sliding
60 s window. All streams of the account in that region count against the same share. Harmonic
and BAM have separate shares. Over the share, the feed withholds transactions and sends a clip
count, which the source logs at warn. After 30 s over the share, the feed ends the stream with
`RESOURCE_EXHAUSTED`. Then a 60 s cooloff starts. During the cooloff, the feed refuses each
subscribe to that region with `RESOURCE_EXHAUSTED`, and the source retries (see Reconnect). A
whole-program filter, such as SPL Token or pump.fun, reaches the share within a minute.

## Reconnect

With `auto-reconnect` on (the default), the source subscribes a stream again on the same channel
when the stream ends, or when it fails with a code marked "subscribe again" in Exit. The first
subscribe at connect is never retried.

The wait before an attempt starts at 100 ms and doubles with each attempt, up to 10 s. A
transaction or a slot start from the server sets the wait and the attempt count back to the
start. So does a drop after a stream that stayed up for 60 s. A ping, a clip or a slot end does
not. A subscribe that does not answer within `timeout` seconds fails with `DEADLINE_EXCEEDED`.
At connect, this stops the source.

`reconnect-max-retries` bounds the resubscribes in a row. A resubscribe that works also counts,
until one of the resets above. 0 never resubscribes. The waits before 11 attempts add up to
52.7 s, inside the 60 s cooloff (see Coverage share), so a bound under 12 cannot outlast a
cooloff. Unset never gives up, as in Triton's own client. The feed has no resume, so a stop
cannot save the transactions that a late reconnect loses.

A reconnect loses the transactions between the drop and the new stream. On Harmonic, the open
slots of that stream get no slot end, and framing starts again at the next slot start. BAM
resumes the slot in progress. Indexes keep counting on both feeds. While one stream reconnects,
the other streams keep delivering, so the run is partial until it returns.

Each retry logs a warning with the feed, the code, the message, the attempt and the wait. The
`reconnects` count in each exit line counts the resubscribes that succeeded. The source has no
metrics.

When a point of presence restarts (`UNAVAILABLE`), the source subscribes again. Triton states
that anycast then lands the subscribe on another point of presence. When a feed is down, the
stream stays open and gets no transactions. There is no error, so the source does not
reconnect.

## Manual run

1. Get the feeds enabled on a Triton token.
2. Set `x-token` and one or two nearby regions per feed.
3. Run a pipeline with a narrow transaction prefilter, for example one pool, wallet or mint. A
   whole-program filter reaches the coverage share (see Coverage share).
4. Compare the signatures with a Dragon's Mouth stream at processed commitment. Triton states
   that about 99% of Harmonic and 91% of BAM preconfs arrive before Geyser. Landing rates are not
   measured yet.
