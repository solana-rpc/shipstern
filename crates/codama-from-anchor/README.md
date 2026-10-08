# shipstern-codama-from-anchor

Converts an Anchor IDL into [`codama-nodes`](https://crates.io/crates/codama-nodes), so a Shipstern parser can be generated from an Anchor program without running `codama convert` first.

It is internal to Shipstern for now: the API may change, and it may move into codama-rs later.

```rust
let idl = serde_json::from_slice(&std::fs::read("idls/my_program.json")?)?;
let root = shipstern_codama_from_anchor::root_node_from_anchor(idl)?;
```

## Scope

Anchor IDL spec `0.1.0`, which Anchor 0.30 and later emit, is converted directly. A legacy IDL from an older Anchor (top-level `name` and `instructions`, no top-level `address` or `metadata.spec`) is first upgraded to spec `0.1.0` with Anchor's own converter, `anchor-lang-idl`; see [Legacy IDLs](#legacy-idls). Any other input fails with `Error::UnsupportedSpec`.

## Compared with `@codama/nodes-from-anchor`

The reference is `@codama/nodes-from-anchor@1.5.6` with `codama@1.11.0` and `@codama/visitors@1.11.0`, the code `codama convert` runs. Two tests hold this crate to it:

- `shipstern-proc-macro`'s `anchor_parity_tests` expands a parser from the JS output and from this crate's output for every supported fixture and requires the two to be byte-identical. It also checks that an unverified layout fails.
- `tests/js_parity.rs` diffs the full Codama JSON against the JS output, after undoing extractPdas, setFixedAccountSizes and setInstructionAccountDefaultValues.

### Feature comparison

This crate targets Shipstern parser generation. It does not replace every client feature in `codama convert`.

| Feature | `codama convert` in JS | This Rust crate |
|---|---|---|
| Raw spec `0.1.0` Anchor IDL | Converts it before the Rust build | Converts it inside the Rust build |
| Legacy Anchor IDL | Uses its v00 converter | Uses Anchor's upgrade, then the v01 converter |
| Legacy Shank or Steel IDL | Converts some inputs | Rejects them; pass the JS output to Shipstern |
| Build requirement | Requires Node and an intermediate Codama JSON file | Requires no Node or intermediate file |
| Output | Writes Codama JSON for clients and generators | Builds an internal `RootNode` for Shipstern |
| Parser passes | Runs all seven default passes | Ports the three that change the parser and emulates the needed PDA effect |
| `repr(C)` zero-copy padding | Reads fields as Borsh and can misread data after a gap | Inserts the implicit padding |
| Invalid parser shapes | Can write output that misreads, fails to compile or panics later | Rejects them with an error that names the item |
| `repr(Rust)` field order | Uses the declared IDL order without proof | Rejects it because the IDL has no field offsets |
| `account.size` | Sets it | Omits it; the parser does not read it |
| `program.pdas` | Extracts PDAs and resolves name conflicts | Keeps PDA defaults inline; `program.pdas` stays empty |
| Client account defaults | Adds payer, identity, program and sysvar defaults | Keeps only an address present in the IDL |

### Passes

`codama convert` runs the mapping and then the seven passes in `defaultVisitor.ts`. The three that change the generated parser are ported, and extractPdas is emulated as the table shows. The other three were removed one at a time from the JS pipeline, and the parser came out byte-identical on every supported fixture, with and without the `proto` and `program-events` features. Removing any of the three ported passes does change it, so the comparison can tell the difference.

| Pass | Ported | Effect on the generated parser |
|---|---|---|
| extractPdas | no, emulated | Its seed types count toward unwrapInstructionArgsDefinedTypes, so `passes.rs` counts them where they stay inline |
| deduplicateIdenticalDefinedTypes | no | None on every fixture; colliding names are rejected instead |
| setFixedAccountSizes | no | None: sets `account.size`, which no renderer reads |
| setInstructionAccountDefaultValues | no | None: fills payer, identity, program and sysvar addresses for client code |
| unwrapInstructionArgsDefinedTypes | yes | Changes `shapes`: a struct used only as one argument is inlined |
| flattenInstructionDataArguments | yes | Changes `anchor_external` and `shapes` |
| transformU8ArraysToBytes | yes | Changes `anchor_external` and `shapes` |

## Known divergences

### Inputs where this crate gives a different parser

| Input | JS 1.5.6 | This crate |
|---|---|---|
| `zero_copy(unsafe)` `repr(C)` struct with implicit padding | Reads it as Borsh and misreads every field after a gap: 43 of 271 mainnet `voltr_vault` `AdaptorAddReceipt` accounts | Inserts a `[u8; N]` field, `padding_before_<field>` or `padding_end`, at each gap, with a `_2` suffix if a field already has that name; also in every struct nested in a `zero_copy(unsafe)` account and in a `zero_copy(unsafe)` struct inside a `bytemuck` account |
| Generic argument whose name the callee reuses | Resolves it in the callee's scope: `Outer<A, B>` passing `Wrapper<B, u8>` gives `Wrapper`'s `A` the wrong type; on `gmsol_store` and Anchor's `tests/idl/idls/idl.json` it recurses until the stack overflows | Resolves it where it is written |
| Const parameter passed on to another generic as a type argument, as Anchor writes it | Writes no count | Keeps the value |
| Anchor type alias, `{ "kind": "type", "alias": T }` (Raydium CLMM's `TickArrayBitmap`) | Rejects it; reads only `{ "kind": "alias", "value": T }` | Converts it as `T`, like Anchor's `cli/src/codama.rs` and Codama 2.x (codama-idl/codama#1181, not yet on npm) |
| PDA seed naming an argument the instruction lacks, or a seed kind other than `const`, `arg` or `account` | Fails the whole conversion | Drops that PDA; the parser never reads PDAs |
| Instruction with no `args` | Fails | Converts it with none |

### Inputs this crate rejects, naming the item

JS writes output for each of these that fails to compile, panics in the renderer, or misreads data.

| Input | JS 1.5.6 | Error |
|---|---|---|
| `zero_copy(unsafe)` account with `repr(Rust)`, no `repr`, `packed`, an option, a vec, a string, a generic, a tuple struct, or an enum, at any depth | Reads it as Borsh | `UnknownLayout` |
| Type with implicit padding in a `zero_copy(unsafe)` account that Borsh also reads (an instruction argument, an event, or a Borsh account field) | Misreads one of the two | `PaddedBorshType` |
| Two types, instructions, accounts or events of one kind whose names camel-case alike, such as `MyType` and `my_type` | Writes both, which do not compile; deletes every copy of identical types and leaves dangling links | `NameCollision` |
| Instruction, account or event discriminator that is a proper prefix of another of its kind | Writes it; the parser can read one as the other | `AmbiguousDiscriminator` |
| Two accounts with the same discriminator | Writes them; the parser always reads the first | `DuplicateDiscriminator` |
| Event whose type is an enum, an alias or a tuple struct | Writes it; the `program-events` parser panics on it | `TypeNotStruct` |
| Generic enum, such as `arcium`'s `SetUnset` | Expands it inline; the renderer panics on an inline enum | `GenericEnum` |
| Any other enum that is not a type's whole definition, such as one written in an argument or bound to a generic | Writes it; the renderer panics | `InlineEnum` |
| `coption` of a type with no fixed size, such as a string | Writes it; the renderer panics | `VariableSizeCOption` |
| Array count above 10 MiB, or a known fixed type above 10 MiB | Writes it; the parser can allocate too much before reading | `InvalidArrayLength`, `FixedTypeTooLarge` |
| Link to an undefined type, a generic type used without arguments, or an unbound generic name | Writes the link, which fails in rustc | `UndefinedType`, `GenericArgsMissing`, `GenericArgMissing` |
| Empty discriminator | Writes it; it matches everything | `EmptyDiscriminator` |
| Name that camel-cases to an empty or digit-leading string, such as `__` or `1st` (program, instruction, instruction account, argument, account, event, type, field or variant) | Writes it | `InvalidName` |
| Nameless enum variant | Writes an empty name | `UnrecognizedType` |
| IDL with no `address` | Leaves out `publicKey`, so Shipstern cannot set `PROGRAM_ID` | `Json` |
| Error with no `code` | Writes `-1`, which `codama-nodes` cannot load | `Json` |
| Discriminator or `const` seed byte above 255 | Wraps it modulo 256, so `256` becomes `00` | `Json` |
| Type nesting deeper than 128 levels, a generic argument nested that deep, or an alias chain that long or cyclic | Accepts it up to its stack depth; the renderer overflows its stack or panics on the aliases | `RecursionLimit` |
| Generics nested so that each level doubles the expansion, past 100,000 type nodes | No bound | `TooLarge` |

Equal instruction discriminators are kept: the parser tells them apart by account count and fails at parse time on two with the same count. Equal event discriminators already fail to compile in the `program-events` parser. Anchor's IDL build rejects every discriminator case above.

Two inputs fail in both converters, and only the error differs. An argument named `discriminator` clashes with the discriminator argument both prepend, and flattening struct arguments can leave two arguments with one name. JS's flatten pass throws `Cannot flatten struct ... [name]` for both, without naming the instruction. This crate fails with `DiscriminatorArgument` or `ConflictingFlattenedArguments` at that instruction.

### Differences that do not reach the parser

| Codama JSON field | JS 1.5.6 | This crate |
|---|---|---|
| `account.size` | Set by setFixedAccountSizes | `None` |
| `program.pdas` | PDAs hoisted there, colliding names renamed | Empty; PDAs stay inline in account defaults |
| Instruction account defaults | Payer, identity, program and sysvar addresses filled in | Only an `address` the IDL gives |
| Integer constants above 2^53 | Rounded to the nearest double | Exact; past `u64` or `i64` they become floats |
| Missing `metadata.version`, or a missing error or constant `name` | `0.0.0` or an empty name | Fails to deserialize |
| `null` for a top-level list, an instruction's `accounts` or an account flag, or docs that are not strings | An empty list, `false`, or the value as is | Fails to deserialize |

### Notes

- The padding layout is computed from numbers, `bool`, pubkeys, arrays, structs, and type aliases. Enums fail because the IDL does not prove their memory size.
- A struct with no `repr` uses `repr(Rust)`. Rust does not guarantee its field order. The converter rejects both forms.
- Anchor writes `repr(packed(N))` as plain `packed`. The converter rejects all packed layouts because it cannot recover `N`.
- `bytemuck` types need no added padding in their own fields. A `zero_copy(unsafe)` struct inside one still needs a verified layout.
- The deepest IDL among 105 mainnet programs nests 11 levels by this crate's count. Through the macro, plain deep nesting hits serde_json's parser limit before the 128-level one.
- Read by neither converter: instruction `returns`, account `relations`, a seed's `account` hint, and metadata beyond name, version and spec. A legacy IDL's `metadata.address` and `metadata.origin` are read too.

## Legacy IDLs

JS reads a legacy IDL with separate v00 functions. This crate runs Anchor's own upgrade instead (`anchor_lang_idl::convert::convert_idl`, the code behind `anchor idl convert`) and converts the result like any spec `0.1.0` IDL. Around the upgrade, `upgrade_legacy`:

- refuses the inputs in the table below;
- removes instruction-account `pda` entries, since the parser never reads PDAs and the upgrade fails on a const seed that is not a string or bytes;
- types a constant written as `{"defined": "usize"}`, which is how Anchor 0.29 wrote `usize` constants, as `u64`;
- names and hashes instructions with JS's snake case. The upgrade uses heck 0.3, which turns `setAB`, Anchor 0.29's name for Rust `set_a_b`, into `set_ab`;
- hashes account and event discriminators from the name with its first letter uppercased, since Anchor hashes the Rust type name. The upgrade hashes the name as written, so the account Pyth's receiver IDL calls `priceUpdateV2` would get a discriminator that no `PriceUpdateV2` account has. JS pascal-cases the whole name, which also drops underscores, so a `Pool_State` account converted by JS matches nothing.

| Input | JS 1.5.6 | This crate |
|---|---|---|
| `metadata.origin` other than `anchor`, such as Shank or Steel, or an instruction, account or event with its own `discriminant` or `discriminator` | Uses an instruction's `discriminant`, or the instruction index for Shank; ignores a `discriminator` and hashes the name | `NotAnchor`; convert it with `codama convert`, and for Steel remove `program.origin` from the result, which `codama-nodes` 0.13.2 cannot load |
| No `metadata.address` | Writes an empty `publicKey`; the macro then fails with `Invalid pubkey length` | `MissingAddress` |
| A `state` section (Anchor before 0.26) | Drops it | `LegacyUnsupported` |
| A generic type or account | Fails: `generic` is not a v00 type | `LegacyUnsupported`, at that type or account |
| A type in `types` with an account's or event's name but other fields | Reads the account or event with its own fields | `LegacyUnsupported`, at that account or event; the upgrade would read the type's fields instead |
| An item Anchor's converter cannot read, such as an account with no `isMut` | Fills in a default (`isMut` becomes `false`) | `Legacy`, at that item |

Neither converter can tell a native program written in this format from an Anchor one. Both give it Anchor's sha256 discriminators, which its data never carries, so the parser matches nothing. 13 of the 14 legacy IDLs in Anchor's repository are of this kind: the `spl-*` IDLs in its TypeScript packages. Such a program needs Codama JSON with its real instruction tags.

The legacy format has no `zero_copy` marker either, so both converters read every account as Borsh.

Anchor 0.29 wrote instruction names with heck 0.3's mixed case, which drops an underscore before a digit: `buy_2` becomes `buy2`. Both converters then hash `global:buy2`, not the program's `global:buy_2`, so the parser never matches that instruction.

Differences that do not reach the parser:

| Codama JSON field | JS 1.5.6 | This crate |
|---|---|---|
| Account docs | Taken from the account's entry | Empty; the upgrade keeps them only on the type |
| Docs of an error with no message | `Name` | `Name: `, as JS writes for spec `0.1.0` |
| A `usize` constant | A link to the undefined type `usize` | `u64`, or a string when the value is an expression |
| A type in `types` with an event's name, as LayerZero's `uln`, `dvn`, `endpoint` and `executor` have | Kept as a defined type nothing links to | Left out; both carry the event's fields inline |

Checked against the 64 legacy IDLs among 170 program IDLs fetched from mainnet on 2026-10-03, with live data sampled on 2026-10-04. 4 are Shank or Steel IDLs and are refused. 4 fail with an error naming the item: JS's output for 3 of them does not compile, and JS fails on the fourth too. The other 56 convert, and their Codama JSON matches JS's except in the fields above. Live data from 52 of them, 378 accounts and 996 instructions, decodes the same through this crate's parser and the JS-converted one. It also matches `@coral-xyz/anchor`'s coder, with two exceptions. That coder keeps one of the two fields named `padding` that one account's IDL declares, where both parsers keep both. And all three fail on 56 instructions whose discriminators the uploaded IDL does not list and on 2 accounts that do not fit their IDL layout.

## Fixtures

`tests/idls/anchor/<name>.anchor.json` (repository root) is the input. `<name>.codama.json` is the JS `rootNodeFromAnchor` output for it. `regenerate.mjs` rebuilds the outputs with the pinned versions; see the header of that file.

| Fixture | Source |
|---|---|
| `dex_v1` | Carbon `examples/versioned-decoders` (MIT) |
| `anchor_external` | Anchor's test suite (Apache-2.0) |
| `anchor_external_legacy` | Anchor's test suite (Apache-2.0): an older, legacy-format version of the program in `anchor_external` |
| `shapes` | Written for this crate: structs, enums and aliases, generics, nested account groups, option, coption, vec and array wrappers, `string`, `bytes` and `[u8; N]`, events, errors, constants, PDA seeds and discriminators |

## In `include_shipstern_parser!`

The macro's loader (`shipstern-proc-macro`, `parse::load_idl`) sends any input without a top-level `kind` or `program` object through this crate, so an Anchor IDL can be passed to the macro directly. The user guide is [`docs/anchor-idl-parser.md`](../../docs/anchor-idl-parser.md).
