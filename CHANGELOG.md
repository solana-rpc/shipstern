# Changelog

All notable changes to this project will be documented in this file.

This project follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/)
and adheres to [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- `codama-nodes-from-anchor`: converts an Anchor IDL (spec 0.1.0) to Codama nodes in Rust, producing the same generated parser as `npx codama convert` ([#TBD](https://github.com/solana-rpc/shipstern/pull/TBD) closing [#329](https://github.com/solana-rpc/shipstern/issues/329) by @senzenn).
- `shipstern-proc-macro`: `include_shipstern_parser!` accepts an Anchor IDL (spec 0.1.0) directly, with no `codama convert` step. Older Anchor IDLs still need converting to Codama JSON first ([#TBD](https://github.com/solana-rpc/shipstern/pull/TBD) by @senzenn).

### Changed

- `shipstern-core`: merging two prefilters that name the same comparison, one of them written with a repeat, now keeps the comparison instead of dropping it and widening ([#333](https://github.com/solana-rpc/shipstern/pull/333) by @senzenn).

### Fixed

- `shipstern-core`: `Prefilter::builder().account_filters(..)` drops repeated comparisons before checking the limit of four, so a config that repeats one is no longer refused ([#333](https://github.com/solana-rpc/shipstern/pull/333) closing [#331](https://github.com/solana-rpc/shipstern/issues/331) by @senzenn).

## [0.11.0] - 2026-09-18

### Changed

- `shipstern-proc-macro`: the deprecation note on `cpi_event_discriminator` and `cpi_event_payload_offset` no longer names a removal version. The 0.9.0 entry below promised their removal for the 0.10 release, which did not happen; they are still parsed and honoured as the fallback for IDLs that declare no CPI event envelope, and no removal version is established. That entry is corrected in place, since it promised a removal consumers may have planned around.
- `shipstern-proc-macro`: bumped the `codama-nodes` pin from `=0.9.1` to `=0.13.2`. codama rewrote its node shapes in 0.9.3, a patch release on the `0.9.1` this repo was pinned to, and 0.10 through 0.13 carry that forward: child nodes are boxed, counts, sizes and offsets moved from `usize` to `u64`, `fixed` and `isOptional` moved from `bool` to `Option<bool>`, and `programNode.origin` became a closed `anchor | shank` enum. An absent `fixed` or `isOptional` still reads as `false`, the value codama 0.9 deserialized it to, so no account or instruction layout changes. The pin stays exact rather than a caret because the macro reads these node types directly, so their Rust shapes are part of its API surface and codama is pre-1.0 ([#322](https://github.com/solana-rpc/shipstern/pull/322) by @senzenn).
- **Breaking** for IDLs that declare a non-standard `programNode.origin`: any value other than `"anchor"` or `"shank"` no longer loads. It previously deserialized into a free-form `Option<String>` and was silently dropped; codama 0.13 models the field as a closed enum, so it is rejected when the IDL loads, with a message naming the offending value and the accepted set. Omitting the field is still fine ([#322](https://github.com/solana-rpc/shipstern/pull/322) by @senzenn).

### Fixed

- `shipstern-proc-macro`: byte and string fields now honour the Codama length prefix declared in the IDL. Every `sizePrefixTypeNode` was decoded with borsh's native `u32` little-endian prefix, so any other width or endianness (`u8`, `u16`, `u64`, `u128`, big-endian, `shortU16`) read the wrong length and misaligned every field after it. A `u64` LE prefix, for instance, took the low four bytes as the length and the high four as payload, shifting the field and dropping four bytes off its tail. **Decoded output changes for affected IDLs**, and the loss is not recoverable after the fact: replaying an already-truncated payload cannot bring back bytes that were never emitted, so consumers who need the full value must reparse from the original transaction data. No IDL bundled in this repo is affected; all of their prefixes are `u32`/`le` ([#325](https://github.com/solana-rpc/shipstern/pull/325) by @Aursen).
- `shipstern-proc-macro`: counts, sizes and offsets now render as unsuffixed integer literals in generated parsers. Bare `quote!` interpolation of a Rust integer emits a suffixed literal, so a codama field typed `usize` produced `558usize` and pinned the generated code to that width; the emitted parsers compare against `data.len()` and declare `usize` consts, so those literals stop compiling the moment codama widens a count, size or offset, even though the IDL never changed. Generated output changes textually; parsing behaviour does not ([#321](https://github.com/solana-rpc/shipstern/pull/321) by @senzenn).

## [0.10.0] - 2026-09-10

### Changed

- `shipstern.parse` and `shipstern.handle` are now `debug` spans instead of `info`, so they no longer spam `info`-level trace output ([#313](https://github.com/solana-rpc/shipstern/pull/313) by @ultrasilicon).
- Bumped the Solana/Agave dependency stack from `3.x` to `4.2.2`. This is the half of Transaction V1 (SIMD-0385) support that lives in the dependency graph: `VersionedMessage::V1` first appears in `solana-message` 4.0.0, so a 3.x build cannot represent a v1 message at all. [#304](https://github.com/solana-rpc/shipstern/pull/304) already surfaced the inline budget as `InstructionShared::transaction_config`. Raises MSRV from 1.90 to 1.93, which Agave 4.2.2 requires (`iter::chain` in const contexts at 1.91, `MaybeUninit::write_copy_of_slice` at 1.93); see [#209](https://github.com/solana-rpc/shipstern/issues/209). `enable_tx_v1` is activated on devnet and testnet. On mainnet-beta the feature account is funded but still owned by the System program with no data, so it is staged rather than queued. Devnet is already carrying v1 traffic.
- Bumped `spl-token-2022` from 10 to 11, forced by Agave 4.2.2's `spl-token-group-interface ^0.7.2`. Brings the `PermissionedBurn` extension and its authority type, added to the proto `AuthorityType` enum as value 17, and replaces `OptionalNonZeroPubkey` with `MaybeNull<Address>`. `NativeExtensionData` still rejects `PermissionedBurn`, because there is no proto message for `PermissionedBurnConfig` yet.
- **Wire-visible behaviour change**: `UpdateGroupAuthority.new_authority` now reports `None` where it previously emitted the all-zero sentinel pubkey as a real key. It was built as an unconditional `Some(..)`; `MaybeNull::get()` maps the sentinel to `None` (`Address::NONE` is `[0u8; 32]`, so null and the zero key are the same value, exactly as in the deleted `opt_nonzero_pubkey_to_bytes` helper). `InitializeGroup.update_authority` is unchanged, because it already went through that helper. This is a fix, but consumers reading `new_authority` will see a changed value.
- `shipstern-solana-snapshot-source`: `bank_forks_utils::load_bank_forks` was replaced upstream by `try_load_bank_forks_from_snapshot`, which takes no blockstore and returns `Option`. Two consequences beyond the signature. The source no longer opens the blockstore at `AccessType::PrimaryForMaintenance`, so it no longer takes an exclusive RocksDB lock on the ledger directory, and it now fails with a clear error when the snapshot archives directory holds nothing loadable. Account scanning moved from the now-private `AccountsDb::scan_accounts` to `Bank::scan_all_accounts`; accounts-db 4.x deleted `ScanOrder` entirely (`ScanConfig` is `pub(crate)` and carries only an abort flag), so startup account updates are no longer emitted in pubkey-sorted order. Nothing in shipstern relied on that order.
- `shipstern-solana-rpc-source`: `solana-client` 4.x replaced `get_program_accounts_with_config` with `get_program_ui_accounts_with_config`, which returns RPC-encoded `UiAccount`s. The owner is now parsed from a base58 string and the data decoded, so a malformed owner or an undecodable payload fails the source where previously neither could happen.
- `shipstern-bpf-loader-parser`: `solana-loader-v3-interface` 7 dropped the `Migrate` and `ExtendProgramChecked` variants, so the instruction match is exhaustive and an unknown discriminant now fails earlier, in `UpgradeableLoaderInstruction::try_from_slice`.
- `shipstern-jetstream-source`: `TransactionStatusMeta.err` is serialized with `wincode` instead of `bincode`. Agave's own `solana-storage-proto` moved that same proto field to wincode in 4.2, and the field carries opaque bytes, so nothing but convention keeps the two ends agreeing. The encoders still produce identical bytes for `TransactionError`, so no consumer sees a change today. `bincode` moves to a dev-dependency there, where a test asserts it does not produce v1 transaction wire bytes.

### Fixed

- `shipstern-jetstream-source`: convert `VersionedMessage::V1` instead of failing to build, and carry its inline budget through to the proto. The message conversion matched only Legacy and V0 and hardcoded `config: None`. Adding the match arm alone would have compiled and then silently dropped every V1 inline budget, and since `versioned` is `true` for both V0 and V1, erased the distinction between them on the wire. `config` presence is what marks a message as V1, so it is now wrapped unconditionally: a V1 message that sets no budget fields still serializes as `Some(TransactionConfig::default())`. V1 carries no address lookup tables, and its lifetime specifier maps onto the existing `recent_blockhash` field ([#310](https://github.com/solana-rpc/shipstern/issues/310)).
- `shipstern-jetstream-source`: map `RewardType::DeactivatedStake`, added in Agave 4.2 with a matching proto `DeactivatedStake = 5`. The `transaction_status_meta` reward mapping had a catch-all arm that turned it into `Unspecified`, losing the reward type for anyone reading it ([#310](https://github.com/solana-rpc/shipstern/issues/310)).
- `shipstern-jetstream-source`: Agave 4.2 removed `RewardInfo::commission`, so `keyed_rewards` forwards the exact `commission_bps` instead of deriving it from a percentage. The percent field is still filled in where the basis points divide evenly, since `shipstern-block-meta-parser` exposes `commission` and has no `commission_bps` to fall back on.

## [0.9.0] - 2026-09-08

### Changed

- **Breaking** for code that matches on `PrefilterError` or builds a prefilter with a struct literal. `PrefilterError` gained variants for the account-filter and data-slice checks, and `AccountPrefilter`, `TransactionPrefilter` and `SlotPrefilter` each gained fields. Neither type is `#[non_exhaustive]`, so an exhaustive `match` on the error and any `AccountPrefilter { accounts, owners }` literal stop compiling; add a wildcard arm and `..Default::default()` respectively. Builder users are unaffected ([#303](https://github.com/solana-rpc/shipstern/issues/303)).
- Bumped `yellowstone-grpc-proto` from `12.4` to `12.6`, picking up the Transaction V1 (SIMD-0385) `Message.config` field and the `cuckoo_account_include` transaction filter. The new filter is not exposed through `TransactionFilter` and is always sent as `None` ([#304](https://github.com/solana-rpc/shipstern/pull/304) by @ultrasilicon).
- `shipstern-jetstream-source`: a parser registered on the `block` pipeline whose prefilter sets `block_meta` or `slot` but none of `include_transactions` / `include_accounts` / `include_entries` no longer receives a `Block` update. Previously any `block_meta`/`slot` prefilter forced its filter ID into the block match list, so such a parser received a header-only `Block`: `transactions` and `accounts` were always empty. Use a `block_meta` pipeline instead.

### Deprecated

- `shipstern-proc-macro`: the `cpi_event_discriminator` and `cpi_event_payload_offset` macro arguments are ignored, with a deprecation warning at the call site, when the IDL declares a CPI event envelope. They still apply to IDLs that declare none. No removal version is currently established ([#299](https://github.com/solana-rpc/shipstern/pull/299) by @senzenn).

### Added

- The Yellowstone gRPC source replaces the live subscription mid-stream, so a changing filter set no longer costs a reconnect. `Runtime::handle` hands out a cheap `Clone` `RuntimeHandle` before `run` consumes the runtime; `filters` reports the last set handed to the source, `update_filters` patches it under a lock, `send_filter_update` replaces it wholesale, and `reset_filters` restores the registered set. The handle exists only for sources implementing `FilterUpdateSource`, which today is gRPC alone, so taking one from any other source is a compile error rather than a runtime one. Keys are parser IDs, except for instruction parsers, which the runtime bundles behind a single `InstructionPipeline::ID` entry. A set naming a parser with no registered pipeline is refused with `FilterUpdateError::UnknownParser` before anything is sent, and `Ok(())` means the set reached the source rather than that the server applied it ([#302](https://github.com/solana-rpc/shipstern/pull/302) by @senzenn, [#305](https://github.com/solana-rpc/shipstern/pull/305) and [#307](https://github.com/solana-rpc/shipstern/pull/307) by @kespinola).
- `Filters` gained `get`, `parser_ids`, `insert`, `merge`, and `remove`, keyed by parser ID, so the next subscription can be built without reaching into the map by hand ([#305](https://github.com/solana-rpc/shipstern/pull/305) by @kespinola).
- `Prefilter` can express the rest of Yellowstone's subscription surface, which `impl From<Filters> for SubscribeRequest` previously hardcoded to empty. `AccountPrefilter` gained `filters` (memcmp, data size, token-account state, and lamport comparisons, via the new `AccountFilter`, `MemcmpData` and `LamportsCmp` types) and `nonempty_txn_signature`; `TransactionPrefilter` gained `accounts_exclude`, `vote` and `signature`; `SlotPrefilter` gained `interslot_updates`; and the Yellowstone gRPC source config gained `accounts-data-slice`, which is request-level rather than per-parser. Builder setters check account filters against the rules the server enforces when it builds its own filter (memcmp encoding and size, filter count, at most one data size, no `token-account-state = false`), so a bad config fails at build time instead of taking the subscription down when it is opened; `YellowstoneGrpcConfig::validate` checks the data-slice windows for order and overlap, and the source calls it before it dials. Every field defaults to the previous behaviour. `transactions_status` and `entry` stay unsupported: they are separate subscription kinds with no pipeline behind them, so nothing could consume what they delivered ([#303](https://github.com/solana-rpc/shipstern/issues/303)).
- `shipstern-core`: `InstructionShared` gained `transaction_config: Option<TransactionConfig>`, the inline compute budget that Transaction V1 (SIMD-0385) carries on the message instead of in top-level `ComputeBudget` instructions. It exposes the priority fee, compute-unit limit, loaded-accounts data-size limit, and heap size. It is `None` for Legacy and V0 transactions and for jetstream-source transactions, which are decoded from an SDK without a V1 message variant ([#304](https://github.com/solana-rpc/shipstern/pull/304) by @ultrasilicon).
- `shipstern-proc-macro`: generated account types and the `Instructions` wrapper now expose `DISCRIMINATOR` and `DISCRIMINATOR_OFFSET` constants, so consumers can build `memcmp` filters and route on discriminators without re-deriving the bytes from the IDL. The pair is a memcmp predicate: the bytes the parser compares at that offset. It is not a payload boundary, because numeric discriminators (for example SPL Governance) are re-read as the first field of the account body, so decoding still goes through `try_unpack`. No constant is emitted where the parser could not honor it: size-only discriminators, zero-length discriminators, fixed-size fields whose declared width disagrees with the decoded default bytes, and instruction names that collide after case folding ([#294](https://github.com/rpcpool/yellowstone-vixen/issues/294)).
- `shipstern-proc-macro`: the CPI event envelope is read from the IDL. An IDL that declares the self-CPI envelope on `eventNode.discriminators`, the form Carbon and `@codama/renderers-vixen-parser` emit, previously failed to build with an event discriminator collision, because every event was keyed on the first discriminator in its chain, which for an enveloped IDL is the shared envelope tag. `EVENT_PAYLOAD_OFFSET` had the mirror problem: it came from the macro arguments and defaulted to 8, so the CPI path always stripped a fixed byte count and only happened to be right for a standard Anchor tag. The macro now infers the envelope from the chain (a constant at offset 0 is the tag, the next discriminator's declared offset is the payload offset), checks that every declaring event agrees on both, rebases the remaining discriminators so the CPI and log paths share one layout, and ANDs a chain of two or more. Padded layouts round-trip because the payload offset is the declared offset rather than the tag length. Chains that cannot be honored fail the build instead of parsing arbitrarily: two constants at offset 0, overlapping ranges, a mix of constant and non-constant nodes behind a tag, an event whose only discriminator is the tag, or discriminator bytes that do not decode, which previously panicked the macro. A build-time guard also rejects an envelope tag that prefixes an instruction discriminator at offset 0 in either direction, because the parser filters any instruction whose data starts with the tag before dispatch; it checks the tag actually in use, whether from the IDL, the macro arguments, or the Anchor default. Instruction parsing is untouched ([#299](https://github.com/solana-rpc/shipstern/pull/299) by @senzenn).

### Fixed

- `shipstern-jetstream-source`: emit `SubscribeUpdateBlockMeta` and `SubscribeUpdateSlot` for filters that request them. `block_meta` and `slot` prefilters were previously bucketed into the block match list, so their filter IDs rode on `UpdateOneof::Block`. The runtime dispatches by variant, so those IDs matched no pipeline and were dropped with only a `trace!` line: block-meta and slot parsers ran to completion having handled zero updates, and `shipstern-block-coordinator`, which keys on `BlockMeta`, received none of it on the historical-replay path (delivering it is a prerequisite for driving that crate from replay, not on its own sufficient). Both messages are built from fields already carried on `BlockData::Block`. Replayed slots report `SLOT_FINALIZED`, since Old Faithful archives hold only finalized history and cannot reproduce the processed/confirmed transitions that `SlotPrefilter::filter_by_commitment = false` asks for.

## [0.8.0] - 2026-08-25

### Changed

- **Breaking**: the project is renamed from Yellowstone Vixen to Shipstern and the repository moved to `solana-rpc/shipstern`. Every crate is renamed with the `yellowstone-vixen` prefix replaced by `shipstern` (`yellowstone-vixen` → `shipstern`, `yellowstone-vixen-core` → `shipstern-core`, `yellowstone-vixen-yellowstone-grpc-source` → `shipstern-yellowstone-grpc-source`, and so on), so dependents must update their `Cargo.toml` and `use` paths. The protobuf wire packages move with it: `vixen.stream` → `shipstern.stream` and `vixen.parser.{token,bpf_loader,token_extensions}` → `shipstern.parser.{token,bpf_loader,token_extensions}`. That is the wire contract, not internal naming. The gRPC method path becomes `/shipstern.stream.ProgramStreams/Subscribe`, so clients generated from the old schemas fail with `UNIMPLEMENTED`, and the `google.protobuf.Any` type URLs in `SubscribeUpdate.parsed` become `type.googleapis.com/shipstern.parser.token.TokenAccount` and friends, so old type-URL matches stop matching without erroring. Consumers must regenerate from the new schemas. Wire-compatibility tests now pin the four package declarations and the service path, and fail on any leftover `vixen.*` declaration. Historical changelog links keep the `rpcpool/yellowstone-vixen` URL, which GitHub redirects ([#279](https://github.com/solana-rpc/shipstern/pull/279) by @kespinola).
- `shipstern-jetstream-source`: bumped `jetstreamer-firehose` and `jetstreamer-utils` from `0.5` to `0.7`. **Breaking** for struct-literal construction: `JetstreamSourceConfig` gained `reverse: bool` (default `false`, `#[serde(default)]`, clap `--reverse` in the example), which replays slots newest-first. `reverse` implies `sequential` upstream, so it also gets the bounded ripget window described under Fixed. Setting `buffer_window_bytes` with neither `sequential` nor `reverse` enabled now logs a warning, since upstream ignores the value in that mode ([#285](https://github.com/solana-rpc/shipstern/pull/285) by @senzenn).

### Fixed

- `shipstern-jetstream-source`: sequential replays could hang without emitting a slot. Upstream's default ripget window, the smaller of 4 GiB and 15% of RAM, can exceed what its own 180-second header-read timeout allows to download, so it retried forever. When `buffer_window_bytes` is unset and `sequential` (or `reverse`) is on, the source now passes `DEFAULT_SEQUENTIAL_BUFFER_WINDOW_BYTES` (256 MiB) and logs that it did; an explicit `buffer_window_bytes` still wins ([#284](https://github.com/solana-rpc/shipstern/pull/284) by @senzenn).
- `shipstern-kafka-sink` (`experimental-account-parser`): an account update is now handed only to the account parser whose program ID matches the account's owner. Previously every registered account parser was tried in turn, so an account owned by an unrelated program could be counted as a parse error and routed to another parser's fallback topic ([#283](https://github.com/solana-rpc/shipstern/pull/283) by @the-orex).
- `shipstern` runtime config: a `[buffer]` section that set `jobs` but omitted `sources-channel-size` failed to deserialize and killed the process before it connected. The `Default` impl already used `100`, but serde never consults it for a missing field. The field now carries a serde default and a clap `default_value_t`, both `100`, matching how `auto-reconnect` already behaves in the gRPC source ([#282](https://github.com/solana-rpc/shipstern/pull/282) by @senzenn).
- `shipstern-jetstream-source`: populate `Reward::commission_bps` on both reward conversion paths. The field arrived with `yellowstone-grpc-proto` 12.5 and was hardcoded to an empty string, so consumers reading it saw nothing even though `commission` was already forwarded. It is now derived from the whole-percent commission, which is lossless because the source is a `u8` percentage: a 7% commission reports `700`. Salvaged from ([#254](https://github.com/rpcpool/yellowstone-vixen/pull/254) by @the-orex), which is otherwise superseded by the 0.7.0 dependency bumps.

## [0.7.0] - 2026-07-26

### Changed

- `shipstern` runtime buffer: replaced the topograph executor with a native crossbeam-channel worker pool. Behavioral change: peak outstanding work is now `jobs` total (queued + in-flight); topograph capped running handlers at `jobs` but left the queue behind them unbounded. `jobs` bounds concurrency, so raising it only helps when the handlers are the bottleneck; with cheap handlers the producer is the limit and a higher `jobs` yields no gain. Public `Buffer` API unchanged. On shutdown the buffer waits for handlers that already started, bounded to 30 seconds, so a signal-initiated stop (SIGINT/SIGTERM) no longer cancels them part-way through; a stop additionally discards work still queued behind them, while a clean close runs the queue too. A handler that never returns is logged and abandoned rather than blocking shutdown indefinitely.
- `shipstern-jetstream-source`: bumped `jetstreamer-firehose` and `jetstreamer-utils` from `0.2` to `0.5` to track upstream Anza releases.
- **Breaking** for downstream struct-literal construction: `JetstreamSourceConfig` gained two new fields, `sequential: bool` (default `false`) and `buffer_window_bytes: Option<u64>` (default `None`), to expose the upstream `sequential` / ripget buffer-window controls. Both are `#[serde(default)]` and have clap defaults, so deserialized and CLI-built configs are unaffected; only direct struct-literal initialization needs updating.
- Bumped `yellowstone-grpc-client` from `12` to `13.1` and `yellowstone-grpc-proto` from `12` to `12.4`, picking up the client's built-in auto-reconnect support introduced in v13.1.
- Auto-reconnect on the Yellowstone gRPC source is now **on by default**. It is configured via three new `YellowstoneGrpcConfig` fields: `auto-reconnect` (bool, default `true`), `reconnect-max-retries`, and `reconnect-slot-retention`. When enabled the source uses a sturdy default backoff (10 retries, 500 ms base, 2.0× multiplier — covers outages up to ~4 minutes) instead of the client library's weaker default. Set `auto-reconnect = false` to opt out. Existing config files keep deserializing (`#[serde(default)]`), but downstream struct-literal construction of `YellowstoneGrpcConfig` must initialize the new fields. Requires the server to have `replay_stored_slots` configured for gap-free recovery.

### Added

- `SubscribeRequest` filter structs gained `cuckoo_accounts_filter` (`SubscribeRequestFilterAccounts`), `token_accounts` (`SubscribeRequestFilterTransactions`), and `cuckoo_account_include` (`SubscribeRequestFilterBlocks`) from `yellowstone-grpc-proto` 12.5. All are defaulted to `None`, preserving prior behavior.
- `Reward` gained `commission_bps` (string) from `yellowstone-grpc-proto` 12.5; defaulted to an empty string on the jetstream source's reward conversion paths.

### Fixed

- `shipstern-jetstream-source`: forward per-account block rewards in emitted `SubscribeUpdateBlock`. Previously `rewards.rewards` was hardcoded to `vec![]`, dropping fee/rent/staking/voting rewards on the historical-replay path even though `num_partitions` was forwarded. Adds `convert::keyed_rewards` performing the canonical `KeyedRewardsAndNumPartitions` → `proto::Rewards` mapping (matches `solana-storage-proto`'s `From<Reward>` encoding).

## [0.5.0] - 2025-09-15

### Added

- Exposed transaction included and required accounts using `FilterPipeline` ([#104](https://github.com/rpcpool/yellowstone-vixen/pull/104) by @fernandodeluret)
- Added Stake Pool Program parser ([#87](https://github.com/rpcpool/yellowstone-vixen/pull/87) by @aoikurokawa)
- Exposed transaction message header to shared data ([#121](https://github.com/rpcpool/yellowstone-vixen/pull/121) by @fernandodeluret)
- Added fumarole `Source` ([#111](https://github.com/rpcpool/yellowstone-vixen/pull/111) by @fernandodeluret)
- Added support for all accounts fetching with empty filter ([#124](https://github.com/rpcpool/yellowstone-vixen/pull/124) by @fernandodeluret)
- Added block subscriber to Shipstern ([#126](https://github.com/rpcpool/yellowstone-vixen/pull/126) by @Nagaprasadvr)
- Updated Shipstern metrics to receive prometheus register ([#118](https://github.com/rpcpool/yellowstone-vixen/pull/118) by @fernandodeluret)
- Documentation and example updates for 0.5 release ([#125](https://github.com/rpcpool/yellowstone-vixen/pull/125) by @kespinola)

### Changed

- Updated Raydium CPMM and Launchpad to latest program version ([#122](https://github.com/rpcpool/yellowstone-vixen/pull/122) by @fernandodeluret)
- Updated the discriminator tracing log to debug formatting ([#113](https://github.com/rpcpool/yellowstone-vixen/pull/113) by @fernandodeluret)
- Updated contribution guide ([#123](https://github.com/rpcpool/yellowstone-vixen/pull/123) by @moses7054)

### Fixed

- Return runtime error on stream error ([#112](https://github.com/rpcpool/yellowstone-vixen/pull/112) by @fernandodeluret)
- Fixed import typo in README.md ([#117](https://github.com/rpcpool/yellowstone-vixen/pull/117) by @riprsa)
- Fixed various bugs in block subscriber and related features ([#126](https://github.com/rpcpool/yellowstone-vixen/pull/126) by @Nagaprasadvr)

## [0.4.0] - 2025-07-23

### Added

- Added Meteora Pools Program parser by @sonicfromnewyoke in [#78](https://github.com/rpcpool/yellowstone-vixen/pull/78)
- Added Meteora Dynamic Bonding Curve Program parser by @sonicfromnewyoke in [#77](https://github.com/rpcpool/yellowstone-vixen/pull/77)
- Added Boop Program parser by @sonicfromnewyoke in [#76](https://github.com/rpcpool/yellowstone-vixen/pull/76)
- Added Raydium Launchpad Program parser by @sonicfromnewyoke in [#75](https://github.com/rpcpool/yellowstone-vixen/pull/75)
- Added Virtuals Program parser by @sonicfromnewyoke in [#74](https://github.com/rpcpool/yellowstone-vixen/pull/74)
- Added supported programs list & missing IDLs in parsers by @sonicfromnewyoke in [#80](https://github.com/rpcpool/yellowstone-vixen/pull/80)
- Added Meteora Vault Program parser by @sonicfromnewyoke in [#82](https://github.com/rpcpool/yellowstone-vixen/pull/82)
- Added codama-parser-generation md file by @fernandodeluret in [#89](https://github.com/rpcpool/yellowstone-vixen/pull/89)
- Added Multiple Sources by @kespinola in [#96](https://github.com/rpcpool/yellowstone-vixen/pull/96)

### Changed

- Removed Localset logic and improved Shipstern Runtime docs by @fernandodeluret in [#85](https://github.com/rpcpool/yellowstone-vixen/pull/85)
- Updated codama-parser-generation.md by @XieJunhua in [#93](https://github.com/rpcpool/yellowstone-vixen/pull/93)
- Bumped yellowstone and exposed from_slot filter by @fernandodeluret in [#91](https://github.com/rpcpool/yellowstone-vixen/pull/91)
- Updated parsers with the latest codama features by @fernandodeluret in [#88](https://github.com/rpcpool/yellowstone-vixen/pull/88)
- Exposed tx_sig & slot to raydium handler by @fernandodeluret in [#99](https://github.com/rpcpool/yellowstone-vixen/pull/99)
- Updated RaydiumAmmV4 parser to handle optional accounts by @fernandodeluret in [#98](https://github.com/rpcpool/yellowstone-vixen/pull/98)
- Updated Shipstern parsers with last Codama updates by @fernandodeluret in [#105](https://github.com/rpcpool/yellowstone-vixen/pull/105)

### Fixed

- Fixed protobuf vulnerability (RUSTSEC-2024-0437) by @aoikurokawa in [#81](https://github.com/rpcpool/yellowstone-vixen/pull/81)
- Updated example in README by @quangkeu95 in [#79](https://github.com/rpcpool/yellowstone-vixen/pull/79)

### Removed

- Removed old raydium parser from parser crate by @fernandodeluret in [#94](https://github.com/rpcpool/yellowstone-vixen/pull/94)

## [0.3.0] - 2025-05-06

### Added

- Added Kamino Limit Orders parser (https://github.com/rpcpool/yellowstone-vixen/pull/72)
- Added new set of pasers: (https://github.com/rpcpool/yellowstone-vixen/pull/70)
  - meteora-amm
  - moonshot
  - orca-whirlpool
  - pump-swaps
  - raydium-amm-v4
  - raydium-clmm
  - raydium-cpmm
- Jupiter Swaps Parser (https://github.com/rpcpool/yellowstone-vixen/pull/69)
- Meteora parser (https://github.com/rpcpool/yellowstone-vixen/pull/65)
- Pump Fun parser (https://github.com/rpcpool/yellowstone-vixen/pull/66)
- Subscribe to block_meta (https://github.com/rpcpool/yellowstone-vixen/pull/67)

### Changed

- Remove feature `orca` from `shipstern-parser` moved parser to dedicated crate `shipstern-orca-whirlpool-parser`
- Update raydium-amm-v4 discriminators strategy (https://github.com/rpcpool/yellowstone-vixen/pull/73)
- Proto generation with Codama (https://github.com/rpcpool/yellowstone-vixen/pull/68)

## [0.2.0] - 2025-04-03

### Added

- Set commitment level on grpc subscription (https://github.com/rpcpool/yellowstone-vixen/pull/58)

### Changed

- Make `AccountKeys` get method public (https://github.com/rpcpool/yellowstone-vixen/pull/60)

[Unreleased]: https://github.com/solana-rpc/shipstern/compare/v0.11.0...HEAD
[0.11.0]: https://github.com/solana-rpc/shipstern/releases/tag/v0.11.0
[0.10.0]: https://github.com/solana-rpc/shipstern/releases/tag/v0.10.0
[0.2.0]: https://github.com/solana-rpc/shipstern/releases/tag/v0.2.0
