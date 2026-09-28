# Slice 1 of 2: the client-side decode entry point in `roost-client-core`

Buildless scoping, at `v3-web@43c1a5b7`. Companion to
`docs/v3-handoff/roost-web-pump-slice.md` (slice 2, the pump). The failure class this
slice is an instance of is recorded at `docs/v3-handoff/silent-no-ops.md`.

## The finding that sizes it

**There is no client-side protobuf decode anywhere in the workspace.**

- `FirehoseFrame` appears in `roost-client-core` in **one place, and it is a
  comment** — `sync/inbound.rs:5`. Zero code references.
- No `From<FirehoseFrame> for SyncFrame`, no `impl From<Pb…>`, no decode on a sync
  frame. (The two `::decode(` hits are `AgentSeenLedger::decode` — local storage.)
- The real frame handling is **`roost-coord/src/sync_ws/`** — the server side.

This is **unwritten, not unwired**. `sync_socket.rs` is a missing *call*; this is
a missing *implementation*, and only the second needs designing.

## CORRECTION 1 — the codec is `buffa`, not prost

`roost-coord/src/sync_ws/retained_frame.rs:20-21` imports
`roost_proto::__buffa::oneof::firehose_frame::Frame` and `roost_proto::buffa::Message`.
`roost-proto/Cargo.toml:18` says it outright: *"the shipped tree contains buffa
alone"* — `prost-types` is there for types, not for the codec.
**`roost-client-core` depends on neither.**

So the decode is `roost_proto::buffa::…`, reached through `roost-proto` (which
`roost-client-core` already may depend on, `xtask/src/crate_dag.rs:114-117`) —
**no new dependency and no DAG change.** An earlier draft of this file said prost;
that was wrong, and reading the coordinator is what corrected it.

## CORRECTION 2 — the authoritative arm list is what coord CONSTRUCTS, not the 27 in the proto

`grep -rhoE 'Frame::[A-Z][A-Za-z]*' crates/roost-coord/src/` gives the arms the
coordinator actually builds. That is a better list than the proto's, because the
proto describes what *may* be sent and this is what *is*.

Separating it out: `Full`/`Chunk` are coord's internal `SharedCellFrame`, and
`CancelScrollbackSearch`, `SearchScrollback`, `RespawnIfMissing`, `Kill`,
`GetScrollbackCells` are **client → server** commands from `commands.rs` — not
inbound. The remaining **27 match the proto's 27 exactly**, including
`Frame::Sessions` (2 sites).

**Which settles the one open question.** `SyncFrame::SessionsSnapshot` has no
named arm, and `sessions = 1` is `JsonEvent` — *"legacy fallback: any kind not
yet in session_event_proto"*. The coordinator **does construct `Frame::Sessions`**,
so the snapshot arrives through that arm, which means **one arm is a JSON decode
nested inside a protobuf one.** That is a second wire format arriving through
the first, and it deserves its own decision rather than being discovered
mid-implementation. It is now *located*; it is not yet *designed*.

(`project_snapshot_sessions` on the coord side is a DB projection write,
`events/projection_writes.rs:95` — correctly ruled out as the answer.)

## The mapping

| arms | becomes |
|---|---|
| `Subscribed` | `Subscribed` |
| `DomainReset` | `DomainReset` |
| `SessionEvent`, `SessionPresence` | `SessionEvent` |
| `Sessions` (JsonEvent) | `SessionsSnapshot` **or** `SessionEvent`, by kind — **nested JSON, undesigned** |
| `CellGrid`, `CellGridChunk` | `CellGrid`, `CellGridChunk` |
| `TerminalViewState` | `ViewState` |
| `InputAccepted`, `InputRejected`, `InputAmbiguous` | `InputResult` (`InputOutcome` picks) |
| `AgentStatus` | `AgentStatus` |
| `Keepalive` | `Keepalive` |
| **14 arms, every one of which the coordinator demonstrably constructs** | `Unknown { field }` |

**The fold is not theoretical.** All 14 — `AuditRow`, `WorkspaceDelta`,
`TaskDelta`, `McpMsg`, `WorkerPresence`, `WorkerRoutable`, `TerminalTitle`,
`LastActivity`, `PairRequestDelta`, `UiState`, `UiCommand`,
`CoordinatorRelocation`, `InputRouteResult`, `TerminalTransportProbeResult` —
appear in coord's constructed list. None is dead proto. Folding is correct for a
first slice and wrong for a finished client, and `UiState`, `LastActivity`,
`PairRequestDelta`, `CoordinatorRelocation` and `TerminalTitle` are arms a
complete web client acts on. **The slice should carry the ignore list as a
product decision with a name on it**, checkable against the proto.

