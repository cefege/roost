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

**Two standing rules for running any row on this list.**

1. **Mutate a copy, never the shared worktree.** Copy the file byte-for-byte into
   a scratch directory, mutate the copy, run the real suite against it, re-sync
   afterwards. Two agents in this programme independently mutated in place and
   both paid: one had a read-only audit read a file mid-mutation and report a
   fixed security defect as **absent**, which cost a slice's credibility for
   nothing; the other had a stale poll report three already-restored files as
   still mutated, which cost a cycle and nearly cost a sibling's confidence.
   **The people most likely to mutate in place are the ones most confident they
   will remember to restore**, which is why this is a default rather than good
   practice. A copy is its own pin: there is nothing to coordinate and nothing to
   reconcile.
2. **A mutation proves a test bites. It does not prove the surrounding code
   compiles.** A slice's scratch crate gave `ChannelId` a stand-in carrying a
   `Display` the real type lacks, so ten `%channel_id` sites compiled in the copy
   and would not have compiled in the tree. Mutation evidence and compile
   evidence are different claims, and a gate needs both.
3. **A per-file claim needs evidence produced by a command whose output
   actually contains it.** A slice reported "zero diagnostics in my files" from
   `cargo check … 2>&1 > /tmp/file` — the redirections in the wrong order, so
   stderr (where every diagnostic goes) went to the pipe and the file it then
   grepped held stdout, which for `cargo check` is nearly empty. **The claim was
   a grep over a file that never contained the diagnostics.** The same slice had
   two real errors in its files. `wc -c` on the artifact before believing a grep
   over it costs one second, and an empty artifact is a finding about the
   command, not about the code.

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
| W9 | A read against a replaced grid epoch is REFUSED | `retained_grid.rs describe`, the `EpochBinding::new(...)` argument: replace `record.cell_emit.grid_epoch()` with a constant | `a_read_against_a_replaced_grid_epoch_is_refused` | **OBSERVED** (W2c) |
| W10 | A page over the ceiling is CLAMPED, not refused | `retained_grid.rs describe`, the `total:` field: use `origin` without adding the retained count | `a_page_larger_than_the_ceiling_is_clamped_not_refused` | **OBSERVED** |
| W11 | A row is spelled the way the browser already parses it | `retained_grid.rs cell_span_json`, the `fg` insert: skip it when `span.fg == 0`, which is what proto3 JSON does | `a_row_is_spelled_the_way_the_browser_already_parses_it` | **OBSERVED** |
| W12 | An append advances the offset past what the window kept | `scrollback.rs append_pty_chunk`: do **the forbidden write by hand** — `record.scrollback.append(chunk)` plus a manual `head_seq += record.scrollback.len()` | `an_append_advances_the_offset_past_what_the_window_kept` | **OBSERVED**. **The best row in the file**: the mutation *is* the defect W-0's private `history_floor` exists to make unrepresentable, so this row proves the type-level guarantee still holds. |
| W13 | The unhandled log stops at its cap and says it did | `scrollback.rs record_unhandled`, the cap comparison: `>=` to `>` | `the_unhandled_log_stops_at_its_cap_and_says_it_did` | **OBSERVED** |
| W14 | A replay feeds the whole window and says whether it was saturated | `scrollback.rs replay_retained_into`: hard-code `evicted: false` | `a_replay_feeds_the_whole_window_and_says_whether_it_was_saturated` | **OBSERVED** |
| W15 | An alt-screen toggle split across chunks is recognised once | `scrollback.rs scan_stream_state`, the `mode_carry` assignment: `Vec::new()` | `an_alt_screen_toggle_split_across_chunks_is_recognised_once` | **OBSERVED** |
| W16 | A full page takes more than one slice | `scrollback_read.rs SCROLLBACK_SLICE_ROWS`: raise it to the page ceiling | `a_full_page_takes_more_than_one_slice` | **OBSERVED**. Without this a page silently stalls every other session once instead of in slices. |
| W17 | A read stopped between slices leaves no half-taken page | `scrollback_read.rs walk_page`: move the `continue_read` check inside the inner row loop | `a_read_stopped_between_slices_leaves_no_half_taken_page` | **OBSERVED** |

**The trap in W12, and why the row does not stop it.** W2c reached for the
forbidden write — `record.scrollback.append` plus a manual `head_seq` bump —
**twice**, and the reason was not carelessness and not a naming failure. The
capture lane needs to advance the capability-probe carry without retaining
anything; in v2 that was `answerQueries(rec, null, bytes)`, and in v3
`TerminalCore` has no `write_raw`/`get_response`, so that call does not exist. It
was therefore holding bytes, needed the tokenizer carry advanced, found that the
only record method taking bytes was the *retention* method, and — correctly —
concluded the carry and the offset must advance atomically. The one call that
does that is `append_retained`, so it wanted to reach past it for the other half.
Its own words: the argument is not wrong, which is what makes it dangerous.

