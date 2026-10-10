# Parse an Anchor Program Without the Codama CLI

`include_shipstern_parser!` reads an Anchor IDL directly and converts it at compile
time, with no Node or Codama step. For dependencies, generated types and events, see
[the parser generation guide](codama-parser-generation.md).

## 1. Check the IDL version

IDLs written by Anchor 0.30 or later have `metadata.spec` set to `"0.1.0"` and work
as they are:

```bash
jq .metadata.spec idl.json
# "0.1.0"
```

If this prints `null`, the IDL is from an older Anchor; see
[Older Anchor IDLs](#older-anchor-idls).

## 2. Get the IDL

From your own program, `anchor build` writes it to `target/idl/<program>.json`.

For a deployed program, fetch the IDL its authority published on chain:

```bash
anchor idl fetch --provider.cluster mainnet -o idl.json <PROGRAM_ID>
```

With Anchor 1.x this reads only the Program Metadata account. If it fails with
`Account not found`, the IDL is in the older IDL account:

```bash
anchor legacy-idl fetch --provider.cluster mainnet -o idl.json <PROGRAM_ID>
```

The on-chain IDL is whatever the authority last uploaded. It can lag behind the
deployed program, so decode a few recent transactions before relying on it (step 5).

## 3. Generate the parser

Add the dependencies from [the parser generation guide](codama-parser-generation.md#quick-start),
then point the macro at the IDL. The path is relative to your crate root.

```rust
use shipstern_proc_macro::include_shipstern_parser;

include_shipstern_parser!("idls/my_program.json");
```

The generated module is named after `metadata.name` in snake_case, so an IDL named
`lending` gives `lending::InstructionParser`, `lending::AccountParser` and so on.

To decode `emit!` and `emit_cpi!` events, turn on the `program-events` feature of
`shipstern-proc-macro`.

## 4. Fix compile errors

A problem in the IDL fails the build with a message naming the item. For example:

```text
Failed to load/parse IDL from "/path/to/my_crate/idls/my_program.json": Failed to convert Anchor IDL: instruction `swap`: discriminator is empty
```

| Message contains | Cause | Fix |
|---|---|---|
| `unsupported Anchor IDL spec` | The file is not Anchor 0.1.0 or a legacy Anchor IDL | Regenerate it or pass Codama JSON |
| `no program id in` | A legacy IDL has no program id | Add `metadata.address` for an Anchor program |
| `legacy IDL is not from Anchor` | The IDL is from Shank, Steel, or another format | Convert it with `codama convert` and pass Codama JSON |
| `cannot upgrade legacy Anchor IDL` or `in a legacy IDL is not supported` | Anchor cannot read the named item | Fix the named item |
| `invalid Anchor IDL JSON` or `unrecognized Anchor IDL type` | A field has the wrong JSON shape | Fix the named item |
| `no type definition` or `type is not a struct` | An account or event has no struct of its name in `types` | Add the struct to `types` |
| `Rust identifier`, `same Rust name`, or `duplicate arguments` | A name is invalid or duplicates another Rust name | Rename the item |
| `clashes with the instruction discriminator` | An argument is named `discriminator` | Rename the argument; argument names are not in the instruction data |
| `discriminator` | A discriminator is empty, duplicated, or a prefix of another | Use the deployed program's discriminator bytes |
| `is not defined`, `used without arguments`, or `is not bound` | A type link or generic argument is invalid | Fix the named type reference |
| `nesting`, `expansion`, `fixed-size type`, or `invalid array length` | A type is too deep or too large | Simplify the named type |
| `enum` or `coption` | The generated parser cannot render the type | Move or replace the named type |
| `Borsh also reads it` | Borsh and zero-copy need different padding | Use separate IDL types |
| `layout cannot be computed` | The IDL cannot prove the zero-copy layout | Pass Codama JSON whose layout you checked against live accounts |

The [converter README](../crates/codama-from-anchor/README.md#known-divergences) lists every rejected shape.

## 5. Check the parser against real data

Decode a few transactions you already know, from an explorer or from Anchor's own
TypeScript client, and compare the values. `tests/proc-macro/tests/anchor_idl_input.rs`
in this repository shows the shape of such a test, with hand-built instruction bytes.

## Limits

- **Accounts are read by position.** An instruction that passes fewer accounts than
  the IDL declares fails with `Account does not exist at index N`, unless each missing
  required account has a fixed address: an IDL `address`, or a program or sysvar name
  such as `tokenProgram`. The parser fills those in, except for instructions that
  share a discriminator, which it tells apart by account count first. A
  Codama-converted parser behaves the same.
- **A filled account is an IDL default, not transaction data.** The parser does not
  fill a list that looks shifted: a carried account in a fixed-address slot differs
  from that address, or a carried account already holds an address the fill would add.
  The `tokenProgram` name rule fills the legacy Token program, also for programs that
  accept Token-2022.
- **The Codama output is not identical to `codama convert`.** Account sizes, PDAs in
  `program.pdas`, and payer, identity and program ID defaults are left out, because
  the parser does not read them. A few IDLs get a different parser, such as one with
  implicit `repr(C)` padding that `codama convert` misreads. The full list is in
  [the converter's README](../crates/codama-from-anchor/README.md#known-divergences).
- **A `repr(Rust)` layout has no guaranteed field order.** The IDL has no field
  offsets, so the converter rejects this layout.
- **A packed layout loses its pack value.** Anchor records `packed(N)` as a Boolean
  value. The converter reads `repr(C, packed)` as `packed(1)`, so `N` above 1 is misread.
- **An enum in a zero-copy account has no known size.** Anchor does not record its
  `repr`, so the converter rejects it.

## Older Anchor IDLs

IDLs from Anchor 0.29 and earlier have no `metadata.spec`. The macro upgrades them with
Anchor's own converter, the one `anchor idl convert` runs, and then converts the result
like any other IDL. Four things to check first:

- **The program id.** The upgrade needs it in `metadata.address`, and older IDLs often
  leave it out. Add it.
- **An Anchor program.** The upgrade gives instructions, accounts and events Anchor's
  sha256 discriminators. A native program written in this format, such as the `spl-*`
  IDLs in Anchor's TypeScript packages, gets discriminators its data never carries, and
  the parser matches nothing, with no error. `codama convert` writes the same
  discriminators, so such a program needs Codama JSON with its real instruction tags.
  For SPL Token, Token-2022 and Stake Pool, use `shipstern-spl-token-parser`,
  `shipstern-spl-token-extensions-parser` and `shipstern-stake-pool-parser` instead.
  An IDL marked as Shank or Steel in `metadata.origin`, or with an instruction
  `discriminant`, is refused. Convert it with `codama convert`, which keeps the
  instruction discriminators but gives accounts none and drops events. Add each
  account's `discriminators` to that output by hand. `codama-nodes` 0.13.2 cannot load
  the `steel` origin that a Steel IDL gets, so remove `program.origin` from it.
- **Instruction names like `buy_2`.** Anchor 0.29 wrote that name as `buy2`, so the
  upgrade hashes the wrong name and the parser never matches that instruction. Fix it
  by hand, as below.
- **Padded `zero_copy` accounts.** An older IDL does not record which types are
  `zero_copy`, so every account is read as Borsh. For a `zero_copy(unsafe)` account with
  `#[repr(C)]` and implicit padding, that misreads every field after a gap, with no
  error. Fix it by hand, as below.

### Fixing an older IDL by hand

Some fixes have no place in the legacy format. Upgrade the IDL with the Anchor CLI, edit
the result, and pass that file to the macro:

```bash
anchor idl convert -p <PROGRAM_ID> -o idl.json old_idl.json
```

- **`buy_2`:** set the instruction's `discriminator` to the hash of its Rust name, from
  `python3 -c 'import hashlib; print(list(hashlib.sha256(b"global:buy_2").digest()[:8]))'`.
- **Padding:** add `"serialization": "bytemuckunsafe"` and `"repr": {"kind": "c"}` to the
  account's entry in `types`. The macro then reads the padding.
- **`COption`:** a legacy `{"defined": "COption<Pubkey>"}` fails with an `is not defined`
  error. Replace each `{"defined": {"name": "COption<Pubkey>"}}` in the result with
  `{"coption": "pubkey"}`.
- **camelCase names:** the CLI hashes account and event names as written, unlike the
  macro. If the old IDL spells one in camelCase, such as `priceUpdateV2` for a
  `PriceUpdateV2` account, set its `discriminator` from `account:PriceUpdateV2` the same
  way.

Codama JSON skips the checks in step 4, so an IDL that would fail them there can
compile into a parser that misreads data.
