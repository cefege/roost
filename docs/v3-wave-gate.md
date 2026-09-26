# The wave gate checklist

Thirteen agents wrote into one coordinator crate at once, so a test binary
could not link while its crate did not build. Every slice therefore handed the
integrator its **mutation experiments in executable form** — the exact edit, and
the named test that must fail — instead of a caveat. This file is that list.

**This is a gate, not a wishlist.** Each row is a property a slice claims to
protect and a single line whose removal must break a named test. A row whose
test still passes after the edit is a test that does not test anything, and the
row stays open until it fails.

Run each mutation, observe the named failure, restore, observe the pass. Record
the result beside the row. **A row that has never been observed failing is
unverified, and "unverified" is the honest state — not "probably fine".**

## Worker track

| # | Property | File and exact edit | Test that must fail | State |
|---|---|---|---|---|
| W1 | A record is born `Spawned`, not the FSM's retired `None`; a second close is REFUSED | `session/types.rs`, `SessionRecord::new`: `fsm: ChannelFsm::new()` → `default()` | `a_new_record_can_be_attached_and_ends_exactly_once` — **its SECOND assertion**; the attach alone passes under a weaker fix | **NEVER RUN.** The fix for the defect W2a found in the lead's own W-0 file. Most load-bearing row on the branch. |
| W2 | A delta past the row cap escalates to a full frame, never a silent hole | `roost-term/src/emitter.rs:127`: `> LIVE_DELTA_SCROLLBACK_ROWS_CAP` → `> u64::MAX` | `a_delta_past_the_row_cap_becomes_a_viewport_only_full` | unrun. **The only row guarding behaviour inside a COMPLETED ported crate.** |
| W3 | An overflow drops exactly one sink and tells it once | `session/cell_sink.rs::drop_for_overflow`: remove `sink.on_overflow()` | `an_overflow_drops_exactly_one_sink_and_tells_it_once` | unrun |
| W4 | A delta too large for one part escalates to a full | `session/emit.rs::build_frame`: delete the `encoded_cell_grid_frame_size(&wire) > CELL_GRID_PART_MAX_BYTES` escalation | `a_delta_too_large_for_one_part_is_escalated_to_a_full` | unrun |
| W5 | The retained floor stays the offset before the oldest retained byte | `session/types.rs::append_retained`: `head_seq += self.scrollback.len()` instead of `chunk.len()` | `the_history_floor_is_the_offset_before_the_oldest_retained_byte` + 2 | **OBSERVED** — 3 named tests failed, in `734982d1` |
| W6 | A keeper control credential is recognised whatever its case | `shell_spec.rs::is_keeper_control_key`: drop `to_ascii_uppercase()` | `a_keeper_control_credential_is_recognised_whatever_its_case` | **OBSERVED** in `734982d1`. **Re-run:** W1's credential cutover landed after that observation. |
| W7 | A frame from `Box<dyn TerminalCore>` is byte-identical to one from `AlacrittyCore` | the `&dyn TerminalCore` widening in `roost-term` | no test yet — this row needs one written | unrun. A widening that silently changed frame bytes is exactly the defect nobody is looking for. |
| W8 | A spawn's ack is correlated 1:1 with its request under concurrency | keeper pool dispatch | W4's ack-correlation test | unrun |

## Coordinator track — rate limiter (M2)

All four **OBSERVED** in an isolated scratch crate, all restored. Re-runnable as
gate checks without reconstructing the method.

| # | Edit | Test that must fail |
|---|---|---|
| C1 | key the reconnecting caller as `"198.51.100.9#conn-2"` | `a_reconnecting_caller_finds_the_budget_it_left_and_another_caller_finds_its_own` |
| C2 | `spend`: `let key = (method.to_owned(), String::new())` | the same test, plus `a_full_bucket_table_fails_closed_and_recovers_when_its_windows_elapse` |
| C3 | add `"WorkspacesList"` to `RATE_LIMITED_METHODS` | `a_read_neither_opens_a_window_nor_spends_a_mutations_budget`, `a_budget_is_chosen_per_rpc_and_never_inherited_by_a_sibling` |
| C4 | `remaining: rate.tokens_per_window()` on the create path | 5 tests, incl. `the_entry_point_the_mount_calls_answers_on_its_own_clock` |