So a reader who needs to advance stream state without retaining will conclude
the two must be atomic, find that `append_retained` is the only method that makes
them atomic, and reach past it — feeling rigorous the whole way. **The mutation
row does not prevent that. Only a missing method does**, and the missing method
is `advance_stream_state`, which cannot land until the query tokenizer exists.
Landing it earlier would be a `pub fn` with no production caller, which is the
other thing this repo bans.

What stopped W2c was not the rule. It was noticing that `head_seq` and the floor
move together in one expression, so a second copy of it is a second answer to
"when does the floor move" — a design argument, visible only from inside the
file. The lead has since strengthened `append_retained`'s doc to say it is THE
ONLY method that writes the ring or the offset, every lane's including the
capture lane's. **That is not the fix; the seam is.** What it buys is that the
next slice can tell "the method I want does not exist" from "the method I want
is elsewhere" — the distinction W2c had to work out from first principles.

**The transferable lesson is about how instructions are written, not about this
bug.** A brief that said "never write the ring directly" would have produced
compliance and no understanding, and the missing method would still be missing.
An instruction that says *report the pull, not the rule* is what sent someone
looking for the actual cause. The lead's own hypothesis here was confidently
wrong, and offering it as a likely explanation would have buried the real one.
If a later slice reports the same pull, ask for the cause, not for compliance.

**Sweep for the class, run by the integrator.** The pattern is a range or
window whose upper end is derived from the **element's** size rather than from
the **buffer's** length, checked from one side only. Grepped across every crate
for `len() - x.len()`, `len() - N`, `..= x.len()`, `chunks_exact(` and
`.windows(`. **No third instance.** The two candidates in source are both
correct and worth recording why, because "correct" here means something a
reader has to check rather than something they can see:

- `roost-term/src/row_spans.rs:161` does `open = Some(spans.len() - 1)`
  immediately after `spans.push(...)`, so the vector is provably non-empty.
  The two `open` reads around it use `spans.get` / `get_mut`, not indexing.
- `roost-protocol/.../stun_url.rs:162` and `sdp/candidate.rs:155` index
  `bytes[0]` and `bytes[bytes.len() - 1]`, and `bytes[0]` would panic on an
  empty slice — but `is_dns_label` returns early on `bytes.is_empty()` one
  line above, so the guard exists and the arithmetic under it is right.

Everything else the grep returns is `windows(N).any(...)` in **tests**, which
is the correct use of a sliding window for substring search and is not the
class. The one `chunks_exact(` in the tree is the CLI's own SSH argument walk,
and it is there because the lead's predecessor used `windows(2)` there and got
`(VALUE, "-o")` pairs instead of `-o VALUE` pairs.

**The reason a sweep is worth running even when it finds nothing:** both real
instances were found by *someone using the code* — one by a mutation, one by an
audit — and neither by reading. A careful reader checks that a guard exists and
that its arithmetic is right, and in both defects both of those were true. What
a reader cannot check is whether the range *around* the guard was cut the way
its author meant.

**The pattern is wider than I first wrote it, and a third instance turned up in
the coordinator wave.** The general form is **a range end computed from a
claimed or element-derived size, sitting next to a guard computed from the real
buffer length.** Three instances, all found by someone *using* the code:

1. `roost-keeper` `FrameDecoder::push` — a frame's claimed length bounded from
   above only. **Fixed.**
2. `roost-worker` `stream_scan.rs::find` — `haystack[from..=haystack.len() -
   needle.len()]`, so a needle in the final `needle.len()` bytes was never
   found and an alt-screen toggle ending a chunk went unrecognised. Found by a
   mutation, not a read.
3. `roost-coord` `tests/middleware_support/mod.rs:286` —
   `&rest[chunk_start..chunk_end.min(rest.len())]`, where `chunk_end` came from
   a hex size **the peer wrote in the chunk header**. Clamping the upper end is
   not enough when the lower end is derived from the same claim. Test-only, so
   it could not affect a deployment, but it is the shape. **Fixed.**

**Two refinements, both from the people who found them, and both wider than my
original grep:**

- **`.min(len())` on one end of a range is not a bound** when the other end comes
  from the same claimed size. That is instance 3, and my original pattern would
  not have caught it — it has neither `len() - N` nor `..=`.
