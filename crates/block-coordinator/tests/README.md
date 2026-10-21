# Block Coordinator Tests

## Test Files

| File | Purpose |
|------|---------|
| `integration.rs` | Synthetic tests for all coordinator behaviors |
| `replay_fixtures.rs` | Real data validation using captured geyser data |

## Test Coverage

### Two-Gate System
| Test | Verifies |
|------|----------|
| `two_gate_flush_end_to_end` | Both gates required, records sorted |
| `empty_slot_flushes` | Gate 1 satisfied with 0 transactions |
| `incomplete_slot_blocks_subsequent` | Incomplete slot blocks flush |
| `metadata_and_confirmation_before_entries_with_parser_already_done` | Early metadata and confirmation wait for delayed entries even when parsing is complete |
| `metadata_and_confirmation_before_entries_still_wait_for_parser` | Delayed entries complete reconstruction while the independent parser gate still waits |
| `out_of_order_duplicate_entries_do_not_complete_pending_metadata` | Repeated entries count once; out-of-order entries and zero-transaction entries remain required |
| `pending_metadata_does_not_mix_entries_from_competing_banks` | Each candidate bank must supply its own complete entries; only the confirmed winner flushes |
| `confirmed_child_waits_for_late_parent_entries` | A confirmed child waits for its parent's delayed entries and both flush in parent order |

### Sequential Ordering
| Test | Verifies |
|------|----------|
| `sequential_flush_order` | Earlier slot blocks later ones |
| `gap_in_sequence_blocks_flush` | Missing parent blocks child |
| `first_child_cannot_anchor_over_an_already_observed_parent` | A parent observed through CreatedBank remains required even before the first output; both outputs wait |
| `entirely_missing_parent_stops_before_child_flush` | A required parent absent from every input channel still stops publication and requires replay |
| `stream_closes_with_missing_parent_returns_error_without_descendant_output` | A finite or disconnected stream with a required parent gap returns an error rather than silently succeeding |
| `stream_closes_with_unparsed_accounts_requires_recovery` | An instruction flush does not permit successful shutdown while the same block still lacks account output |

### Missing Canonical Data and Discard Handling

A timeout does not authorize skipping a produced canonical block. Once the grace
expires, unresolved required data stops the coordinator with an actionable error
so recovery can replay from the last published block. Actual skipped slots and
abandoned candidate banks remain eligible for cleanup.

