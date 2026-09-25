# shipstern-codama-from-anchor

Converts an Anchor IDL into [`codama-nodes`](https://crates.io/crates/codama-nodes), so a Shipstern parser can be generated from an Anchor program without running `npx codama convert` first.

```rust
let idl = std::fs::read("idls/my_program.json")?;
let root = shipstern_codama_from_anchor::root_node_from_anchor(&idl)?;
```

`root_node_from_anchor_idl` takes an already deserialized `idl::Idl` instead.

## Scope

Only Anchor IDL spec `0.1.0` is supported, which is what Anchor 0.30 and later emit. Anything else, including a legacy IDL with no `metadata.spec`, fails with `Error::UnsupportedSpec`. The JS converter falls back to its legacy path for any spec it does not recognize; this crate refuses instead of guessing.

## Parity with `@codama/nodes-from-anchor`

The reference is `@codama/nodes-from-anchor@1.5.6` (with `codama@1.11.0` and `@codama/visitors@1.11.0`), which is what existing Shipstern parsers were generated with. Two tests hold the crate to it:

- `shipstern-proc-macro`'s `anchor_parity_tests` expands a parser from the JS CLI output and from this crate's output, and requires the two to be byte-identical.
- `tests/parity.rs` compares the converted nodes field by field against the JS pipeline minus the passes listed below, and names the JSON path of any difference. It covers every fixture but `pump_fun`, whose second 460 KB copy is not worth keeping.

The second catches things the first cannot see on a given corpus. Replacing the JS `camelCase` port with `codama-rs`'s own case conversion, for example, leaves every real-world fixture's parser unchanged but breaks `shapes`, and the field-level test reports `program.constants[0].name: JS "aRRAY", Rust "array"`.

### Passes

`codama convert` runs the mapping and then the seven passes in `defaultVisitor.ts`. Four are ported. The other three were removed one at a time from the JS pipeline and the resulting Codama JSON expanded into a parser; the parser came out byte-identical to the full pipeline's on every fixture, with and without the `proto` and `program-events` features.

| Pass | Ported | Effect on the generated parser |
|---|---|---|
| extractPdas | no | none: moves inline PDAs into `program.pdas`, which the parser never reads |
| setInstructionAccountDefaultValues | no | none: fills payer, identity and known program addresses for client code |
| deduplicateIdenticalDefinedTypes | no | none: acts only on identical defined types whose camel-cased names collide, which no fixture has |
| setFixedAccountSizes | yes | none today: sets `account.size`, which the IR stores but no renderer reads |
| unwrapInstructionArgsDefinedTypes | yes | changes `shapes` (a struct used only as one argument is inlined) |
| flattenInstructionDataArguments | yes | changes `anchor_external` and `shapes` |
| transformU8ArraysToBytes | yes | changes `anchor_external`, `mpl_token_metadata` and `shapes` |

Removing flatten or u8-arrays-to-bytes does change the parser, which shows the comparison can tell the difference.

## Known divergences

These are the places where output can differ from the JS converter's.

- **PDA placement.** PDAs stay inline in instruction account defaults, and `program.pdas` stays empty. The PDA-name collision renaming extractPdas does is therefore absent too.
- **No client-side account defaults.** Instruction accounts carry no payer, identity, program-id or SPL/MPL address defaults unless the IDL gives an `address`.
- **Colliding defined-type names.** Two structurally identical defined types whose names camel-case to the same string are both kept. JS removes the duplicates. Anchor writes one entry per Rust type, so this takes names like `MyType` and `my_type` in one program.
- **Inlining can count uses differently.** In JS the histogram also counts PDA seeds hoisted into `program.pdas`. A defined struct used once as an argument and also as a whole PDA seed would be inlined here but not in JS. Anchor seeds must be `AsRef<[u8]>`, which rules that out in practice.
- **Large integer constants.** Constants above 2^53 are kept exact; JS rounds them to the nearest double. Integers past `u64`/`i64` become floats. The parser does not read constants.
- **Out-of-range discriminator bytes.** A discriminator byte above 255 fails to deserialize; JS writes malformed hex.
- **Missing error codes.** An error with no `code` fails to deserialize. JS writes `-1`, which `codama-nodes` cannot load either.
- **Missing program address.** An IDL with no `address` is rejected. JS converts it but leaves out `publicKey`, and shipstern cannot load that output, because the address becomes the generated parser's `PROGRAM_ID`.
- **Nesting past 128 levels.** JSON or type nesting deeper than `NESTING_LIMIT` (128) fails with `Error::RecursionLimit`. JS accepts it up to its own stack depth. The limit is deliberate, because it also stops unbounded recursion. The deepest IDL among 105 mainnet programs nests 15 levels.

These inputs fail in both converters. The JS crashes on them; this crate returns an error:

- **Self-referential generic arguments.** A generic argument naming a parameter of the same name, as Anchor's own `tests/idl/idls/generics.json` does (`GenericEnum<T, U, N>` passed `[T, U, N]`). JS resolves arguments in the callee's scope and overflows its stack. This crate stops at a recursion limit with `Error::RecursionLimit`.
- **`{ "kind": "type", "alias": ... }` type definitions**, as in Raydium CLMM's IDL, are unrecognized in both.
- **A PDA seed naming a missing instruction argument** fails in both.

Read by neither converter: instruction `returns`, account `relations`, a seed's `account` hint, type `repr` and `serialization`, and metadata beyond name, version and spec. Because `serialization` is ignored, a `bytemuck` (zero-copy) account is described as if it were Borsh-encoded, in both converters.

Some differences from Anchor's own decoder come from shipstern, not from either converter, so they happen with `codama convert` output too. A generated parser takes instruction accounts by position and fails with `Account does not exist at index N` when a transaction passes fewer accounts than the IDL declares. Anchor's decoder never reads the accounts. On mainnet this happens with Orca Whirlpool `swap` and `swap_v2`, which pass one account fewer than the published IDL, and with `jupZ4m2G…` `swap_in`.

## Fixtures

`tests/fixtures/<name>.anchor.json` is the input. `<name>.codama.json` is what `codama convert` produced from it. `<name>.parity.codama.json`, absent for `pump_fun`, is that pipeline without the three unported passes. `regenerate.mjs` rebuilds both outputs with the pinned versions; see the header of that file.

| Fixture | Source |
|---|---|
| `dex_v1`, `dex_v2` | Carbon `examples/versioned-decoders` (MIT) |
| `anchor_relations`, `anchor_external`, `anchor_docs_pda`, `mpl_token_metadata` | Anchor's test suite and docs (Apache-2.0) |
| `pump_fun` | Pump.fun's on-chain IDL, `6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P` |
| `jupiter_lending` | Jupiter Lend's on-chain IDL, `jup3YeL8QhtSx1e253b2FDvsMNC87fDrgQZivbrndc9` |
| `shapes` | Written for this crate: one of every construct the converter maps |

## In `include_shipstern_parser!`

The macro's loader (`shipstern-proc-macro`, `parse::load_idl`) sends any input without a top-level `kind` through this crate, so an Anchor IDL can be passed to the macro directly. Codama JSON takes the old path unchanged.
