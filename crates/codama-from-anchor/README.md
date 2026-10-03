# shipstern-codama-from-anchor

Converts an Anchor IDL into [`codama-nodes`](https://crates.io/crates/codama-nodes), so a Shipstern parser can be generated from an Anchor program without running `codama convert` first.

It is internal to Shipstern for now: the API may change, and it may move into codama-rs later.

```rust
let idl = serde_json::from_slice(&std::fs::read("idls/my_program.json")?)?;
let root = shipstern_codama_from_anchor::root_node_from_anchor(idl)?;
```

## Scope

Only Anchor IDL spec `0.1.0` is supported, which is what Anchor 0.30 and later emit. Anything else, including a legacy IDL with no `metadata.spec`, fails with `Error::UnsupportedSpec`. The JS converter falls back to its legacy path for any spec it does not recognize; this crate returns the error instead.

## Compared with `@codama/nodes-from-anchor`

The reference is `@codama/nodes-from-anchor@1.5.6` with `codama@1.11.0` and `@codama/visitors@1.11.0`, the code `codama convert` runs. Two tests hold this crate to it:

- `shipstern-proc-macro`'s `anchor_parity_tests` expands a parser from the JS output and from this crate's output for every fixture and requires the two to be byte-identical.
- `tests/js_parity.rs` diffs the full Codama JSON against the JS output, after undoing extractPdas, setFixedAccountSizes and the payer, identity and program ID account defaults, which this crate does not port.

### Function map

| JS (`nodes-from-anchor`, v01) | Rust | Behavior |
|---|---|---|
| `rootNodeFromAnchor` | `lib.rs` `root_node_from_anchor` | Same, after a spec check (see Scope) |
| `programNodeFromAnchorV01` | `v01.rs` `program_node` | Same field order; adds the checks listed under Known divergences |
| `extractGenerics`, `unwrapGenericTypeFromAnchorV01` | `types.rs` `Generics`, `defined_type`, `resolve` | Generic arguments resolve in the caller's scope, not the callee's |
| `typeNodeFromAnchorV01`, `IDL_V01_TYPE_LEAVES` | `types.rs` `type_node_at`, `leaf_type_node` | Same keys, in the same order; also reads Anchor's alias form |
| `arrayTypeNodeFromAnchorV01`, `optionTypeNodeFromAnchorV01` | `types.rs` `array_len`, `const_value`, `option` | Same for integer and const lengths; keeps a const passed on as a type argument |
| `enumTypeNodeFromAnchorV01` and its variant helpers | `types.rs` `enum_type` | Same; a nameless variant is rejected |
| `structTypeNodeFromAnchorV01`, `tupleTypeNodeFromAnchorV01` | `types.rs` `struct_type`, `tuple_type` | Same |
| `accountNodeFromAnchorV01`, `eventNodeFromAnchorV01` | `v01.rs` `account_node`, `event_node`, `payload` | Same; rejects an empty discriminator, and an event that is not a struct |
| `errorNodeFromAnchorV01` | `v01.rs` `error_node` | Same; a missing `code` or `name` fails |
| `constantNodeFromAnchorV01`, `parseConstantValue` | `v01.rs` `constant_node`, `constant_value` | Same; integers stay exact, and a missing `name` fails |
| `getAnchorDiscriminatorV01` | `v01.rs` `base16_bytes` | Same |
| `definedTypeNodeFromAnchorV01` | `v01.rs` `defined_type_node` | Same |
| `instructionNodeFromAnchorV01`, `instructionArgumentNodeFromAnchorV01` | `v01.rs` `instruction_node` | Same; also converts an instruction with no `args` |
| `instructionAccountNodesFromAnchorV01`, `hasDuplicateAccountNames` | `v01.rs` `instruction_accounts`, `has_duplicate_names` | Same group prefixing |
| `instructionAccountNodeFromAnchorV01`, `pdaSeedNodeFromAnchorV01` | `v01.rs` `instruction_account_node`, `pda_default`, `pda_seed` | Same; a PDA with an unresolvable seed is dropped |
| `parseDocs` | `idl.rs` `docs` | Same for a string, an array and `null`; anything else fails |
| `camelCase`, `pascalCase`, `titleCase` (`@codama/nodes`) | `case.rs` `camel_case`, `pascal_case`, `title_words` | Same |
| `CODAMA_VERSION` (`1.9.2`) | `v01.rs` `CODAMA_STANDARD_VERSION` | Same |
| none | `layout.rs` `pad_zero_copy` | This crate only: implicit `repr(C)` padding |
| `defaultVisitor` | `passes.rs` `run` | 3 of the 7 passes and the address rules of a fourth, in JS order; see Passes |
| `setInstructionAccountDefaultValuesVisitor`, `getCommonInstructionAccountDefaultRules` (`@codama/visitors`) | `passes.rs` `set_instruction_account_default_values`, `common_account_address` | Program and sysvar address rules only, each regex spelled out as the names it matches |

### Passes

`codama convert` runs the mapping and then the seven passes in `defaultVisitor.ts`. The three that change the parser on the fixtures are ported, as are the program and sysvar address rules of setInstructionAccountDefaultValues, and extractPdas is emulated. Removing deduplicateIdenticalDefinedTypes or setFixedAccountSizes from the JS pipeline left the parser byte-identical on every fixture, with and without the `proto` and `program-events` features; removing any of the three changes it. No fixture account takes an address from the name rules, so `tests/conversion.rs` covers them.

| Pass | Ported | Effect on the generated parser |
|---|---|---|
| extractPdas | no, emulated | Its seed types count toward unwrapInstructionArgsDefinedTypes, so `passes.rs` counts them where they stay inline |
| deduplicateIdenticalDefinedTypes | no | None on every fixture; colliding names are rejected instead |
| setFixedAccountSizes | no | None: sets `account.size`, which no renderer reads |
| setInstructionAccountDefaultValues | partly: program and sysvar addresses | Used when an instruction leaves that account off; the payer, identity and program ID rules are not ported, the parser never reads them |
| unwrapInstructionArgsDefinedTypes | yes | Changes `shapes`: a struct used only as one argument is inlined |
| flattenInstructionDataArguments | yes | Changes `anchor_external` and `shapes` |
| transformU8ArraysToBytes | yes | Changes `anchor_external` and `shapes` |

## Known divergences

### Inputs where this crate gives a different parser

| Input | JS 1.5.6 | This crate |
|---|---|---|
| `zero_copy(unsafe)` `repr(C)` struct with implicit padding | Reads it as Borsh and misreads every field after a gap: 43 of 271 mainnet `voltr_vault` `AdaptorAddReceipt` accounts | Inserts a `[u8; N]` field, `padding_before_<field>` or `padding_end`, at each gap; also in every struct nested in a `zero_copy(unsafe)` struct, packed or not |
| Generic argument whose name the callee reuses | Resolves it in the callee's scope: `Outer<A, B>` passing `Wrapper<B, u8>` gives `Wrapper`'s `A` the wrong type; on `gmsol_store` and Anchor's `tests/idl/idls/idl.json` it recurses until the stack overflows | Resolves it where it is written |
| Const parameter passed on to another generic as a type argument, as Anchor writes it | Writes no count | Keeps the value |
| Anchor type alias, `{ "kind": "type", "alias": T }` (Raydium CLMM's `TickArrayBitmap`) | Rejects it; reads only `{ "kind": "alias", "value": T }` | Converts it as `T`, like Anchor's `cli/src/codama.rs` and Codama 2.x (codama-idl/codama#1181, not yet on npm) |
| PDA seed naming an argument the instruction lacks, or a seed kind other than `const`, `arg` or `account` | Fails the whole conversion | Drops that PDA; the parser never reads PDAs |
| Instruction with no `args` | Fails | Converts it with none |

### Inputs this crate rejects, naming the item

JS writes output for each of these that fails to compile, panics in the renderer, or misreads data.

| Input | JS 1.5.6 | Error |
|---|---|---|
| `zero_copy(unsafe)` struct holding an option, a vec, a string, a generic, a tuple struct, a `repr(Rust)` struct, an enum with data or an enum with a `repr`, at any depth | Reads it as Borsh | `UnknownLayout` |
| Padded type that Borsh also reads (an instruction argument, an event, or a Borsh account field) | Misreads one of the two | `PaddedBorshType` |
| Two types, instructions, accounts or events of one kind whose names camel-case alike, such as `MyType` and `my_type` | Writes both, which do not compile; deletes every copy of identical types and leaves dangling links | `NameCollision` |
| Instruction, account or event discriminator that is a proper prefix of another of its kind | Writes it; the parser can read one as the other | `AmbiguousDiscriminator` |
| Two accounts with the same discriminator | Writes them; the parser always reads the first | `DuplicateDiscriminator` |
| Event whose type is an enum, an alias or a tuple struct | Writes it; the `program-events` parser panics on it | `TypeNotStruct` |
| Generic enum, such as `arcium`'s `SetUnset` | Expands it inline; the renderer panics on an inline enum | `GenericEnum` |
| Link to an undefined type, a generic type used without arguments, or an unbound generic name | Writes the link, which fails in rustc | `UndefinedType`, `GenericArgsMissing`, `GenericArgMissing` |
| Empty discriminator | Writes it; it matches everything | `EmptyDiscriminator` |
| Name that camel-cases to an empty or digit-leading string, such as `__` or `1st` (program, instruction, instruction account, argument, account, event, type, field or variant) | Writes it | `InvalidName` |
| Nameless enum variant | Writes an empty name | `UnrecognizedType` |
| IDL with no `address` | Leaves out `publicKey`, so Shipstern cannot set `PROGRAM_ID` | `Json` |
| Error with no `code` | Writes `-1`, which `codama-nodes` cannot load | `Json` |
| Discriminator or `const` seed byte above 255 | Wraps it modulo 256, so `256` becomes `00` | `Json` |
| Type nesting deeper than 128 levels | Accepts it up to its stack depth | `RecursionLimit` |
| Generics nested so that each level doubles the expansion, past 100,000 type nodes | No bound | `TooLarge` |

Equal instruction discriminators are kept: the parser tells them apart by account count and fails at parse time on two with the same count. Equal event discriminators already fail to compile in the `program-events` parser. Anchor's IDL build rejects every discriminator case above.

Two inputs fail in both converters, and only the error differs. An argument named `discriminator` clashes with the discriminator argument both prepend, and flattening struct arguments can leave two arguments with one name. JS's flatten pass throws `Cannot flatten struct ... [name]` for both, without naming the instruction. This crate fails with `DiscriminatorArgument` or `ConflictingFlattenedArguments` at that instruction.

### Differences that do not reach the parser

| Codama JSON field | JS 1.5.6 | This crate |
|---|---|---|
| `account.size` | Set by setFixedAccountSizes | `None` |
| `program.pdas` | PDAs hoisted there, colliding names renamed | Empty; PDAs stay inline in account defaults |
| Instruction account defaults | Payer, identity and program ID filled in by account name | Not set |
| Integer constants above 2^53 | Rounded to the nearest double | Exact; past `u64` or `i64` they become floats |
| Missing `metadata.version`, or a missing error or constant `name` | `0.0.0` or an empty name | Fails to deserialize |
| `null` where a list or flag is expected, or docs that are not strings | An empty list, `false`, or the value as is | Fails to deserialize |

### Notes

- The padding layout is computed from numbers, `bool`, pubkeys, arrays, structs other than `repr(Rust)` ones, type aliases, and fieldless enums of 2 to 256 variants with no `repr`, which take one byte. An IDL that leaves out a `repr` the enum has in code, as Stakenet's hand-written steward entry does, would put such an enum at the wrong size. `bytemuck` types need nothing, since `bytemuck` forbids padding.
- The deepest IDL among 105 mainnet programs nests 11 levels by this crate's count. Through the macro, plain deep nesting hits serde_json's parser limit before the 128-level one.
- Read by neither converter: instruction `returns`, account `relations`, a seed's `account` hint, and metadata beyond name, version and spec.

## Fixtures

`tests/idls/anchor/<name>.anchor.json` (repository root) is the input. `<name>.codama.json` is the JS `rootNodeFromAnchor` output for it. `regenerate.mjs` rebuilds the outputs with the pinned versions; see the header of that file.

| Fixture | Source |
|---|---|
| `dex_v1` | Carbon `examples/versioned-decoders` (MIT) |
| `anchor_external` | Anchor's test suite (Apache-2.0) |
| `shapes` | Written for this crate: structs, enums and aliases, generics, nested account groups, option, coption, vec and array wrappers, `string`, `bytes` and `[u8; N]`, events, errors, constants, PDA seeds and discriminators |
| `event_payload_links` | Written for this crate: an instruction argument and an enum variant typed as event payloads (#335, #336) |
| `event_payload_in_event` | Written for this crate: an event field typed as another event's payload |

## In `include_shipstern_parser!`

The macro's loader (`shipstern-proc-macro`, `parse::load_idl`) sends any input without a top-level `kind` or `program` object through this crate, so an Anchor IDL can be passed to the macro directly. The user guide is [`docs/anchor-idl-parser.md`](../../docs/anchor-idl-parser.md).
