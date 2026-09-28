# shipstern-codama-from-anchor

Converts an Anchor IDL into [`codama-nodes`](https://crates.io/crates/codama-nodes), so a Shipstern parser can be generated from an Anchor program without running `npx codama convert` first.

It is internal to Shipstern for now: the API may change, and it may move to codama-rs once v2 is out.

```rust
let idl = serde_json::from_slice(&std::fs::read("idls/my_program.json")?)?;
let root = shipstern_codama_from_anchor::root_node_from_anchor(idl)?;
```

## Scope

Only Anchor IDL spec `0.1.0` is supported, which is what Anchor 0.30 and later emit. Anything else, including a legacy IDL with no `metadata.spec`, fails with `Error::UnsupportedSpec`. The JS converter falls back to its legacy path for any spec it does not recognize; this crate returns the error instead.

## Parity with `@codama/nodes-from-anchor`

The reference is `@codama/nodes-from-anchor@1.5.6` (with `codama@1.11.0` and `@codama/visitors@1.11.0`). `shipstern-proc-macro`'s `anchor_parity_tests` expands a parser from the JS `rootNodeFromAnchor` output (what `codama convert` runs) and from this crate's output for every fixture, and requires the two to be byte-identical. `tests/js_parity.rs` also diffs the full Codama JSON against the JS output, after undoing extractPdas, setInstructionAccountDefaultValues and setFixedAccountSizes on the JS side.

### Passes

`codama convert` runs the mapping and then the seven passes in `defaultVisitor.ts`. The three that change the generated parser are ported. extractPdas is not ported, but unwrapInstructionArgsDefinedTypes counts the seed types it hoists into `program.pdas`, so `passes.rs` counts the same seed types where they stay inline. The other three were removed one at a time from the JS pipeline and the parser came out byte-identical on every fixture, with and without the `proto` and `program-events` features. Removing any of the three ported passes does change it, so the comparison can tell the difference.

| Pass | Ported | Effect on the generated parser |
|---|---|---|
| extractPdas | no, emulated | its seed types count toward unwrapInstructionArgsDefinedTypes, so `passes.rs` counts them inline |
| setInstructionAccountDefaultValues | no | none: fills payer, identity, known program and sysvar addresses for client code |
| deduplicateIdenticalDefinedTypes | no | none on every fixture; colliding names are rejected, see below |
| setFixedAccountSizes | no | none: sets `account.size`, which no renderer reads |
| unwrapInstructionArgsDefinedTypes | yes | changes `shapes` (a struct used only as one argument is inlined) |
| flattenInstructionDataArguments | yes | changes `anchor_external` and `shapes` |
| transformU8ArraysToBytes | yes | changes `anchor_external` and `shapes` |

## Known divergences

- **No account sizes.** `account.size` stays `None`.
- **PDA placement.** PDAs stay inline in instruction account defaults, and `program.pdas` stays empty, so the renaming of colliding PDA names that extractPdas does is absent too.
- **No client-side account defaults.** Instruction accounts carry no payer, identity, program-id or SPL/MPL address defaults unless the IDL gives an `address`.
- **Colliding defined-type names.** Two defined types whose names camel-case to the same string, such as `MyType` and `my_type`, fail with `Error::NameCollision`, since both would become one Rust type. JS deletes every copy of identical ones and leaves the links to them dangling, so its output does not compile either.
- **Large integer constants.** Constants above 2^53 are kept exact; JS rounds them to the nearest double. Integers past `u64`/`i64` become floats. The parser does not read constants.
- **Out-of-range discriminator bytes.** A discriminator byte above 255 fails to deserialize; JS wraps it modulo 256, so `256` becomes `00`.
- **Missing error codes.** An error with no `code` fails to deserialize. JS writes `-1`, which `codama-nodes` cannot load either.
- **Missing program address.** An IDL with no `address` is rejected. JS converts it but leaves out `publicKey`, and shipstern cannot load that output, because the address becomes the generated parser's `PROGRAM_ID`.
- **Nameless enum variants.** A variant with no `name` is rejected. JS writes it with an empty name, which cannot become a Rust identifier.
- **Empty discriminators.** An instruction, account or event with `"discriminator": []` is rejected, since it would match everything. JS writes it as is.
- **Names that are not identifiers.** A program, instruction, account, argument, type, field or variant name that camel-cases to an empty or digit-leading string, such as `__` or `1st`, fails with `Error::InvalidName`. JS writes it as is. Error, constant and PDA names are not rendered and are not checked.
- **Unresolvable PDA seeds.** A PDA whose seed names an argument the instruction does not have, or has a kind other than `const`, `arg` or `account`, is dropped. JS fails the whole conversion. The parser never reads PDAs, so every IDL JS accepts still gives the same parser.
- **Generic arguments resolve where they are written.** JS resolves them in the callee's scope, so a parameter name the callee reuses captures the caller's argument: `Outer<A, B>` passing `Wrapper<B, u8>` gives `Wrapper`'s `A` the wrong type, and the parser misreads every field after it.
- **Generic enums.** A generic enum fails with `Error::GenericEnum`. JS expands it inline like any generic, and the parser renderer panics on an inline enum, as `arcium` shows.
- **Dangling type links.** A `defined` type with no entry in `types`, or a generic type used without arguments, fails with `Error::UndefinedType` or `Error::GenericArgsMissing`. JS writes the link, which fails later in rustc.
- **Nesting past 128 levels.** Type nesting deeper than 128 levels fails with `Error::RecursionLimit`. JS accepts it up to its own stack depth. Through the macro, plain deep nesting hits serde_json's parser limit first. The deepest IDL among 105 mainnet programs nests 11 levels by this crate's count.
- **Expansion past 100,000 type nodes.** Generic arguments are expanded at every use. Generics nested so their expansion doubles at each level fail with `Error::TooLarge` instead of hanging the build. JS has no bound.

Both converters reject `{ "kind": "type", "alias": ... }` type definitions, as in Raydium CLMM's IDL.

Read by neither converter: instruction `returns`, account `relations`, a seed's `account` hint, type `repr` and `serialization`, and metadata beyond name, version and spec. Because `serialization` is ignored, a `bytemuck` (zero-copy) account is described as if it were Borsh-encoded, in both converters. That is right when the layout has no implicit padding, which `bytemuck` guarantees. A `bytemuckunsafe` (`zero_copy(unsafe)`) `repr(C)` type can have some, and then every field after the gap is misread: 1 account among the 105 mainnet programs does: `voltr_vault`'s `AdaptorAddReceipt`, with 7 bytes before `last_updated_epoch`.

## Fixtures

`tests/idls/anchor/<name>.anchor.json` (repository root) is the input. `<name>.codama.json` is the JS `rootNodeFromAnchor` output for it. `regenerate.mjs` rebuilds the outputs with the pinned versions; see the header of that file.

| Fixture | Source |
|---|---|
| `dex_v1` | Carbon `examples/versioned-decoders` (MIT) |
| `anchor_external` | Anchor's test suite (Apache-2.0) |
| `shapes` | Written for this crate: structs, enums and aliases, generics, nested account groups, option, coption, vec and array wrappers, `string`, `bytes` and `[u8; N]`, events, errors, constants, PDA seeds and discriminators |

## In `include_shipstern_parser!`

The macro's loader (`shipstern-proc-macro`, `parse::load_idl`) sends any input without a top-level `kind` or `program` object through this crate, so an Anchor IDL can be passed to the macro directly. The user guide is [`docs/anchor-idl-parser.md`](../../docs/anchor-idl-parser.md).
