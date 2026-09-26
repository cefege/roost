# Phase 4 — the client core contract

The contract `crates/roost-client-core` implements, written before the code so a
reader who has never seen v2 can implement a front end against it.

v2's client is ~45 framework-free files under `apps/web/src/client/` plus the
non-Solid half of `apps/web/src/store/`. That code is the source of truth for
*behaviour* and for the reasons behind it; it is not the source of truth for
*structure*. Where v2 and a spec document disagree, §11 records the
disagreement rather than resolving it silently.

---

## 1. The one hard constraint

`roost-client-core` names no DOM type, no `web-sys` symbol, no `wasm-bindgen`
symbol, and no `tokio` I/O type. The Phase 4 gate is
`cargo build -p roost-client-core --target wasm32-unknown-unknown` with zero
`web-sys` in the dependency tree, *and* a full `cargo test` natively.

The reason is not purity. It is that the same state machine has to drive a
`wasm32` browser, a `tokio` TUI, and a mobile host, and the only shape all
three can share is a synchronous function that takes an event and returns what
should be sent. Every platform capability is therefore either

1. a **trait the core calls into synchronously** (§2), or
2. an **outbound effect the host performs** and reports back as an event (§3).

There is no third option, and in particular there is no "get me an async
transport" trait: an async transport is an effect, and its result is an event.

`grep -rn "web-sys\|wasm-bindgen" crates/roost-client-core/` must return
nothing. That is the acceptance test.

---

## 2. What the host provides: two traits

Both are the whole host surface for *calls the core makes*. Everything else the
core wants is an effect.

### `Clock` — `fn now_ms(&self) -> u64`

Milliseconds, monotonic within a process, arbitrary epoch.

**Why it exists.** Four separate deadlines in v2 read `Date.now()` or
`performance.now()` at the moment they fire: the chunked-snapshot stall sweep
(`apps/web/src/store/terminal-stream-chunks.ts`), the resync retry gate
(`apps/web/src/store/terminal-stream-repair.ts:96-106`), the view lease
renewal (`apps/web/src/store/terminal-stream-view.ts:193-209`), and the held
input admission timeout (`apps/web/src/store/transport/terminal-input-router.ts`).
`roost-protocol` deliberately takes every timestamp as a *parameter* rather than
reading a clock, so that its tests are deterministic — which means the core owns
the one clock read and hands the number in. Without this trait, every deadline
would become untestable or a global.

`now_ms` is read at most once per `handle` call and passed down. Two modules
reading the clock independently is how a deadline starts disagreeing with the
sequence number that justified it.

### `KeyValueStore` — `get` / `set` / `remove` over `&str` keys

The `localStorage`/`IndexedDB` equivalent.

**Why it exists, and why three methods.** The Sync recovery watermark is the
one piece of client state that must outlive a reload: v2 persists
`_lastSeenEventId` under `roost.syncLastEventId` and sends it as `since=` on the
next dial, which is what makes a reconnect a backfill rather than a full
re-hydration (`apps/web/src/store/sync-frame.ts:32-54`). The pairing tab id is
the second key. A TUI wants a file; a mobile host wants preferences; a browser
wants `localStorage`. All three are the same three operations. One method would
be a wrapper, not an interface.

**Writes are debounced by the core, not by the host.** The core latches
`Effect::PersistWatermark` and re-emits it at most once per sweep; the host
performs the write. v2 does the same thing with a private timer
(`_scheduleLastSeenPersist`), and the reason is in the same file: a credential
boundary must be able to *discard* the pending write, which a host-side debounce
could not.

### What is deliberately *not* a trait

The plan named five more. Each is an effect in this implementation, and the
reason is the same in every case: the core never *calls* them, so a trait would
be a second description of something the host already has.