- **The combination matters, not either arm.** Instance 2 was caught only by
  `..=` *together with* `len() -`. A reader should keep both.

**And the one that is not about ranges at all**, from the same wave: a peer-
supplied value parsed with `.expect()`. `usize::from_str_radix(..).expect("a
chunk size")` two lines below instance 3 is the same mistake in a different
costume — a parse of untrusted input treated as impossible. In a decode path
that is a panic on a malformed message, and the fix is to end the decode, not to
unwind.

## Findings from the read-only audit of the ported crates

The integrator ran a read-only audit of `roost-term`, `roost-keeper`,
`roost-protocol`, `roost-host` and `roost-observability` for the four classes the
wave's own findings demonstrated: a boundary that assumed it was closed, a
derived `Default` producing a terminal state, a trust decision made by an
omitted call, and an executor that silently inherited its caller's state.

| Finding | Severity | Status |
|---|---|---|
| `FrameDecoder::push` bounds a frame length from above only, so a peer sending `00 00 00 00` gets an empty frame and `frame[0]` panics — unwinding the serve loop and `main`, destroying every PTY on the machine. Four bytes from any same-uid process. v2 refused it with a `RangeError` at `protocol-envelope.ts:114`; the port kept the indexing and lost the check, and `LengthMismatch` was declared for it and never constructed. | **CRITICAL** | **Fixed** on `v3` with a lower bound and a test per short length. |
| The keeper applies `spec.env` verbatim with no admission check, while v2 refused a keeper control key in `isShellSpec` **and** again in `mergeEnvironment`. | CRITICAL | **Open.** A worker-side strip is now real, but v2 refused on both sides and the keeper side is the structural one. |
| The keeper's local endpoint has no capability authentication at all; the socket's `0600` mode is the entire boundary. v2 required a verified capability before dispatching any frame, plus a byte cap, a timer and a connection cap. | HIGH | **Open.** Either fix it or record the deliberate drop in the keeper contract and a commit body. |
| Two implementations of the keeper binary digest disagree, so survivor admission can never succeed. | HIGH | **Open.** The keeper-side one is correct; the worker's is not. |
| `keeper_client.rs:295 wait_for_reply` pulls from `self.events` and **discards any frame that does not match** (`:310`) — and `self.events` is not a reply channel. `client_io.rs:42` routes only `SpawnAck`/`SpawnErr` to the pending-spawn map; `PtyOut` goes into the same channel at `:73`, and `server.rs:259-263` proves the interleaving is real. **Every PTY byte the keeper emits between a client request and its reply is lost permanently, with no log and no counter.** A drag-resize is ~60 requests/second. v2's `keeper-pool-lifecycle.ts:194-303` is one `dispatchFrom` loop routing every frame by tag, with output and reply paths disjoint. | **CRITICAL — data loss** | **FIXED and PROVEN.** `wait_for_reply` and `wait_for_any_input_result` now **defer** a non-answer rather than dropping it, and `next_event` drains the deferred queue **before** the socket in arrival order — a frame deferred now arrived before what is on the wire, so reading the socket first would move the hole to the head of the stream. Poisoned locks recovered, not propagated. Proof: `pty_output_written_before_a_control_ack_is_not_lost` (real client, real socket, real handshake, `PtyOut` flushed before the `ResizeAck`, asserting the **exact bytes** — a liveness check would pass against a client that drops the frame and returns a later one) and `output_deferred_by_a_wait_precedes_whatever_arrived_after_it` (the property the fix introduces). **Mutation: `wait_for_reply` dropping the non-answer again fails both, 0 passed / 2 failed.** |
| `KeeperClient` is missing the client half of six frames its own daemon already serves: `GetHistoryRecords`, `GetHistory`, `GetTerminalState`, `ResizeStatus`, `KillChild`, `Shutdown`/`ShutdownIfEmpty`. The tag table is complete (all 34) and the daemon implements every one, so this is client-only. **A worker restarting against a surviving keeper cannot read the history it is supposed to adopt, nor learn the geometry the keeper applied.** `FAILURE-INDEX.md:354` names the user-visible form: history gone after worker restart, pane freezes. | **CRITICAL** | **Open; blocks worker adoption (W2b/W4).** The `KeeperChannels` seam is right — the client underneath it cannot ask. |
| `resize()` discards the `ResizeAck` payload (the **applied** seq and geometry) and collapses nine reject reasons into `ClientError::Io("the keeper refused the resize of channel N")`. v2's `KeeperResizeResult` distinguishes ack/reject/unknown and `session-terminal-txn.ts:188-222` maps all three. Today `resize()` blocks until its ten-second deadline — **with the discard bug open, that entire deadline is spent losing output.** Two findings compounding on one call site. | HIGH | **Open.** Fixing the discard does not fix this. |
| `TerminalCore` has no `synchronized_output` / `synchronized_output_generation`, so DECSET 2026 withholding cannot be ported and the two rescue ceilings in `stream_fence.rs` are unreachable. **The dangerous workaround is to omit the gate**, which makes every full-screen TUI repaint in visible steps instead of atomically. | HIGH | **Open.** Do not "fix" by always passing. |
| `TerminalCore` has no unhandled-sequence reader. v2 reached *past* its own ABI for one — a `WeakMap` plus a raw memory read at hand-computed offsets — so a port reading only the trait concludes "v2 did not use this", which is wrong. `terminal.unhandled_sequence` therefore has no producer. | MEDIUM | **Open.** Telemetry gap: the "wrong in Roost, fine in iTerm" class is unanswerable. |
| `TerminalCore` has no `get_resource_state`, so `terminal.hyperlink_saturated` has no producer. At the core's fixed OSC 8 table capacity, new links render as dead plain text with nothing logged. | MEDIUM | **Open.** |
| `browser_commands::OWNERS` marks `list-skills` and `git-diff` `Absent`; v2 does the same quietly, and no caller exists. v3 answers `rpc-error` where v2 answered silence. | LOW | **Open.** Harmless today; silence becomes a registered-but-unanswered request if a browser ever sends one. The legacy-drop rule says every dropped path is named in a commit body, and nothing names these two. |

