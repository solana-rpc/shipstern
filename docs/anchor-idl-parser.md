# Parse an Anchor Program Without the Codama CLI

`include_shipstern_parser!` reads an Anchor IDL directly and converts it at compile
time, with no Node or Codama step. For dependencies, generated types and events, see
[the parser generation guide](codama-parser-generation.md).

## 1. Check the IDL version

Only IDLs written by Anchor 0.30 or later work. They have `metadata.spec` set to
`"0.1.0"`:

```bash
jq .metadata.spec idl.json
# "0.1.0"
```

If this prints `null`, the IDL is from an older Anchor. See
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
| `unsupported Anchor IDL spec` | IDL from before Anchor 0.30 | See [Older Anchor IDLs](#older-anchor-idls) |
| `missing field` and `address` | The IDL has no program address | Add the program id as the top-level `address` |
| `unrecognized Anchor IDL type` | A type the converter does not know, such as `u256` | Replace it in the IDL; `codama convert` rejects it too |
| `does not convert to a Rust identifier` | A name that is empty or starts with a digit | Rename it in the IDL |
| `same Rust name as` | Two types, instructions, accounts or events whose names differ only in case or underscores, such as `MyType` and `my_type` | Rename one of them in the IDL |
| `discriminator is empty` | `"discriminator": []` on an instruction, account or event | Use the real discriminator bytes |
| `is a prefix of` | Two instructions, accounts or events where one discriminator starts the other and is shorter | Use the real discriminators; `anchor build` rejects these |
| `same discriminator as` | Two accounts with the same discriminator | Use the real discriminators; `anchor build` rejects these |
| `no type definition` | An account or event with no entry in `types` | Add the type, or regenerate the IDL with `anchor build` |
| `type is not a struct` | An account or event whose entry in `types` is an enum, alias or tuple struct | Name a tuple struct's fields in the IDL, which reads the same bytes; enums are not supported |
| `invalid Anchor IDL JSON` | A field has the wrong JSON shape, such as a discriminator byte above 255 | Fix that item in the IDL |
| `produces duplicate arguments` | A struct argument has a field with the same name as another argument | Rename one of them |
| `clashes with the instruction discriminator` | An argument named `discriminator` | Rename it in the IDL |
| `is not defined` | A `defined` type with no entry in `types` | Add the type, or fix the name |
| `used without arguments` | A generic type referenced without its `generics` | Pass its arguments in the IDL |
| `generic enum` | An enum with `generics`, which would be expanded inline | Not supported yet; with `codama convert` output the macro panics instead |
| `type nesting exceeds 128 levels` | A type nested more than 128 levels deep | Flatten the type in the IDL |
| `type expansion exceeds` | Generic types nested so their expansion doubles at every level | Simplify the nesting in the IDL |
| `Borsh also reads it` | A `zero_copy(unsafe)` `repr(C)` type with implicit padding that is also an instruction argument, an event, or a Borsh account field | Split it into two types in the IDL, one per use |
| `layout cannot be computed` | A `zero_copy(unsafe)` `repr(C)` struct holding an option, a generic, a tuple struct or an enum with data, at any depth | Not supported: its padding cannot be placed, and `codama convert` output misreads the fields after a gap |

## 5. Check the parser against real data

Decode a few transactions you already know, from an explorer or from Anchor's own
TypeScript client, and compare the values. `tests/proc-macro/tests/anchor_idl_input.rs`
in this repository shows the shape of such a test, with hand-built instruction bytes.

## Limits

- **Accounts are read by position.** An instruction that passes fewer accounts than
  the IDL declares fails to parse with `Account does not exist at index N`. A
  Codama-converted parser has the same limit.
- **The Codama output is not identical to `codama convert`.** Account sizes, PDAs in
  `program.pdas` and client-side account defaults are left out, because the parser
  does not read them. The generated parser is the same. The full list is in
  [the converter's README](../crates/codama-from-anchor/README.md#known-divergences).

## Older Anchor IDLs

IDLs from Anchor 0.29 and earlier have no `metadata.spec`, and the macro rejects them.
Upgrade one with the Anchor CLI and pass the result to the macro. Add
`-p <PROGRAM_ID>` if the IDL has no address:

```bash
anchor idl convert -o idl.json old_idl.json
```

If the upgraded IDL still fails, for example on a `COption<...>` type, convert the
old IDL to Codama JSON instead and pass that file:

```bash
npx -p codama -p @codama/nodes-from-anchor codama convert old_idl.json codama.json
```