| Named as a trait in the plan | Implemented as | Why |
|---|---|---|
| `RpcTransport` (Connect unary) | `Effect::Rpc(RpcCall)` + `ClientEvent::RpcResult` | A Connect call is asynchronous in every host, including the browser. A trait returning a future would put a runtime type in this crate's public API — exactly what the `crates/README.md` seam forbids. |
| `SyncSocket` | `Effect::DialSync` / `Effect::SendSync` / `Effect::CloseSyncLink` | Same, plus the socket is a *generation*, and generation admission is core state (§7). |
| `DirectCarrier` (loopback WS / WebRTC channels) | `Effect::RequestDirectGrant`, and `ClientEvent::DirectCarrier*` | A carrier is reported, not polled. See §8. |
| `SecureKeyStore` (sign with the device key) | `Effect::SignChallenge`, `ClientEvent::ChallengeSigned` | Signing is async everywhere. It also keeps a crypto dependency out of a crate that must build for `wasm32` with no JS glue. |
| `Notifier` | nothing | The host reads the store after every `handle` call. A notification trait would be a way to be told something the host can already observe, and it is the classic place a front end starts doing work inside the core's stack. |

Two in-memory implementations ship with the crate — `MemoryClock` and
`MemoryKeyValueStore` — because every test needs them and a TUI host needs them
on day one.

---

## 3. `handle_event`: the single entry point

```rust
impl ClientCore {
    pub fn handle(&mut self, event: ClientEvent) -> Vec<Effect>;
}
```

One function. `ClientEvent` is the complete input alphabet (§4); `Effect` is
the complete output alphabet (§5). The host loop is:

```rust
let effects = core.handle(event_from_the_host);
// perform every effect, in order, and feed each result back as a ClientEvent
```

**What `handle` may do.** Read and mutate the store. Read the clock, at most
once. Read the key/value store. Emit `tracing` events at state transitions. Push
effects into the returned vector.

**What `handle` may not do.**

- **No I/O.** No socket, no filesystem, no timer, no `async`. A future host that
  needs to await something must express it as an effect.
- **No encoding.** Effects are *typed* commands, not protobuf bytes. The host
  owns the wire encoding, because the encoding is what changes with the
  protocol while the state machine does not. (`roost-proto` is a dependency of
  this crate only for the two cell message types the chunk assembler itself
  requires — §6.)
- **No `unwrap` / `expect`** anywhere outside `#[cfg(test)]`.
- **No second fold.** Session projection delegates to
  `roost_protocol::wire::fold_event` and never re-decides what an event means.
  See §9 and the guard in `docs/FAILURE-INDEX.md:27`.
- **No global state.** Everything lives in the `Store` the core owns. A
  document-scoped registry in v2 becomes a struct field here, which is what
  makes two `ClientCore`s in one test process legal.

**Effects are ordered and must be performed in order.** Two effects in one
return value are a decision with a sequence (a `DialSync` before the
`Subscribe` that rides on it), not a set.

---

## 4. The event set

Every variant names where it came from and what v2 it replaces.

### Transport observations

| Event | Replaces | Meaning |
|---|---|---|
| `SessionEventReceived(SessionEvent)` | `apps/web/src/store/sync-frame.ts` `sessionEvent` case | One metadata-plane event. Folds through the shared `fold_event`. |
| `SessionsSnapshot(BTreeMap<SessionId, Session>)` | `applySessionsSnapshot` | An authoritative full session set: bootstrap, or a reconnect's fresh domain generation. |
| `SyncLinkOpened(SyncLinkOpened)` | `_installLiveSyncLink` | A socket is open and the coordinator assigned it a scope and `socket_id`. |
| `SyncLinkClosed { generation, reason }` | `_clearLiveSyncLink` / `_closeFailedSyncLink` | The socket this generation owns is gone. |
| `SyncSubscribed { generation, socket_id, domains }` | `waitForSyncSubscribed` | The v2 announcement. |
| `SyncFrameReceived(SyncFrame)` | `_dispatchSyncFrame` | One decoded application or control frame. |
| `CellGridFrameReceived { owner, frame }` | `dispatchTerminalCellFrame` | One non-chunked authoritative cell frame. |
| `CellGridChunkReceived { owner, chunk }` | `dispatchTerminalCellChunk` | One part of a chunked baseline. |
| `DomainReady { generation, domain, snapshot_token }` | `sync-domain-state.ts` | The snapshot/live gap is closed for one domain. |
| `ViewStateReceived { owner, state }` | `dispatchTerminalViewState` | A generation-matched view-state result. |
| `InputResultReceived { owner, input_seq, outcome }` | `settleTerminalInput` | Truthful write result for one admitted batch. |
| `RpcResult { call_id, result }` | Connect client callbacks | One unary response. |
| `ChallengeSigned { .. }` | auth ceremony | The host signed. |