| Test | Verifies |
|------|----------|
| `dead_slot_discarded` | Dead slot removed, no output |
| `dead_slot_spares_sibling` | A dead slot does not discard its sibling |
| `incomplete_confirmed_block_stops_before_child_flush` | A confirmed block with missing entries stops with the missing slot and observed progress; neither output skips to its child |
| `cleared_pending_metadata_does_not_block_replacement_bank` | Clearing a bank waiting for entries rejects its late data and preserves the replacement |
| `dead_metadata_first_bank_cannot_revive_and_replacement_can_flush` | Death tombstones a metadata-first bank before wrapper registration, rejects late data, and permits a replacement |
| `frozen_loser_does_not_hide_missing_entries_of_confirmed_winner` | Missing entries of the confirmed winner require recovery; a complete losing candidate cannot fill the gap |
| `late_block_meta_for_losing_bank_keeps_confirmed_bank` | Only the confirmed bank of a slot flushes |
| `lost_parent_block_meta_stops_before_child_flush` | A required parent that never freezes stops the coordinator after grace; a later parent arrival requires replay |
| `lost_parent_after_a_jump_stops_before_child_flush` | Progress after a jump still exposes the missing canonical parent and stops publication |
| `lost_block_meta_of_winning_bank_stops_before_child_flush` | A frozen losing bank cannot substitute for the winning parent bank whose metadata is missing |
| `late_parent_block_meta_within_grace_flushes` | A confirmed child's records wait for its ambiguous parent's winning bank, then both flush in order inside grace |
| `record_for_skipped_slot_does_not_block_forever` | A record for a skipped slot that never freezes is discarded after 300 slots |
| `late_record_for_skipped_slot_does_not_block_forever` | The same holds when the record arrives after the child froze |
| `record_below_first_parent_edge_requires_recovery` | An early orphan not proven off-chain requires recovery rather than being skipped on timeout |
| `frozen_parent_that_never_confirms_stops_before_child_flush` | An ambiguous frozen parent stops the coordinator at the deadline; no descendant is published |
| `late_frozen_parent_that_never_confirms_stops_before_child_flush` | A parent freezing after child confirmation preserves the original grace, then stops without guessing a winner or skipping the parent |
| `late_forked_parent_that_never_confirms_stops_before_child_flush` | A fork report after a repaired root cannot authorize skipping a required canonical parent |
| `second_parent_bank_freeze_keeps_original_grace_deadline` | A second candidate just before expiry does not restart the deadline or permit publishing across the gap |
| `far_bank_on_other_branch_keeps_slot_with_records` | A slot with records that nothing skips is not discarded |
| `alpenglow_switch_skips_vote_only_slots` | Vote-only slots of the Alpenglow switch never block, and the last Tower block flushes on Finalized alone |
| `child_of_repaired_dead_slot_flushes` | A forked report does not stop a confirmed child of a repaired slot |
| `forked_report_after_confirm_keeps_confirmed_bank` | A forked report after the confirm keeps the confirmed bank |
| `child_confirm_before_forked_parent_keeps_parent` | A child confirm before its fork-reported parent's confirm keeps the parent |
| `fork_report_after_child_confirm_keeps_parent` | A fork report that comes with the parent's late BlockMeta keeps the parent |
| `dead_bank_record_does_not_block_after_repair_loses` | A record of a dead bank does not block later slots |
| `unfrozen_bank_of_forked_slot_can_still_win` | A bank that has not frozen survives a fork report and can win |
| `bank_frozen_after_fork_report_does_not_block` | A bank that freezes after a fork report does not block later slots |
| `discarded_slot_ignores_parsed_messages` | Messages for discarded slot dropped |
| `cleared_banks_release_payloads_and_ignore_late_callbacks` | Explicit invalidations release records immediately and preserve replacement banks |
| `commitment_for_cleared_bank_requires_recovery` | Confirmed or finalized status contradicting a cleared bank stops with an error instead of silently selecting it |
| `clearing_raw_confirmed_bank_with_pending_metadata_requires_recovery` | Clearing a bank already confirmed on the raw stream stops immediately even when delayed entries prevented the wrapper from confirming it |

### Fork Handling
| Test | Verifies |
|------|----------|
| `sibling_fork_via_finalized` | A forked sibling does not block a later slot on the winner |

### Edge Cases
| Test | Verifies |
|------|----------|
| `parsed_messages_before_lifecycle_are_buffered` | Early messages preserved |
| `duplicate_confirm_does_not_change_frozen_count` | A second confirm keeps the frozen account count |
| `malformed_block_meta_or_entry_is_skipped` | A bad hash, or a parent at or above the slot, is skipped |
| `missing_bank_id_stops_coordinator` | Bank ID 0 in two slots stops the coordinator with `MissingBankId` |
| `bank_in_two_slots_stops_coordinator` | One bank ID in two slots stops the coordinator with an error |
| `output_channel_closed_returns_error` | A closed instruction output stops the coordinator with an error |

### Account Gate
| Test | Verifies |
|------|----------|
| `account_gate_blocks_flush_until_account_count_received` | Accounts flush once the confirmed account count is met |
| `account_only_mode_flushes_without_transaction_parsed_messages` | With the tx gate off, instructions flush with no TransactionParsed |
| `account_event_after_confirmed_is_warn_not_error` | An account event after the confirm is dropped, not an error |

### Late Post-Flush Messages
| Test | Verifies |
|------|----------|
| `late_message_for_flushed_slot_is_dropped` | Drops stale parsed events after the flush frontier |

### Replay (`replay_fixtures.rs`)
| Test | Verifies |
|------|----------|
| `replay_fixture_through_coordinator` | Real slots flush in order with the right tx count and parent |
| `replay_fixture_with_competing_bank_flushes_only_confirmed_bank` | A second bank that never confirms does not change the output |

## Fixture File

`fixtures/sample.bin`: captured 09-oct-2026 from a live mainnet Yellowstone gRPC stream (proto 14, with bank IDs).

**Contents:**
- 50 slots (454787399 - 454787448)
- 60,420 total messages
- 60,073 entries
- 295 slot lifecycle events
- 50 BlockMeta events

## Running Tests

```bash
cargo test -p shipstern-block-coordinator
cargo test -p shipstern-block-coordinator -- --nocapture
```

## Custom Fixture Path

```bash
FIXTURE_PATH=/path/to/fixture.bin cargo test -p shipstern-block-coordinator
```