**C4 caught a real defect before the crate ever linked:** the bucket-opening
request was not spending a token, so the 101st call in a window was admissible.
v2 falls through from bucket-creation to the charge. A port that is *more*
permissive than v2 at a rate-limit boundary is the worst direction for this file
to be wrong in, and at the integrated gate it would have surfaced as "the limit
sometimes does not fire".

## Coordinator track — agent status (AG1)

| # | Edit | Test that must fail |
|---|---|---|
| C5 | `status_order.rs::accepts`, last line: `update.common.revision > held.common.revision` → `true` | `a_late_report_never_displaces_a_fresh_one` |
| C6 | delete the body of `impl Drop for AgentStatusWaiter` in `status_wait/registry.rs` | `a_released_wait_is_not_leaked_when_the_subscriber_is_gone` |
| C7 | `status_push.rs::classify_transition`: drop `&& next.common.completed_revision > previous.common.completed_revision` from the done arm | `only_a_block_and_a_finished_turn_reach_a_phone` |
| C8 | `rpc_status.rs handle_agent_status_wait`: move `AgentStatusWaitRequest::new` validation ABOVE `require_open_agent_status_session` | `a_wait_never_becomes_an_oracle_for_which_sessions_exist` |
| C9 | `config.rs::set_agent_config`: write `selected.to_owned()` instead of the trimmed-or-OMP fallback | `the_default_agent_is_shared_and_a_blank_selection_falls_back` |

## Coordinator track — firehose adapters (SY1)

Six named, one per property the slice exists to protect. The one the brief names
specifically:

| # | Edit | Test that must fail |
|---|---|---|
| C10 | `sync_ws/feed/mod.rs:131`: `Some(&self.meta)` → `None` | `a_frame_queued_and_then_drained_still_carries_its_meta` |

This is contract §12.8's defect — a queued frame carried no meta, so a
reconnecting client got a frame it could not place and the cursor never advanced.
`FeedFrame` has private fields and a `pub(in crate::sync_ws::feed)` constructor so
a frame cannot leave the directory without its meta; this row is what proves the
type-level guarantee survives the next edit.

## Coordinator track — tasks (S2)

| # | Edit | Test that must fail |
|---|---|---|
| C11 | delete the `publish` call in `handle_tasks_set_state` (line 186) | `a_task_change_reaches_a_sync_subscriber` |

The FAILURE-INDEX regression this file exists to prevent.

**Caveat on a sibling test:** `two_devices_never_claim_the_same_task` is
timing-dependent. Run it at `--test-threads=1` several times; one pass is not
proof.

## Known gaps the gate does not close

- **`claim_ttl_ms` is write-only.** A browser that claims a task and closes leaves
  it claimed forever; the wire advertises a bound no reaper enforces. **v2
  behaves identically**, so this is a pre-existing product gap, not a port
  regression, and no reaper is being written during parity. Recorded here so the
  next person reads "a decision is waiting" rather than "a bug slipped through".
- **`workers::rpc::account_device` omits the `x-roost-auth-layer` marker v2 sets.**
  The workers slice's file; lead-owned at integration.
- **The device-refusal helper is written three times** (S2, A2, and one other).
  De-duplicate at integration; the lead needs the three paths.
- **`new_task_id` lives in `sessions::tasks`** and S1/S3 may have forked it. The
  integration commit hoists it to a shared coordinator id module. It is a
  deterministic composite of process epoch, boot time and a counter — no RNG, and
  no RNG crate is being added for it.
- **§4.11's route count in `docs/phase3-coord-contract.md` is wrong.** The limited
  set is **32**: 31 string literals plus the `PAIR_POLL_METHOD` alias entry, which
  appears as an identifier rather than a quoted string and is therefore missed by
  counting quotes. v2 also has 32. Three numbers have circulated for this one line
  (44 in the doc, 31 and 32 in relays); 32 is the measured one.
- **§4.11's "LRU maintenance by insertion order" is v2's mechanism, not the
  port's.** The port does `buckets.retain(|_, bucket| now < bucket.reset_at)`,
  dropping what has expired and reaching the fail-closed refusal only when the
  survivors alone fill the ceiling. Same guarantee, different mechanism.