### Front-end intent

| Event | Replaces | Meaning |
|---|---|---|
| `ViewOpened { session_id, worker_fp, view_id, cols, rows }` | `createTerminalView` | A pane attached. Registers a lease and a geometry demand. |
| `ViewResized { session_id, view_id, cols, rows }` | view handle resize | An effective-size change; forces a fresh stream expectation. |
| `ViewHidden { session_id, view_id }` | explicit hide | Removes the view immediately — no lease wait. |
| `ViewClosed { session_id, view_id }` | pane close | Removes the view and any input hold it owned. |
| `TerminalInput { session_id, view_id, bytes }` | `admitTerminalInput` | Bytes from a keyboard. The core decides the route. |
| `CarrierReady { connection }` | `terminalDirectRegistry.register` | A loopback or WebRTC carrier authenticated. |
| `CarrierLost { connection_id, reason }` | `#unregister` | It closed or was displaced. |
| `SearchPageReceived { .. }` | `decodePageMatches` | One coordinator scrollback-search page. |

### Time

| Event | Replaces | Meaning |
|---|---|---|
| `Sweep { now_ms }` | every `setTimeout` in the terminal store | One pass over every deadline: chunk stall, resync retry, view lease, held-input timeout. |

`Sweep` exists so the core owns no timer. A host with one repeating interval can
drive every deadline in the client. A host with none cannot silently skip one:
`now_ms` is the clock read, passed in, so the sweep is pure.

---

## 5. The effect set

| Effect | Meaning |
|---|---|
| `DialSync(SyncDial)` | Open `/ws/coord-sync` with `roost-auth`, `flow=1&sync_v=2`, `tab`, and `since`. The core names the contract; the host owns the origin. |
| `CloseSyncLink { generation, reason }` | Close the socket this generation owns. |
| `SendSync(SyncCommand)` | One typed client frame: `Ack`, `Subscribe`, `Unsubscribe`, `DomainReady`, `TerminalView`, `TerminalResync`, `TerminalInput`, `ProbeResult`. |
| `Rpc(RpcCall)` | One typed Connect unary call. |
| `SignChallenge { .. }` | Ask the host to sign with the device key. |
| `PersistWatermark { event_id }` | Write the Sync recovery watermark. Debounced by the core. |
| `RequestDirectGrant { session_id, worker_fp, tab_id }` | Ask the coordinator for a time-bounded, memory-only direct-terminal grant. |

There is deliberately no `Notify`, no `Present`, and no `Repaint`: the host reads
the store.

---

## 6. The cell stream: the rules that are the point

This is where a weakened port loses terminal output, so every rule below has a
named test. The v2 originals are `apps/web/src/client/terminal-stream/terminal-stream-frame-fold.ts`
and `apps/web/src/store/terminal-stream-replica.ts`; the normative statements
are `protocol/spec/terminal-stream.md:22-30`.

### 6.1 Stream expectation, and what a change to it invalidates

A `TerminalSession` holds an *expectation*: `expected_stream_id`,
`effective_cols`, `effective_rows`. `install_expected_stream` writes all three
together (`apps/web/src/store/terminal-stream-replica.ts:75-102`):

- A change in stream id clears liveness, sets `requires_fresh_baseline`, and
  drops any chunked transfer in flight.
- `baseline_ready` is then recomputed as
  `!requires_fresh_baseline && canonical.is_some() && canonical.stream_id == new id && canonical.cols == cols && canonical.rows == rows`.

A new stream id is minted routinely — the view authority mints one for a first
view, an effective-size change, a last-view disable/re-enable, a worker
replacement, or a retry (`protocol/spec/terminal-stream.md:24`). A client that
keeps accepting deltas across a stream change is splicing two grids.

### 6.2 Full-before-delta

**Invariant.** A delta is admissible only when a complete authoritative full
for the *current* stream is installed.

`fold_terminal_delta` refuses a delta when `baseline_ready` is false or
`canonical` is absent (`terminal-stream-frame-fold.ts:129-153`). That is the
whole rule; everything else in that function narrows it further.