**The question that found all of these, and the reason it works:** *what does a
TypeScript caller reach for that this trait does not offer?* It is a question
about the TypeScript, not the Rust — and it is the only one that can find an
omission, because **a trait cannot be audited for what it omits.** Eleven
surfaces were fully enumerated with **no difference** — `ClientControlFrame` 23
variants 1:1, `SessionEvent` 13 variants 1:1, both link unions a superset of
v2's, all seven brand types with `check`/`TryFrom`/`Display`, all 34 keeper
tags, `CoordConfig` and `roost_host::paths` 1:1 — which is what makes the two
`roost-keeper` defects isolated rather than symptomatic.

It also **narrowed** a finding of mine: `write_raw`/`get_response` is one
omission plus one shape constraint, not three, and the per-chunk `afterChunk`
hook is a latency artifact of the WASM build rather than a semantic
requirement. I would have spent a method on it.

The audit also reported the keeper `env_clear` fix as absent. **That was a
false positive**: the audit read `pty_channel.rs` while a mutation experiment
had the line temporarily replaced, and the committed tree carries
`command.env_clear()` at line 134. Recorded here because the hazard is real —
**a read-only review of a tree that is being mutated will read the mutation.**
Pin reviews to a commit hash, not to a working tree.

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
- **`ShellSpecResolver` has a trait and no implementation.** Declared at
   `session/spawn.rs:94`, consumed by two finished slices at
  `session/lifecycle.rs:179` and `:217` as `Arc<dyn ShellSpecResolver>`. **This
  is not a compile error — it is a seam that looks complete**, and the gate passes
  over it unless something constructs one. A `SessionManager` that cannot resolve
  a shell spec cannot spawn, and that failure surfaces as a browser unable to
  open a terminal, a long way from this trait. Owner: the `W10SpecResolver`
  slice.
- **`TerminalCore` has no `write_raw` or `get_response`,** so the query
  tokenizer cannot be ported, so `advance_stream_state` cannot land, so
  `append_pty_chunk` stays misnamed for what it does. This is the second of the
  wave's "a method is missing and the thing next to it has to be misused instead"
  shape — W2c reached for the retention method because it was the only one taking
  bytes, and reached for it twice. **The first instance of this shape was a
  naming problem and the second is a missing-method problem, and the difference
  matters:** the first is fixable in a header, the second only by porting the
  query tokenizer. Owner: the worker track, at integration.
- **The device-refusal helper is written MORE than three times.** S2's
  correction: ten definitions now exist — seven byte-equivalent and
  marker-bearing (collapse these), and three divergent (each needs a decision,
  not a merge). The real owner is `auth/principal.rs::require_account_device`,
  which cannot build a `ConnectError` and so is not a copy. `terminal_screen/rpc.rs`
  answers `PermissionDenied` where v2's `auth-interceptor.ts:256-271` answers
  `Unauthenticated` with a marker, which a browser reads as "refresh my session" —
  fix that one first.
- **`workers::rpc::account_device` omits the `x-roost-auth-layer` marker v2 sets.**
  The workers slice's file; lead-owned at integration.
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