The precedent for the cost is `inbound.rs:111-118`: without `AgentStatus`, every
report decoded as `Unknown { field: 29 }` — "sequenced and acknowledged, and
applied to nothing."

## The rule that must not regress to prose

`client/sync/frame.rs:1-19`: a frame reaching the queue with **no** meta cannot
be acknowledged, cannot be placed, the recovery cursor stops where the queue
began, and a reconnect never recovers. The meta is built **once, in the
constructor**, from socket-carried values (`SyncFrameMeta::new(generation,
delivery_seq, socket_id, frame)`, `frame.rs:56-61`) — never defaulted, never
reconstructed from the frame's own shape — and a frame arriving without it is
**refused at the door**, not queued unplaceable.

## `DomainReady` is constructed, not decoded — and has NO wire source at all

This section replaces an earlier, weaker claim of mine ("derived from the reply"),
which reading `handle_domain_ready` overturned.

**There is no reply.** `roost-coord/src/sync_ws/commands.rs:196-243`:

- `ready.domain.as_known()` else `Invalid` — a domain this build does not know is
  a protocol violation, and answering `Nothing` "would leave it believing the
  fence closed".
- `!state.subscribed || ready.generation != state.generation || state.ready` ->
  `Nothing`.
- For the **terminal** domain it consumes a **one-time** token; a token that does
  not check out calls `reset_terminal(session, "snapshot_token_invalid")`
  (`:221, :224`) — it **resets the domain** rather than ignoring the bad token,
  because a client that believes the terminal domain is hydrated and is not would
  otherwise receive cells for sessions it never learned about.
- Then `state.ready = true`, `session.request_flush()`, and
  `CommandOutcome::DomainReady { .. }` — a **local** outcome, not a wire send.

So the client learns a domain is ready because **that domain's frames start
arriving**, not from any frame announcing it. The only wire-visible failure is
`DomainReset { reason: "snapshot_token_invalid" }`, which *is* decodable — and
`SyncFrame::DomainReset` already carries `reason`.

**The consequence, and it is a tenth instance of the silent no-op class:**
`SyncFrame::DomainReady` has **four consumers in production code**
(`handle_sync.rs:38`, `sync/domain.rs:158` and `:169`,
`handle_sync/apply_frame.rs:34`) and **exactly one producer in the entire
workspace: a test helper** (`tests/support/sync_reconnect.rs:77`). Nothing in
production can emit it. **Three production code paths handle an event that
nothing can produce, and a test that constructs it by hand is what makes them
look covered.**

**This also corrects my third named test.** It cannot assert that the token and
generation "land where `snapshot_registry` expects" — `snapshot_registry.rs` is a
**server-side** registry and the client cannot see it. The testable properties are
the client's own: that `DomainReady` is built from the command the client sent
(its domain, generation and token), and that a `DomainReset` carrying
`"snapshot_token_invalid"` clears that readiness. A decode grown from the wire
shape will never produce it, which is the trap — see
`docs/v3-handoff/silent-no-ops.md` §8 and §10.

## Named tests this slice owes

1. **Refuse, do not default** — a frame with no placeable meta never enters the
   queue. Invisible until a user reconnects, therefore not a comment.
2. **The proto/coord divergence test** — I treat `sync.proto` as the wire and
   `sync_ws/` as the worked example, coord winning if they disagree. That is a
   judgement and should be a property: a decode written from the proto alone
   silently accepts arms the coordinator never sends, or drops ones it does.
3. **`DomainReady` is constructible** — construct it and assert the token and
   generation land where `snapshot_registry` expects. Fails if the decode ever
   grows a oneof arm for it.

## Why this half goes first in a single slot

It is what the pump has nothing to call without, it owns the named meta trap, and
**it is pure decode-and-map over bytes — testable natively, with no browser, no
wasm target, no Playwright.** While Track U yields a build slot it is the only
part of the slice verifiable at all.

## Not claimed

No compiler was run. Unverified: that `roost-client-core` compiles today, that its
347/11 is unchanged, and the line cost of the mapping. The nested-JSON decision
for `Sessions` is *located* but not *designed*, and that is the honest state:
**12 of 12 variants accounted for, one arm's source identified and its handling
undecided.**