A delta before any full is therefore not a "first delta" — it is a refusal, and
a refusal latches exactly one snapshot request (§6.5).

**Invariant.** A full is admissible only when it is a *complete* baseline for the
current expectation. `valid_terminal_full` requires all of
(`terminal-stream-frame-fold.ts:72-97`):

- `full == true`
- `base_seq == 0` — a full is not an extension of anything
- `stream_id == expected_stream_id`
- `cols == effective_cols` and `rows == effective_rows` — the pane's geometry,
  not merely the sender's
- `viewport_rows.len() == rows`, and row `i` has `index == i` for every
  `i in 0..rows`
- `scrollback_append` is empty — a full is not also an append
- `scrollback_rows` are contiguous from `sb_base`, and the last one is
  `scrollback_total - 1`

**Invariant.** A full never goes backwards.
`terminal_full_follows_canonical` (`:99-113`) refuses a full whose `seq` is
below the installed `seq`, and refuses a full that re-states the same `seq` with
different `grid_epoch`, `cols`, `rows`, or `alt_screen`. This is the
late-repair case: a snapshot request issued on generation A can still be
answered on generation B, and answering it must not undo B.

**The trap, named.** `roost_protocol::cell::apply_delta` accepts a frame with
`full == true` and replaces its base wholesale
(`crates/roost-protocol/src/cell/diff_grid.rs:74-77`). That is correct for the
server-side emitter, which has no canonical to protect. In the client it is a
loaded gun: fold on `frame.full` *first*, exactly as v2 does, so a stale full is
refused by `terminal_full_follows_canonical` before it can reach
`apply_delta`. A client that folds by calling `apply_delta` and then checking
accepts out-of-order baselines. `tests/terminal_full_before_delta.rs` pins the
ordering by feeding a stale full through the public entry point.

### 6.3 The delta fence

A delta is admitted only when **all** of these hold
(`terminal-stream-frame-fold.ts:129-153`, plus
`crates/roost-protocol/src/cell/diff_grid.rs:79-90`):

| Check | Why |
|---|---|
| `baseline_ready` and a canonical frame exist | §6.2 |
| no chunked snapshot is in flight (`active_snapshot_id().is_none()`) | A delta arriving mid-baseline belongs to the frame the partial is rebuilding. Folding it onto the old canonical and then completing the snapshot would publish a grid that is neither. |
| `stream_id == expected_stream_id` | §6.1 |
| `base.stream_id == expected_stream_id` | The installed baseline belongs to this stream. |
| `delta.grid_epoch == base.grid_epoch` | **The epoch fence.** The grid epoch is the worker's opaque grid-numbering identity; a resize mints a new one. A delta from a previous epoch indexes a grid that no longer exists, so it is refused before any row is read. |
| `delta.cols == effective_cols` and `delta.rows == effective_rows` | The pane's current geometry, per `protocol/spec/terminal-stream.md:26`. |
| `delta.base_seq == base.seq` | The delta's declared base is the frame it was computed against. |
| `delta.seq == delta.base_seq + 1` | **Exact successor.** A gap is not a fast path. |
| `delta.alt_screen == base.alt_screen` | Alt-screen occupancy changes the row numbering; `apply_delta` refuses it too. |
| `base.viewport_rows.len() == base.rows` | The base is itself well-formed. |
| `delta.scrollback_rows` is empty | A delta appends, it does not restate history. |
| every `delta.viewport_rows[i].index` is in range and appears once | A duplicate index would make the "overwrite by index" fold order-dependent. |

**Any one failure invalidates the cursor and latches one snapshot request**
(`protocol/spec/terminal-stream.md:27`). "Invalidates the cursor" means the
canonical frame is left exactly as it was — not partially advanced — so the next
full is the first thing that moves the grid.

**The table has TWO enforcement points, and which one fires is worth knowing.**
Four of the rows above — the exact-successor `seq`, a `base_seq` that is not the
installed sequence, a duplicate or out-of-range row index, and `scrollback_rows`
on a delta — are already refused UPSTREAM, by `roost_protocol::cell`'s decoder
(`proto_to_cell_frame`), before this crate's fold ever sees the frame. The
remaining rows are the ones only the client's fold can catch: the baseline rule,
the chunk-in-flight rule, both stream rows, the epoch fence, the pane geometry,
and alt-screen occupancy.

