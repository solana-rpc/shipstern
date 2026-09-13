# Walkthrough: An Anchor IDL to a Working Parser

A runnable, end-to-end path from an Anchor IDL to a Shipstern parser in your own
crate, using the `include_shipstern_parser!` proc macro.

[`codama-parser-generation.md`](./codama-parser-generation.md) is the reference:
what the macro reads, how the self-CPI envelope resolves, and the byte layouts.
This guide is the walkthrough that stands it up from nothing. Read that one when
you need a rule; read this one when you need a working crate.

Every command, file and output below was run against the published `0.10.0`
crates. See [Verification](#verification).

## Before you start

| Pin | Value | Why it matters to you |
|---|---|---|
| MSRV | 1.93 | `rust-toolchain.toml`; Agave 4.2.2 requires it |
| Solana/Agave stack | 4.2.2 | pulled in transitively through `shipstern-core` |
| `codama-nodes` (Rust) | `=0.9.1` | exact pin, and it decides what your IDL must contain |
| Node.js | any current LTS | only for the one-time IDL conversion |

You do not depend on `codama-nodes` yourself. It is a dependency of
`shipstern-proc-macro`, but the exact pin is what step 1 works around, so it is
worth knowing.

## 1. Convert the Anchor IDL to Codama JSON

The macro reads Codama nodes. An Anchor IDL has to be converted first, and
`JSON.stringify(rootNodeFromAnchor(idl))` on its own does not produce a file the
macro will accept. Two independent things get in the way.

**Codama freezes its nodes.** The tree comes back frozen all the way down, so
nothing can be added in place. Under an ES module, where strict mode is always
on, the assignment throws rather than failing silently:

```javascript
const node = rootNodeFromAnchor(idl);

Object.isFrozen(node);                  // true
Object.isFrozen(node.program);          // true
Object.isFrozen(node.program.accounts); // false, but every element is frozen

node.additionalPrograms = [];
// TypeError: Cannot add property additionalPrograms, object is not extensible
```

Round-tripping through JSON gives back an unfrozen tree that can be patched.

**Some collections have no serde default.** Codama's JavaScript omits empty
collections. Fourteen node kinds in `codama-nodes` 0.9.1 carry a collection
field declared without `#[serde(default)]`, so on those an omitted collection is
a hard error rather than an empty vector. The macro surfaces it as:

```
error: Failed to load/parse IDL from ".../idls/counter.json":
       Failed to parse JSON: missing field `additionalPrograms`
```

Which field you hit depends on which collection happened to be empty:
`additionalPrograms` for any IDL at all, `accounts` for an instruction that
takes none, `fields` for a fieldless struct. Backfilling them by node kind fixes
all of it at once.

Optional fields need no help. `Option<T>` fields such as `accountNode.size`,
`accountNode.pda` and `instructionArgumentNode.defaultValue` deserialise as
`None` when absent, so the backfill only has to cover collections.

```javascript
// codama.mjs
import fs from "node:fs";
import path from "node:path";
import { rootNodeFromAnchor } from "@codama/nodes-from-anchor";

//
// Collection fields that codama-nodes 0.9.1 deserialises without a serde
// default, and that Codama's JavaScript omits when they are empty.
//
// Derived from the `#[node]` and `#[type_node]` structs in codama-nodes 0.9.1.
// Re-derive this table if the pin in the workspace Cargo.toml ever moves.
//
const REQUIRED_COLLECTIONS = {
    arrayValueNode: ["items"],
    enumTypeNode: ["variants"],
    hiddenPrefixTypeNode: ["prefix"],
    hiddenSuffixTypeNode: ["suffix"],
    instructionNode: ["accounts", "arguments"],
    mapValueNode: ["entries"],
    pdaNode: ["seeds"],
    pdaValueNode: ["seeds"],
    rootNode: ["additionalPrograms"],
    setValueNode: ["items"],
    structTypeNode: ["fields"],
    structValueNode: ["fields"],
    tupleTypeNode: ["items"],
    tupleValueNode: ["items"],
};

function backfill(value) {
    if (Array.isArray(value)) return value.map(backfill);
    if (value === null || typeof value !== "object") return value;

    for (const key of Object.keys(value)) value[key] = backfill(value[key]);

    for (const field of REQUIRED_COLLECTIONS[value.kind] ?? []) {
        value[field] ??= [];
    }

    return value;
}

const [, , input = "idl.json", output = "idl.codama.json"] = process.argv;

const anchorIdl = JSON.parse(fs.readFileSync(path.resolve(input), "utf8"));

// Codama freezes every node, so mutate a JSON clone, not the node tree.
const codamaIdl = backfill(JSON.parse(JSON.stringify(rootNodeFromAnchor(anchorIdl))));

fs.writeFileSync(path.resolve(output), `${JSON.stringify(codamaIdl, null, 2)}\n`);

console.log(`wrote ${output}`);
```

```bash
pnpm install @codama/nodes-from-anchor
node codama.mjs idl.json idls/counter.json
```

Commit the converted file. It is an input to your build, and regenerating it is
a deliberate step taken when the program's IDL changes, not something a build
script should do behind your back.

If your IDL is already in complete Codama form, skip this step. The files in
`tests/idls/` are all in that shape and are useful to compare against.

### The Anchor IDL used below

```json
{
  "address": "Cnt1111111111111111111111111111111111111111",
  "metadata": { "name": "counter", "version": "0.1.0", "spec": "0.1.0" },
  "instructions": [
    {
      "name": "increment",
      "discriminator": [11, 18, 104, 9, 104, 174, 59, 33],
      "accounts": [
        { "name": "counter", "writable": true },
        { "name": "authority", "signer": true },
        { "name": "event_authority" },
        { "name": "program" }
      ],
      "args": [{ "name": "amount", "type": "u64" }]
    }
  ],
  "accounts": [
    { "name": "Counter", "discriminator": [255, 176, 4, 245, 188, 253, 124, 25] }
  ],
  "events": [
    { "name": "IncrementEvent", "discriminator": [64, 198, 205, 232, 38, 8, 113, 226] }
  ],
  "errors": [{ "code": 6000, "name": "Overflow", "msg": "counter overflowed" }],
  "types": [
    {
      "name": "Counter",
      "type": {
        "kind": "struct",
        "fields": [
          { "name": "authority", "type": "pubkey" },
          { "name": "count", "type": "u64" },
          { "name": "bump", "type": "u8" }
        ]
      }
    },
    {
      "name": "IncrementEvent",
      "type": {
        "kind": "struct",
        "fields": [
          { "name": "counter", "type": "pubkey" },
          { "name": "new_count", "type": "u64" }
        ]
      }
    }
  ]
}
```

## 2. Set up the consuming crate

```toml
[dependencies]
borsh = { version = "^1.0.0", features = ["derive"] }
shipstern-core = { version = "0.10.0" }
shipstern-parser = { version = "0.10.0" }
shipstern-proc-macro = { version = "0.10.0" }
```

All four are required, and the reason is the same in each case: the macro
expands into your crate, so whatever the expansion names has to resolve there.

| Dependency | What the expansion needs it for |
|---|---|
| `shipstern-proc-macro` | the macro itself |
| `shipstern-parser` | `use shipstern_parser::prelude::*` at the top of the module |
| `shipstern-core` | `::shipstern_core::` paths, `Pubkey`, `InstructionUpdate`, `ParseError` |
| `borsh` with `derive` | `#[derive(::borsh::BorshDeserialize, ::borsh::BorshSerialize)]` on every type |

Dropping either of the last two fails at expansion, not at link time:

```
error[E0433]: cannot find `BorshDeserialize` in `borsh`     // derive feature off
error[E0432]: unresolved import `shipstern_core`            // crate not declared
```

### Events, and what enabling them changes

Events are behind the `program-events` feature on `shipstern-proc-macro`:

```toml
shipstern-proc-macro = { version = "0.10.0", features = ["program-events"] }
```

Without it the macro emits no event types at all, however the IDL declares its
events. `Events`, `event::Event`, `resolve_event_default` and
`resolve_events_from_logs` are simply absent, so referencing one is a compile
error at the call site rather than a runtime miss.

With it, `InstructionParser::Output` changes from `Instructions` to
`ProgramEventOutput`:

```rust
// without program-events
Instructions { instruction: Increment { .. } }

// with program-events
ProgramEventOutput {
    instruction: Some(Instructions { .. }),
    program_events: vec![Events { .. }],
}
```

That is a public type change driven by a Cargo feature, which makes it subject
to feature unification: if any crate in your workspace turns `program-events`
on, every crate sharing that dependency sees it on, and their parsers change
output type too. This repository hits exactly that, which is why
`tests/proc-macro-events` is listed under `exclude` in the workspace
`Cargo.toml` rather than being a normal member. If you need both shapes, put
them in separate workspaces.

`CustomInstructionParser` is the exception: its `Output` stays `Instructions`
whether or not the feature is on. A custom resolver sees instructions only, and
does not collect events.

### Protobuf output

The separate `proto` feature makes the generated types derive `prost::Message`
and exposes `PROTOBUF_SCHEMA`. It needs two more things in your manifest:

```toml
prost = "0.14"
shipstern-core = { version = "0.10.0", features = ["proto"] }
shipstern-proc-macro = { version = "0.10.0", features = ["proto"] }
```

Leaving `shipstern-core`'s own `proto` feature off fails with
`cannot find PublicKeyProtoWrapper in shipstern_core`. Feature unification
applies here too.

## 3. Invoke the macro

```rust
// src/lib.rs
use shipstern_proc_macro::include_shipstern_parser;

include_shipstern_parser!("idls/counter.json");
```

The path resolves against `CARGO_MANIFEST_DIR`, your crate root. Nothing is
written to disk and no build script runs.

The generated module takes the program's name in snake_case, so `counter` here:

| Item | Type |
|---|---|
| `counter::PROGRAM_ID` | `[u8; 32]` |
| `counter::AccountParser` | `Parser<Input = AccountUpdate, Output = CounterAccount>` |
| `counter::InstructionParser` | `Parser<Input = InstructionUpdate, Output = Instructions>`, without `program-events` |
| `counter::CounterAccount` | account dispatch wrapper, with `try_unpack` |
| `counter::Counter` | the decoded account, with `DISCRIMINATOR` and `DISCRIMINATOR_OFFSET` |
| `counter::Instructions` | instruction dispatch wrapper, with `INCREMENT_DISCRIMINATOR` |
| `counter::instruction::{IncrementAccounts, IncrementArgs}` | per-instruction types |
| `counter::resolve_instruction_default` | `(accounts, data, path) -> ParseResult<Instructions>` |

`resolve_instruction_default` takes three arguments, the third being the
instruction's `Path`, which it uses in the `DiscriminatorNotFound` message.
`InstructionResolver::resolve` takes the same three, so a custom resolver has to
accept and forward the path.

> The snippet under *Handling Discriminator Collisions* in the root README still
> shows the older two-argument form. The three-argument signature is what 0.10.0
> ships. `Pubkey` there is an alias for `KeyBytes<32>`, so the account slice type
> in that snippet is right either way.

## 4. Parse an instruction, an account and an event

Everything below runs offline against bytes built in the test, so it needs no
RPC endpoint and no fixtures. For parsing against recorded mainnet data instead,
see [`crates/mock`](../crates/mock/README.md).

```rust
// tests/parse.rs
use borsh::BorshSerialize;
use my_crate::counter;
use shipstern_core::{instruction::Path, Pubkey};

fn key(byte: u8) -> Pubkey { Pubkey::new([byte; 32]) }

#[test]
fn parse_account() {
    let mut data = counter::Counter::DISCRIMINATOR.to_vec();

    counter::Counter { authority: key(1), count: 7, bump: 254 }
        .serialize(&mut data)
        .unwrap();

    let parsed = counter::CounterAccount::try_unpack(&data).unwrap();

    println!("{parsed:#?}");
}

#[test]
fn parse_instruction() {
    let mut data = counter::Instructions::INCREMENT_DISCRIMINATOR.to_vec();
    data.extend_from_slice(&5_000_000_000u64.to_le_bytes());

    let accounts = vec![key(2), key(1), key(3), Pubkey::new(counter::PROGRAM_ID)];

    let parsed =
        counter::resolve_instruction_default(&accounts, &data, &Path::new_single(0)).unwrap();

    println!("{parsed:#?}");
}
```

The discriminator is a memcmp predicate, not a payload boundary: the bytes the
parser compares at `DISCRIMINATOR_OFFSET`. Prepending it as above is correct for
this IDL because Anchor's discriminator is a byte prefix, but decoding always
goes back through `try_unpack`, which knows where each account's body starts.

`parse_account` prints:

```
CounterAccount {
    account: Counter(
        Counter {
            authority: KeyBytes(
                "4vJ9JU1bJJE96FWSJKvHsmmFADCg4gpZQff4P3bkLKi",
            ),
            count: 7,
            bump: 254,
        },
    ),
}
```

`parse_instruction` prints:

```
Instructions {
    instruction: Increment {
        accounts: IncrementAccounts {
            counter: KeyBytes(
                "8qbHbw2BbbTHBW1sbeqakYXVKRQM8Ne7pLK7m6CVfeR",
            ),
            authority: KeyBytes(
                "4vJ9JU1bJJE96FWSJKvHsmmFADCg4gpZQff4P3bkLKi",
            ),
            event_authority: KeyBytes(
                "CktRuQ2mttgRGkXJtyksdKHjUdc2C4TgDzyB98oEzy8",
            ),
            program: KeyBytes(
                "Cnt1111111111111111111111111111111111111111",
            ),
            remaining_accounts: [],
        },
        args: IncrementArgs {
            amount: 5000000000,
        },
    },
}
```

### The event, through `InstructionParser`

This part needs `program-events`, and therefore a crate where the feature is on.
An `emit_cpi!` event reaches the parser as an inner instruction of the
instruction that emitted it, with data shaped
`[envelope tag][event discriminator][borsh payload]`. This IDL declares no
envelope, so the parser uses Anchor's default 8-byte tag.

```rust
// tests/events.rs
use std::sync::Arc;

use borsh::BorshSerialize;
use my_events_crate::counter;
use shipstern_core::{
    instruction::{InstructionShared, InstructionUpdate, Path},
    Parser, Pubkey,
};

/// Anchor's `emit_cpi!` tag: the first 8 bytes of `sha256("anchor:event")`,
/// little-endian on the wire.
const ANCHOR_EVENT_IX_TAG: [u8; 8] = 0x1d9a_cb51_2ea5_45e4_u64.to_le_bytes();

fn key(byte: u8) -> Pubkey { Pubkey::new([byte; 32]) }

#[tokio::test]
async fn parse_instruction_and_event() {
    let program = Pubkey::new(counter::PROGRAM_ID);
    let shared = Arc::new(InstructionShared::default());

    let mut ix_data = counter::Instructions::INCREMENT_DISCRIMINATOR.to_vec();
    ix_data.extend_from_slice(&7u64.to_le_bytes());

    let mut event_data = ANCHOR_EVENT_IX_TAG.to_vec();
    event_data.extend_from_slice(&[64, 198, 205, 232, 38, 8, 113, 226]);

    counter::event::IncrementEventArgs { counter: key(2), new_count: 7 }
        .serialize(&mut event_data)
        .unwrap();

    let inner = InstructionUpdate {
        program,
        accounts: vec![key(3), program],
        data: event_data,
        shared: Arc::clone(&shared),
        inner: vec![],
        path: Path::new_single(0).push_clone(0),
        log_range: 0..0,
    };

    let update = InstructionUpdate {
        program,
        accounts: vec![key(2), key(1), key(3), program],
        data: ix_data,
        shared,
        inner: vec![inner],
        path: Path::new_single(0),
        log_range: 0..0,
    };

    let output = counter::InstructionParser.parse(&update).await.unwrap();

    println!("{output:#?}");

    assert!(output.instruction.is_some());
    assert_eq!(output.program_events.len(), 1);
}
```

`output` is:

```
ProgramEventOutput {
    instruction: Some(
        Instructions {
            instruction: Increment {
                accounts: IncrementAccounts {
                    counter: KeyBytes(
                        "8qbHbw2BbbTHBW1sbeqakYXVKRQM8Ne7pLK7m6CVfeR",
                    ),
                    authority: KeyBytes(
                        "4vJ9JU1bJJE96FWSJKvHsmmFADCg4gpZQff4P3bkLKi",
                    ),
                    event_authority: KeyBytes(
                        "CktRuQ2mttgRGkXJtyksdKHjUdc2C4TgDzyB98oEzy8",
                    ),
                    program: KeyBytes(
                        "Cnt1111111111111111111111111111111111111111",
                    ),
                    remaining_accounts: [],
                },
                args: IncrementArgs {
                    amount: 7,
                },
            },
        },
    ),
    program_events: [
        Events {
            event: IncrementEvent {
                accounts: IncrementEventAccounts {
                    remaining_accounts: [],
                },
                args: IncrementEventArgs {
                    counter: KeyBytes(
                        "8qbHbw2BbbTHBW1sbeqakYXVKRQM8Ne7pLK7m6CVfeR",
                    ),
                    new_count: 7,
                },
            },
        },
    ],
}
```

An `emit!` event arrives as a `Program data:` log line instead, with no envelope
on it, and is read by `resolve_events_from_logs`:

```rust
let encoded = base64::engine::general_purpose::STANDARD.encode(&payload);
let logs = vec![
    "Program Cnt1111111111111111111111111111111111111111 invoke [1]".to_string(),
    format!("Program data: {encoded}"),
    "Program Cnt1111111111111111111111111111111111111111 success".to_string(),
];

let events = counter::resolve_events_from_logs(&logs);
```

`payload` here is the event discriminator followed by the Borsh body, with no
envelope tag, and the surrounding `invoke`/`success` lines matter: a
`Program data:` line is only decoded while your program is the innermost
program on the invocation stack, so a nested call from another program cannot
inject events into your stream.

## 5. Configuring the self-CPI envelope

Most Anchor programs need nothing here. `rootNodeFromAnchor` gives each event a
single `constantDiscriminatorNode` at offset 0, which declares no envelope, and
the parser falls back to Anchor's default 8-byte tag, which is what the program
actually emitted.

Configuration is for programs that wrap events differently, such as a Pinocchio
program using a one-byte envelope. Precedence at 0.10.0, verified rather than
taken from the changelog:

1. An envelope declared in the IDL, as a chain of two or more
   `constantDiscriminatorNode`s on the event. This always wins.
2. The `cpi_event_discriminator` and `cpi_event_payload_offset` macro arguments,
   which apply only when the IDL declares no envelope.
3. Anchor's default 8-byte tag, when neither is supplied.

```rust
include_shipstern_parser!(
    "idls/custom_events.json",
    cpi_event_discriminator = 0xfe,
    cpi_event_payload_offset = 1,
);
```

`cpi_event_discriminator` also accepts a string for multi-byte tags (`"fe01"`).
`cpi_event_payload_offset` defaults to the discriminator's length when omitted,
and the macro rejects an offset shorter than the discriminator, or an empty
discriminator, at the call site.

Both arguments are present and functional in 0.10.0, and no removal version is
established. Passing them alongside an IDL-declared envelope compiles, ignores
them, and warns at the call site:

```
warning: use of deprecated constant `CPI_EVENT_ARGS_IGNORED`:
         the IDL declares a CPI event envelope, so cpi_event_discriminator and
         cpi_event_payload_offset are ignored; ...
```

The note published with 0.10.0 ends "these arguments are removed in 0.10", which
was written ahead of a removal that did not happen. Do not plan a migration
around it. The wording on `main` drops the version.

Passing the arguments with an IDL that declares no envelope is the supported
fallback and warns about nothing.

Declaring the envelope in the IDL is the path to prefer. See
[`codama-parser-generation.md`](./codama-parser-generation.md#self-cpi-events-emit_cpi)
for the node shape, the agreement rules across events, and the build-time guard
that rejects an envelope tag which would mask an instruction.

## What the macro reads, and what it ignores

The macro consumes `program.instructions`, `program.accounts`,
`program.definedTypes`, and, behind `program-events`, `program.events`. Of the
rest:

- `pdas` and `docs` are read nowhere in `shipstern-proc-macro`. Hand-editing
  either cannot change the generated parser.
- `errors` is touched only on load, where a string `"code": "6000"` is coerced
  to a number so the node deserialises. It is never rendered, so the generated
  module has no error type and no code-to-name mapping.

An IDL carrying a `pdaNode` still has to be well-formed enough to deserialise,
which is why `pdaNode.seeds` is in the backfill table. Nothing downstream reads
it.

### Discriminators

Instructions and accounts both accept constant, field and size discriminators,
but they resolve a `fieldDiscriminatorNode` from different places and accept
different field types:

| | `constantDiscriminatorNode` | `fieldDiscriminatorNode` resolves against | field types accepted |
|---|---|---|---|
| instruction | bytes or unsigned number | the instruction's `arguments` | `fixedSizeTypeNode` (Anchor sighash) or `numberTypeNode` (Shank u8 index) |
| account | bytes or unsigned number | the account's own data struct fields | `fixedSizeTypeNode` with a bytes default only |

Both read only the *first* entry in `discriminators`. A chain of two or more is
meaningful on events alone, where it declares the self-CPI envelope.

Whether the discriminator is part of the decoded body differs by kind, which
matters if you are slicing bytes yourself. A byte discriminator is a prefix and
decoding starts after it. A numeric constant discriminator on an *account* is
re-read as the first field of the body, so decoding starts at offset 0; SPL
Governance is the program this exists for, and
`tests/proc-macro/tests/spl_governance.rs` spells the consequence out. On an
instruction, a numeric discriminator is skipped like a prefix. This is why the
emitted `DISCRIMINATOR` constants are documented as memcmp predicates and not
payload boundaries, and why decoding should go through `try_unpack` rather than
your own slice.

A size-discriminated instruction routes normally, on data length, but has no
discriminator bytes, so no `*_DISCRIMINATOR` constant is emitted for it. When
every instruction in a program is size-discriminated the whole
`impl Instructions` block is skipped and referencing any `*_DISCRIMINATOR` will
not compile. `tests/proc-macro/tests/size_only_instructions.rs` covers this.

> The discriminator table in
> [`codama-parser-generation.md`](./codama-parser-generation.md) marks
> `fieldDiscriminatorNode` and `sizeDiscriminatorNode` as unsupported on events.
> On 0.10.0 both are in fact accepted: events share the same discriminator
> decoding path as instructions, resolving field names against the event's data
> struct, and an event declared either way matches at runtime. Treat the two
> "no" cells as describing intent rather than behaviour, and prefer constant
> discriminators on events, which are what every IDL in `tests/idls/` uses.

## Verification

Run against the published `0.10.0` crates from crates.io, with no path
dependencies, on Rust 1.93.0, `@codama/nodes-from-anchor` 1.5.5 and Node 26.

| Claim | How it was checked |
|---|---|
| Naive conversion fails | `JSON.stringify(rootNodeFromAnchor(idl))` built, then `cargo build`: `missing field 'additionalPrograms'` |
| Nodes are frozen | `Object.isFrozen` on root, program and account nodes; assignment throws `TypeError` under ESM |
| An empty instruction needs `accounts`/`arguments` | an Anchor instruction with `"accounts": []` and `"args": []`: `missing field 'accounts'` |
| A fieldless struct needs `fields` | an Anchor event with `"fields": []`: `missing field 'fields'` |
| `Option` fields need no backfill | built with `size`, `pda`, `defaultValue`, `defaultValueStrategy` all absent |
| The backfill table is complete | script output built clean for a minimal IDL, an IDL with enums, PDAs, optional accounts, vecs, options and fixed arrays, and an IDL of empty structs |
| Four dependencies are required | removed the Borsh `derive` feature, then `shipstern-core`; both fail at expansion |
| No event types without `program-events` | `resolve_events_from_logs` is `cannot find function` |
| Output type changes with the feature | `fn assert_output<P: Parser<Output = ...>>` against both builds |
| Instruction, account and event decode | the tests in step 4, outputs transcribed from `cargo test -- --nocapture` |
| Size-discriminated instruction routes | `tests/idls/size_only_instructions.json`: parses; `ONLY_SIZED_DISCRIMINATOR` is `no associated constant` |
| Shank numeric field discriminator | a `numberTypeNode` discriminator argument routes, args start at offset 1, constant is `[3]` |
| Events accept field and size discriminators | `tests/idls/single_discriminator_event.json` re-declared each way; both match through `resolve_event_default` |
| Envelope precedence | `tests/idls/padded_cpi_event_envelope.json` with macro args warns and ignores them; `tests/idls/single_discriminator_event.json` with macro args honours a `fe` tag on both the event and CPI paths |
| Macro-arg validation | `cpi_event_discriminator = "fe01"` builds; an offset below the tag length and an empty tag are both rejected at the call site |
| `errors` is normalised on load | an Anchor IDL with `"code": "6001"` converts to a string code and still builds |
| `proto` needs `shipstern-core/proto` | builds with the feature, `cannot find PublicKeyProtoWrapper` without |

## Keeping this current

The dependency snippets here are pinned, so this file is bumped with the rest at
release time. See [Releasing](../CONTRIBUTING.md#releasing).
