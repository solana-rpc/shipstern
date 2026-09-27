# Generate a Shipstern Parser from an Anchor or Codama IDL

Shipstern generates a parser from an Anchor IDL or a [Codama](https://github.com/codama-idl/codama)
IDL at compile time, through the `include_shipstern_parser!` proc macro. Nothing is written
to disk and no build script is involved.

```
Anchor IDL (spec 0.1.0) ─┐
                         ├──> include_shipstern_parser! ──> generated parser
Codama JSON ─────────────┘       (compile time)        (accounts, instructions, events)
```

## Quick start

**1. Point the macro at your IDL.** An Anchor IDL from Anchor 0.30 or later, where
`metadata.spec` is `"0.1.0"`, works as it is: the macro converts it in-process with
`shipstern-codama-from-anchor`, which produces the same parser as
`@codama/nodes-from-anchor@1.5.6`. No Node and no intermediate file.

An older Anchor IDL has no `metadata.spec` and fails to compile with a message saying so.
Convert it to Codama JSON first and pass that instead:

```bash
npx -p codama -p @codama/nodes-from-anchor codama convert idl.json codama.json
```

Both `-p` flags matter. `codama convert` reads Anchor through
`@codama/nodes-from-anchor`, and it only installs that dependency for you when it runs
inside a directory that already has a `package.json`. A Rust crate has none, so the bare
`npx codama convert` fails there with `Missing dependency: @codama/nodes-from-anchor`.
Naming both packages puts them in one temporary install and the command works from any
directory, with no Node project to set up. Append `@<version>` to either package to pin
it.

That output loads as it is. Codama's JavaScript omits empty and default-valued
collections when it serializes, and `codama-nodes` (pinned to `=0.13.2`) defaults
them on the way back in, so there is nothing to add by hand.

One constraint to know about: if `programNode.origin` is present it has to be
`"anchor"` or `"shank"`, which is the closed set Codama models. Any other value is
rejected when the IDL loads. Leaving the field out is fine.

If your IDL is already in complete Codama form, pass it directly. `tests/idls/*.json`
in this repository are all in that shape and make a useful reference.

**2. Add the dependencies.**

```toml
[dependencies]
borsh = { version = "^1.0.0", features = ["derive"] }
shipstern-core = { version = "0.11.0" }
shipstern-parser = { version = "0.11.0" }
shipstern-proc-macro = { version = "0.11.0" }
```

`shipstern-core` and the Borsh `derive` feature are both required: the generated code
imports `shipstern_core` and derives `BorshDeserialize` / `BorshSerialize` on every type.

To parse events, including self-CPI events, add the `program-events` feature:

```toml
shipstern-proc-macro = { version = "0.11.0", features = ["program-events"] }
```

**3. Invoke the macro.** The path is resolved relative to your crate root
(`CARGO_MANIFEST_DIR`).

```rust
use shipstern_proc_macro::include_shipstern_parser;

include_shipstern_parser!("path/to/codama.json");
```

**4. Use the generated module.** It is named after the program, in snake_case, and
contains `PROGRAM_ID`, `InstructionParser`, `AccountParser`, and the argument and account
types.

```rust
let parsed = my_program::InstructionParser.parse(&update).await?;
```

## What Shipstern reads from the IDL

The macro consumes a subset of the Codama tree. Anything not listed here is ignored, so
changing it will not affect the generated parser.

| Codama node | Becomes |
|---|---|
| `program.name` | the generated module name, in snake_case |
| `program.publicKey` | `PROGRAM_ID` |
| `program.instructions[].discriminators` | the instruction dispatch key |
| `program.instructions[].arguments` | the `*Args` struct |
| `program.instructions[].accounts` | the `*Accounts` struct |
| `program.accounts[]` | account types and `AccountParser` |
| `program.definedTypes[]` | shared types, referenced through `definedTypeLinkNode` |
| `events[]` | event types, **only** with the `program-events` feature |

Ignored: `pdas` and `docs` are not read at all. `errors` is only normalised on load, where a
string `"code": "6000"` is coerced to a number; it is never rendered into the parser.

### Discriminators

Instructions and accounts do not accept the same discriminator kinds.

| Declared on | `constantDiscriminatorNode` | `fieldDiscriminatorNode` | `sizeDiscriminatorNode` |
|---|---|---|---|
| instruction | yes, bytes or number value | yes | yes, for dispatch only |
| account | yes | yes | yes |
| event | yes, and a chain of two or more declares the CPI envelope | no | no |

A size-discriminated instruction routes on data length like any other, but carries no
discriminator bytes, so no `*_DISCRIMINATOR` constant is emitted for it. When *every*
instruction in a program is size-discriminated the whole `impl Instructions` block is
skipped, and referencing a `*_DISCRIMINATOR` on it will not compile. See
`tests/proc-macro/tests/size_only_instructions.rs`.

Where several instructions share a discriminator, the generated parser disambiguates by
account count, trying the variant with the most accounts first. When variants share both
the discriminator and the account count, supply a `CustomInstructionParser`; see the
[README](../README.md#handling-discriminator-collisions).

## The `program-events` feature

Event parsing is feature-gated. Without `program-events`, the macro emits no event types
and no self-CPI handling at all, however the IDL declares its events.

Enabling it also **changes the parser's output type**: `InstructionParser::Output` becomes
`ProgramEventOutput`, which carries the decoded instruction alongside any events found on
the transaction's inner instructions, instead of `Instructions` alone.

```rust
// with program-events
ProgramEventOutput {
    instruction: Some(Instructions { .. }),
    program_events: [Events { .. }],
}
```

Because this changes a public type, enabling the feature anywhere in a workspace affects
every crate that shares the dependency through Cargo's feature unification. Keep crates
that need different settings in separate workspaces.

## Self-CPI events (`emit_cpi!`)

`emit_cpi!` does not write a log line. It invokes the program from itself, with
instruction data shaped as:

```
[envelope tag][event discriminator][borsh payload]
```

Codama has no node for that tag, so Shipstern infers it positionally from the
event's discriminator chain, the same rule the Carbon renderer uses. Declare two
or more `constantDiscriminatorNode`s on the event:

```json
"discriminators": [
  { "kind": "constantDiscriminatorNode", "offset": 0,
    "constant": { "kind": "constantValueNode",
      "type": { "kind": "fixedSizeTypeNode", "size": 8, "type": { "kind": "bytesTypeNode" } },
      "value": { "kind": "bytesValueNode", "data": "e445a52e51cb9a1d", "encoding": "base16" } } },
  { "kind": "constantDiscriminatorNode", "offset": 8,
    "constant": { "kind": "constantValueNode",
      "type": { "kind": "fixedSizeTypeNode", "size": 8, "type": { "kind": "bytesTypeNode" } },
      "value": { "kind": "bytesValueNode", "data": "40c6cde8260871e2", "encoding": "base16" } } }
]
```

The rules:

- The constant at **offset 0** is the envelope tag.
- Every remaining constant forms the event's own discriminator.
- `payload_offset` is `sorted[1].offset`, the declared offset of the next
  discriminator, not the tag's length. Padded layouts therefore round-trip, and
  discriminators need not be contiguous.
- One program has one envelope. Events that declare one must agree on both the
  tag bytes and the payload offset, or the build fails.
- An event with a single discriminator declares no envelope. It keeps matching
  at offset 0 and stays reachable from `emit!` log lines.
- When no event declares an envelope and no macro argument supplies one, Anchor's
  default 8-byte tag is used.

`e445a52e51cb9a1d` is the little-endian serialisation of
`sha256("anchor:event")[..8]`, whose natural byte order is `1d9acb512ea545e4`.
Write the wire bytes, not the digest prefix.

### Setting the envelope through Codama

The discriminator chain is the whole configuration surface. A padded envelope, taken from
`tests/idls/padded_cpi_event_envelope.json`, declares a one-byte tag at offset 0 and the
event's own two-byte discriminator at offset 4:

```json
"discriminators": [
  { "kind": "constantDiscriminatorNode", "offset": 0,
    "constant": { "kind": "constantValueNode",
      "type": { "kind": "fixedSizeTypeNode", "size": 1, "type": { "kind": "bytesTypeNode" } },
      "value": { "kind": "bytesValueNode", "data": "fe", "encoding": "base16" } } },
  { "kind": "constantDiscriminatorNode", "offset": 4,
    "constant": { "kind": "constantValueNode",
      "type": { "kind": "fixedSizeTypeNode", "size": 2, "type": { "kind": "bytesTypeNode" } },
      "value": { "kind": "bytesValueNode", "data": "a1a2", "encoding": "base16" } } }
]
```

That gives a `payload_offset` of 4, so the wire layout is:

```
offset   0    1              4        6
        | fe | 00 00 00     | a1 a2 | borsh payload |
        | tag| three pad    | disc  |               |
```

`tests/proc-macro-events/tests/padded_cpi_event_envelope.rs` covers both directions: the
CPI path strips the declared offset of 4 rather than a fixed 8, and the log path still
matches the same event once its discriminators are rebased.

### The envelope must not mask an instruction

The generated parser filters any instruction whose data starts with the envelope
tag before it dispatches, so an instruction whose discriminator shares a prefix
with the tag at offset 0 could never be parsed. That is rejected at build time:

    error: Invalid CPI event envelope in "...": CPI event envelope 09 collides
    with instruction `swapBig` (discriminator 09 at offset 0); the generated
    parser filters every instruction whose data starts with the envelope tag, so
    `swapBig` would never parse

The check runs against the tag the parser actually uses, so it covers an IDL
envelope, the `cpi_event_discriminator` fallback, and the Anchor default alike.
It compares the tag only against instructions. Instruction variants that
deliberately share a discriminator with each other, resolved at runtime by
account count or a `CustomInstructionParser`, are unaffected.

Note this differs from Anchor, whose dispatcher checks the event-CPI sentinel
after every instruction rather than before.

### Deliberate divergences from Carbon

Both follow from Shipstern supporting `emit!` log events, which Carbon's
generated decoders do not.

1. **Mixed programs are accepted.** Carbon bails on the whole program if any
   single event lacks the two-discriminator form; Shipstern accepts a program
   that mixes `emit_cpi!` and `emit!` events. An IDL Shipstern accepts may
   therefore render nothing in Carbon.
2. **A lone two-part discriminator reads as enveloped.** The heuristic is
   positional and no marker node exists, so an event with a genuine two-part
   discriminator starting at offset 0 is indistinguishable from
   envelope-plus-discriminator, and is treated as the latter.

### Legacy macro arguments

`cpi_event_discriminator` and `cpi_event_payload_offset` on
`include_shipstern_parser!` remain a fallback for IDLs that declare no envelope:

```rust
include_shipstern_parser!(
    "path/to/codama.json",
    cpi_event_discriminator = 0xfe,
    cpi_event_payload_offset = 1,
);
```

Precedence is:

1. an envelope declared in the IDL, which wins whenever present
2. these arguments, which apply only when the IDL declares none
3. Anchor's default 8-byte tag, when neither is supplied

When the IDL declares an envelope the arguments are ignored and a deprecation warning
fires at the call site. No removal version is currently established. Prefer declaring the
envelope in the IDL, which is the supported path.

## Generating a standalone parser crate

There is also a Codama renderer that writes a complete parser crate to disk, rather than
expanding a macro in your own crate. It is a separate tool with its own workflow.

> **Note:** the renderer has not been republished under the Shipstern name. The package
> `@codama/renderers-shipstern-parser` referenced by earlier versions of this guide does
> not exist on npm. The last published release is `@codama/renderers-vixen-parser` 1.2.8
> (October 2025). Running it against a small Anchor IDL on the current toolchain emitted
> only the `generated_sdk/` half and exited non-zero, without producing the
> `generated_parser/` or `proto/` output this workflow depends on. Whether that is
> specific to that IDL was not investigated further. Use the proc macro above.