Consequence for anyone changing this: a test that asserts only "it was refused"
passes even with the client's own row deleted, because the decoder caught it
instead. `tests/terminal_epoch_fence.rs` therefore has two helpers —
`refuse` (stage-agnostic, for the four decoder-enforced rows) and
`refuse_at_the_client_fence` (insists the reason is `delta_unfollowed`, the
client's own vocabulary, rather than `delta_fold_rejected` or a decode
diagnosis). Each of the client's seven rows is mutation-verified: deleting the
row makes its test fail.

`tests/terminal_epoch_fence.rs` fails if any single row of that table is
weakened: it drives the public entry point with a frame that satisfies every
other row.

### 6.4 Chunked baselines

A baseline larger than `CELL_GRID_PART_MAX_BYTES` (1 MiB,
`protocol/spec/terminal-stream.md:36`) arrives as `PbCellGridChunk` parts
assembled by `roost_protocol::cell::CellGridChunkAssembler`
(`crates/roost-protocol/src/cell/frame_chunk_assembler.rs:129-141`).

The client's three rules:

1. **A part is admitted only for the session and stream the replica expects.**
   A part naming another stream is dropped without touching the assembler.
2. **Any rejection resets the assembler and latches a resync.** The assembler
   already resets itself on error; the client additionally invalidates the
   chunk-transfer state and requests a fresh baseline, because a partial that
   was refused for *ordering* cannot be completed by a later part.
3. **The part-size ceiling applies to a *wire* frame, never to an assembled
   one.** `decodeTerminalWireFrame` skips the `CELL_GRID_PART_MAX_BYTES` check
   when the frame came from the assembler (`terminal-stream-frame-fold.ts:44-60`),
   because a chunked baseline is *supposed* to exceed one part. Re-applying the
   ceiling to the assembled product would refuse every large terminal.

The whole of `protocol/conformance/cell-chunks/` is to be replayed through the
client's own chunk path in `tests/terminal_chunk_conformance.rs`, so the client
and the shared assembler cannot drift. That test is specified in §14 and is NOT
yet written — the rules above are currently pinned only by
`tests/terminal_epoch_fence.rs`, which drives one real chunked transfer through
the assembler rather than the whole vector family.

Stall: a partial that has not advanced for `CELL_GRID_CHUNK_STALL_MS` (10 s,
`protocol/spec/terminal-stream.md:42`) is dropped and latches a resync. The
sweep that does this is the same `Sweep` that renews leases.

### 6.5 The repair latch

One latch per session (`apps/web/src/store/terminal-stream-repair.ts`):

- Any invalid delta, invalid full, chunk rejection, or session mismatch sets
  `resync_latched` **once**. A second refusal while latched does not queue a
  second request; it is the same gap.
- The latch is cleared only by an accepted **full**. An accepted delta proves
  the lane, not the gap, so it does not clear it.
- The send is rate-limited to one per `TERMINAL_VIEW_HEARTBEAT_MS` (5 s,
  `crates/roost-protocol/src/viewport.rs:20`) *per generation*. A generation
  change re-arms it, because the new socket has never been asked.
- The latch is generation-scoped: a resync requested on generation A is not sent
  on generation B, and a frame accepted on generation A never repairs
  generation B.

The escalation beyond a re-request is deliberately **not** in the client. v2's
`beginTerminalScopedRepair` / proof-challenge ladder and the coordinator's
two-attempt `requestFreshStream` escalation are server-side authority
(`docs/FAILURE-INDEX.md:1085` is explicit that the coordinator is the only party
that knows which stream it expects). The client latches once and re-requests on
the heartbeat; inventing a client-side escalation would give the client an
opinion about a stream only the authority minted.

---

## 7. Sync: the state machine

`protocol/spec/sync.md:23-31` is normative. The client-side rules, and what v2
does that the spec does not say:

**Socket generation.** Every dial allocates a new generation
(`apps/web/src/store/sync-link-state.ts:95-99`). A callback, a timer, or a
pending frame that names an older generation is inert. The generation is part of
the terminal generation token, so a socket change invalidates every in-flight
terminal repair.

