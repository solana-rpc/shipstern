# Block Coordinator Tests

## Test Files

| File | Purpose |
|------|---------|
| `integration.rs` | Synthetic tests for all coordinator behaviors |
| `replay_fixtures.rs` | Real data validation using captured geyser data |
| `source.rs` | `CoordinatorSource` against a local Geyser server |

## Test Coverage

### Two-Gate System
| Test | Verifies |
|------|----------|
| `two_gate_flush_end_to_end` | Both gates required, records sorted |
| `empty_slot_flushes` | Gate 1 satisfied with 0 transactions |
| `incomplete_slot_blocks_subsequent` | Incomplete slot blocks flush |

### Sequential Ordering
| Test | Verifies |
|------|----------|
| `sequential_flush_order` | Earlier slot blocks later ones |
| `gap_in_sequence_blocks_flush` | Missing parent blocks child |

### Discard Handling
| Test | Verifies |
|------|----------|
| `dead_slot_discarded` | Dead slot removed, no output |
| `dead_slot_spares_sibling` | A dead slot does not discard its sibling |
| `incomplete_block_discarded` | A block with more entries than its BlockMeta counts is discarded as incomplete |
| `entry_after_block_meta_flushes` | An entry that arrives after its BlockMeta completes the block, which flushes in order |
| `confirm_before_late_entry_waits_for_it` | A confirm that comes before the late entry waits for it |
| `late_entry_after_the_child_confirms_still_flushes` | A child confirm does not end the wait; the parent flushes first once its entry comes |
| `entry_that_never_comes_stops_coordinator` | A block whose entry never comes counts as pending, even with no records, and stops the coordinator |
| `first_flush_waits_for_a_parent_the_stream_saw` | The first flush skips only a parent from before the stream started |
| `late_entry_from_before_the_stream_does_not_block_the_start` | A late entry of a block that froze before the stream does not hold back the first flush |
| `commitments_that_never_flush_stop_coordinator` | Commitments for banks that never freeze count toward the pending-bank limit |
| `first_block_of_a_stream_started_mid_slot_is_discarded` | Before the first flush, a block missing its CreatedBank and first entry is discarded and does not block its child |
| `block_meta_before_every_entry_at_startup_waits_for_them` | At startup, a block whose CreatedBank was seen waits for entries that all come after its BlockMeta |
| `second_death_drops_the_held_repair_bank` | A second death of a slot, which block machine 0.11 does not report, still drops its held repair bank |
| `repair_after_a_second_death_flushes_before_its_child` | Dropping banks after an unreported second death keeps the slot open, so a later repair flushes before its child |
| `repair_block_meta_after_its_child_flushes_first` | A child of a dead slot waits for the repair's BlockMeta, even when it comes after the child's |
| `short_replacement_of_a_forked_slot_flushes_before_its_child_at_startup` | At startup, a held replacement in a forked slot is frozen in the state, so its child waits for it |
| `late_entry_of_a_repair_bank_flushes_before_its_child` | A held repair bank puts its dead slot back on the chain, so its child waits for the late entry |
| `late_entry_with_extra_transactions_discards_the_block` | A late entry that overshoots the transaction count discards the block |
| `update_parent_for_a_bank_waiting_for_entries_stops_coordinator` | Clearing a bank that froze short of entries stops the coordinator |
| `late_block_meta_for_losing_bank_keeps_confirmed_bank` | Only the confirmed bank of a slot flushes |
| `lost_parent_block_meta_stops_coordinator` | A parent that never freezes stops the coordinator with `TooManyPendingBanks` instead of a silent stall |
| `lost_parent_after_a_jump_stops_coordinator` | The same holds for the first bank after a jump |
| `lost_block_meta_of_winning_bank_stops_coordinator` | A winning bank whose BlockMeta never comes stops the coordinator at its slot |
| `late_parent_block_meta_flushes` | A parent BlockMeta that arrives under the pending limit still flushes, parent first |
| `record_for_a_seen_skipped_slot_does_not_block` | A skipped slot whose bank the block machine saw is discarded when its child confirms |
| `record_for_an_unseen_skipped_slot_stops_coordinator` | A record for a slot the block machine never saw stops the coordinator |
| `late_record_for_an_unseen_skipped_slot_stops_coordinator` | The same holds when the record arrives after the child froze |
| `record_below_first_parent_edge_stops_coordinator` | A record below the first parent edge at stream start stops the coordinator |
| `frozen_parent_that_never_confirms_stops_coordinator` | A frozen parent that never confirms stops the coordinator |
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
| `commitment_for_a_cleared_bank_stops_coordinator` | A commitment for a bank that UpdateParent cleared stops the coordinator |
| `finalized_child_waits_for_parent_bank_named_by_hash` | A finalized child commits the parent bank its parent blockhash names, not the only candidate seen so far |
| `finalized_child_selects_parent_after_delayed_winning_metadata` | The same when only the winner's BlockMeta is late |
| `finalized_child_waits_for_hash_selected_parents_late_entries` | The same when only the winner's entries are late |
| `finalized_child_verifies_grandparent_before_promoting_sole_candidate` | Finality reaches the block machine only once every ancestor is linked by blockhash |
| `finalized_child_selects_replacement_after_explicit_parent_clear` | A cleared bank does not hide its replacement from the child's parent blockhash |
| `child_with_conflicting_published_parent_hash_stops_before_output` | A child naming another block than the published parent stops the coordinator |
| `cleared_banks_release_payloads_and_ignore_late_callbacks` | Explicit invalidations release records immediately and preserve replacement banks |

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

### Source (`source.rs`)
| Test | Verifies |
|------|----------|
| `unknown_slot_status_is_skipped` | A slot status the proto does not know is skipped, not read as processed or as missing its bank ID |
| `subscription_carries_the_config_data_slice` | The subscription carries the config's data slice, at processed |
| `invalid_data_slice_fails_before_dialing` | A data slice the server would refuse fails the run before it subscribes |
| `entry_update_parent_reaches_the_coordinator` | The source requests UpdateParent and forwards it to the coordinator |

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