**Dispatch is not generation-gated; the ACK is.**
`dispatchSyncFrameCausally` (`apps/web/src/client/sync/sync-flow.ts:48-65`)
applies a frame that a live callback already accepted, *or* one retained by the
pre-hydration queue, regardless of generation — "reconnecting cannot revoke a
frame that was accepted earlier". Only the cumulative ACK is gated to the
still-current, accepting, open owner. Rationale: revoking an applied frame
because the socket redialled mid-batch would strand the state the frame already
changed, and the coordinator's replay would not re-send it.

**Flow control.** Every application frame on a negotiated socket has a positive
monotonic `delivery_seq`; controls carry `0` and never consume the window
(`protocol/spec/sync.md:29`). After a *synchronous* dispatch of a frame with
`delivery_seq > 0`, the client sends the cumulative
`Ack { ack_delivery_seq, socket_id }`. The client must never ACK a frame it did
not apply, and must never ACK a control.

**Domain generations.** A v2 socket receives `SyncSubscribed`, then the client
subscribes to exact domains. A domain's retained snapshot must precede its live
frames, and `domain_ready` closes that gap. A terminal domain's hydration
requires the one-time `SessionsList` snapshot token; a `domain_ready` without a
current token resets the domain with `snapshot_token_invalid`
(`protocol/spec/sync.md:28,50`).

**Recovery watermark.** `last_seen_event_id` advances inside the same dispatch
that folds the event, is sent as `since=` on the next dial, and is discarded at
a credential boundary — a persisted global cursor would skip the next socket's
initial history.

**Redial reasons** that redial immediately rather than backing off:
`visibility`, `manual`, `stale`, `flow`, `terminal-liveness`
(`apps/web/src/client/sync/sync-flow.ts:72-78`). A close with code `1013` and
reason `sync backpressure` is a coordinator verdict, not a network blip.

**Auth revocation** is close code `4001`
(`apps/web/src/store/sync.ts:102`) and is terminal for the socket — it does not
redial, because a redial would present the same rejected credential.

---

## 8. Direct carriers and route election

`protocol/spec/direct-terminal.md`. Preference is same-worker loopback, then a
qualified authenticated WebRTC peer, then ready Sync.

**A candidate has no canonical effect until it has a complete validated
baseline and its promotion commits** (`protocol/spec/direct-terminal.md:27`,
`protocol/spec/terminal-stream.md:28`). The browser keeps one canonical replica
and folds a direct candidate *separately* until promotion. In the core that is a
second `TerminalSession` under a candidate key, not a second canonical.

`commit_session_promotion` is refused unless **all** of
(`apps/web/src/store/terminal-stream-transport.ts:269-306`):

- the registry is not resetting
- the prepared attempt id is still the current attempt
- the connection is still registered for its worker, as active or candidate
- there is live view demand for this session on that worker
- the connection's own token equals the prepared token, exactly
- the token the caller believed was current still equals the old token
- the connection's grant still admits this session
- if another connection is already active for the worker, this one is loopback

Only then is the candidate's canonical applied, demand views migrated, and the
route recorded. The ordering is load-bearing: candidate folding owns its own
state, but no listener runs until every owner is current.

**Promotion of the worker-level slot**: a candidate becomes active only when it
holds at least one route and the current active holds none
(`:360-372`). This is why retiring one connection can promote the other without
a renegotiation.

**On route loss** (`protocol/spec/direct-terminal.md:29`): the browser retires
*only that token*, stops candidate work, and repairs from a fresh Sync baseline
plus a worker input-route claim when available. Painted rows stay. It never
replays accepted or ambiguous input. If neither direct nor Sync is ready, new
input is rejected rather than buffered against a route that no longer exists.

---

## 9. Input lanes and the route-claim fence

`protocol/spec/direct-terminal.md:27,29` and `docs/FAILURE-INDEX.md:1503`.

**Caps** (`apps/web/src/client/carriers/terminal-input-lanes.ts:11-14`), all
carried with their reasons:

| Constant | Value | What breaks past it |
|---|---|---|
| `MAX_INPUT_BYTES` | 64 KiB | A single batch larger than the peer frame ceiling can never be written, so the lane would hold bytes that can only ever be rejected. |
| `MAX_PENDING_INPUTS_PER_SESSION` | 200 | A stuck route turns a held lane into an unbounded queue. |
| `MAX_PENDING_INPUT_BYTES_PER_SESSION` | 256 KiB | The byte cap, for the same reason with paste-sized input. |
| `INPUT_RESULT_TIMEOUT_MS` | 10 s | A batch whose result never arrives is resolved `ambiguous`, not left pending forever. |

**The outcome vocabulary is three-valued and the third one matters.**

- `accepted` — written, with `written_bytes`.
- `rejected` — refused, `written_bytes == 0`, and **never retried**.
- `ambiguous` — the send may or may not have reached the PTY. **Never retried,
  never replayed** (`protocol/spec/direct-terminal.md:29`). This is the whole
  reason the vocabulary exists: a retried ambiguous batch is a doubled
  keystroke in someone's shell.

**Holding.** When the route is reconnecting (`holding`) or claiming
(`claiming`), a batch is admitted and *held*, unsent, with an admission
timeout. It is released only when a destination is presented; if the timeout
fires first it is `rejected` — refused, not sent, so nothing can double.

**The route claim.** When the worker advertises `terminal-input-route-v1`, the
client claims the route before its first write and sends
`input_route_epoch` with the batch. Only the *acknowledged* epoch is installed.
The revision counter is `u64`; the ceiling is `i64::MAX` because the wire field
is a signed 64-bit revision (`apps/web/src/store/transport/terminal-input-route-claim.ts:14`).

**Retirement fencing.** When a destination retires, a batch that had *started*
settles `ambiguous` ("the batch will not be retried"), and one that had *not*
started settles `rejected` ("before input was sent"). The distinction is the
only thing standing between a redial and a duplicated paste.

---

## 10. Session projection

One rule, and it is a `FAILURE-INDEX` entry: **the client never hand-mirrors the
event fold** (`docs/FAILURE-INDEX.md:27`).

```rust
self.sessions = fold_event(&self.sessions, &event);
```

`fold_event` never mutates its input and returns the whole next map, so
assignment is the entire projection. Only `SessionEvent::Closed` removes a
session, which is what makes an out-of-order or stale event harmless: it cannot
prune a live session as a side effect. The v2 Solid store needed a per-key diff
because a whole-`Record` `setStore` silently no-ops on a subtree; a plain
`BTreeMap` has no such hazard, so the diff is the host's problem and not the
core's.

`tests/session_projection_fold.rs` asserts the client's projection is
reference-equal to `roost_protocol::wire::fold_all` over the same event list —
the Rust half of the existing v2 guard.

---

## 11. Where v2 and the spec documents disagree

Recorded, not resolved. Each is a place where the code and
`protocol/spec/*.md` say different things, and a future implementer needs to
know which one they are reading.

1. **`terminal-stream.md:27` omits the chunk-in-flight refusal.** The spec says
   a delta is accepted when stream id, epoch, dimensions, `base_seq` and
   successor `seq` match. v2 additionally refuses a delta while
   `assembler.activeSnapshotId !== null`
   (`terminal-stream-frame-fold.ts:138`). The code is right: a delta that
   arrives mid-baseline belongs to the frame the partial is rebuilding, and
   folding it onto the old canonical before the snapshot completes publishes a
   grid that is neither the old one nor the new one. **This crate implements the
   code's rule and this is the spec's omission.**

2. **`terminal-stream.md:26` puts geometry in the view authority; the code also
   fences the client.** The spec says the authority computes "independent-axis
   minimum geometry" and that resize forces a new full. v2 additionally refuses
   a frame whose `cols`/`rows` do not equal the *pane's* effective geometry
   (`terminal-stream-frame-fold.ts:85-86,142-143`). A client that accepted the
   authority's geometry unconditionally would paint a grid the pane cannot show
   and would then keep accepting deltas against the wrong `rows` count. **This
   crate implements both fences.**

3. **`sync.md:29` reads as though the whole frame is socket-scoped.** It says
   the client sends the cumulative ACK "after synchronous client dispatch".
   v2 decouples the two: dispatch is explicitly *not* generation-gated
   (`sync-flow.ts:48-65`). The spec is silent on which happens first, and a
   reader could reasonably build either. **This crate implements the code's
   split**, because revoking an applied frame on redial strands state the
   coordinator will not replay.

4. **`direct-terminal.md:27` describes holding as conditional on a capability;
   v2 holds before the capability is known.** The spec says "if
   `terminal-input-route-v1` is present, unsent input remains held until the
   worker acknowledges the exact route claim; older loopback retains its
   established no-replay behavior." v2's router holds in the `holding` phase
   regardless, and the *claim* is what the capability gates
   (`terminal-input-router.ts:196-204`). **This crate implements the code's
   behaviour**: hold while the route is unknown, claim only when supported.
   The spec sentence reads as if holding is itself capability-gated.

5. **`apply_delta`'s contract is wider than the client's needs.** The shared
   helper accepts `full == true` and replaces its base
   (`crates/roost-protocol/src/cell/diff_grid.rs:74-77`), which is right for
   the emitter and wrong for a replica. The spec (§6.2 above) has no say on
   this because the spec describes the *client rule*, not the helper's shape.
   **This crate orders the fold on `full` first, before the helper is reached.**

---

## 12. Where the plan and this implementation disagree

The plan (Phase 4, item 1) lists seven platform traits: `Clock`/`Timers`,
`KeyValueStore`, `SecureKeyStore`, `RpcTransport`, `SyncSocket`, `DirectCarrier`,
`Notifier`. This crate ships **two** (`Clock`, `KeyValueStore`) and expresses the
other five as effects and events. The reasoning is in §2; the short version is
that a trait the core never calls is a second description of something the host
already owns, and `RpcTransport`/`SyncSocket` in particular would have put a
runtime type in this crate's public API, which is the exact thing
`crates/README.md:49-53` says this crate exists to prevent.

`Timers` is also not a trait: the core owns no timer. `Sweep { now_ms }` makes
every deadline a pure function of a timestamp the host supplies, so a host with
no timer support still cannot silently skip a deadline — it simply cannot run
one.

---

## 13. What this crate does not do

- It does not render. `roost-web-terminal` owns the DOM.
- It does not encode protobuf. Effects are typed; the host encodes.
- It does not open a socket, run a timer, or touch a file.
- It does not fold a session event. `roost-protocol` owns that.
- It does not assemble a chunk. `roost-protocol` owns that; the client owns when
  to feed it and what to do when it refuses.
- It does not mint stream ids. The view authority owns that.

---

## 14. Test map

Landed:

| Test | Pins |
|---|---|
| `tests/terminal_epoch_fence.rs` | Every row of the §6.3 table, one weakened rule at a time, split by which enforcement point owns it (see §6.3); plus "a refused frame leaves the replica byte-identical", and the two cases that reach a row no other row masks — the stream splice and the geometry reflow. |
| `tests/terminal_full_before_delta.rs` | §6.2: no delta before a full; a full must be complete for the current expectation; the `apply_delta` ordering trap; one gap latches exactly one repair; only a full clears it; a new stream drops both. |
| `tests/support/mod.rs` | Hand-built protobuf cell frames, so each rule can be broken in isolation. |

Specified but not yet written — each is named here so the omission is a tracked
gap rather than a silent one:

| Test | Pins |
|---|---|
| `tests/terminal_chunk_conformance.rs` | Every `protocol/conformance/cell-chunks/` vector through the client's own chunk path. |
| `tests/sync_flow_control.rs` | §7: generation gating, dispatch-not-gated, cumulative ACK, controls excluded. |
| `tests/route_election.rs` | §8: every promotion precondition, loopback precedence, candidate-to-active, route loss. |
| `tests/input_lane_fencing.rs` | §9: caps, hold/release/timeout, never-replay for `ambiguous`, retirement fencing. |
| `tests/session_projection_fold.rs` | §10: the projection equals the shared fold (in-crate unit tests cover the fold itself). |
| `tests/find_page_validation.rs` | Coordinator search pages: contiguous windows, matches inside the window, continuation direction. |
| `tests/history_range_arithmetic.rs` | Absolute scrollback interval arithmetic a pager needs. |
| `tests/core_without_a_browser.rs` | The claim of §1 end to end, and the `SyncDomain` wire numbers against `sync.proto`. |
