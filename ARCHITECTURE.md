<!-- AUDIENCE: human -->
# Architecture

A tour of how Roost fits together. For build/run, see
[`GETTING_STARTED.md`](GETTING_STARTED.md); for terms,
[`GLOSSARY.md`](GLOSSARY.md). The exhaustive in-repo reference (for LLM
collaborators) is [`CLAUDE.md`](CLAUDE.md).

Roost has three product processes, one operator CLI, and one shared protocol
package. The browser SPA is served by the coordinator; coordinator and worker
processes run on Bun.
Path references in this document are repo-root-relative.

```
  Browser (Solid SPA on any device that can reach the operator's front door)
       │
       │  unary Connect-RPC + protobuf Sync WebSocket (HTTPS)
       ▼
  Front door (Caddy, nginx, a tunnel, tailscale serve — not Roost's code)
       │  plaintext HTTP to 127.0.0.1:4103
       ▼
  Coordinator (Bun)
       │
       │  WebSocket + protobuf (worker link)
       │
       ├──────────────────────┬──────────────────────┐
       ▼                      ▼                      ▼
  Worker (Bun)           Worker (Bun)           Worker (Bun)
  macOS/Linux            macOS/Linux            macOS/Linux
       │                      │                      │
       │ Unix socket          │ Unix socket          │ Unix socket
       ▼                      ▼                      ▼
  Keeper daemon          Keeper daemon          Keeper daemon
       │                      │                      │
       └─ PTY per session     └─ PTY per session     └─ PTY per session
```

The coordinator owns one listener: a plaintext loopback bind. TLS, DNS, and
public reachability belong to whatever the operator puts in front of it, and
`ROOST_WEB_PUBLIC_URL` is how the coordinator learns the resulting origin.

## Product processes and shared protocol

### `apps/web` — the browser client

The SolidJS SPA owns operator interaction: workspaces, sessions, terminal
painting, settings, and dashboard selection. It consumes
typed Sync frames and renders cell snapshots/deltas; raw PTY bytes never reach
the browser.

### `apps/coord` — the control plane

The coordinator owns the durable event log and projections, authentication,
dashboard authorization, workspace/task metadata, worker presence, Sync
fan-out, and terminal cell replicas. It routes terminal commands but never
owns the PTY or terminal parser.

### `apps/worker` — one per host

Each v0.5.0 worker runs on macOS or Linux, owns the keeper link and
`@wterm/core` terminal state, persists crash-sensitive lifecycle delivery,
and maintains the outbound coordinator link. Windows host release support is
paused; Windows remains usable as a browser client.

### `apps/roost-cli` — install, health, and rollout

The `roost` binary installs the local coordinator/worker pair, reports
`status` and `doctor`, and performs journaled fleet rollout.

### `apps/shared` — the protocol contract

Wire schemas, generated protobuf types, event folding, terminal cells,
configuration, logging, and platform rules live here. The package is
subpath-only: consumers import the exported concern rather than a barrel.

## Event sourcing

Durable session state is an ordered event log, not a replaceable standalone
snapshot.

1. The **worker** emits typed lifecycle and metadata events.
2. `opened`, `closed`, `respawned`, and private `agent_reference` updates enter
   the worker's bounded, fully synchronized SQLite `SessionEventStore`.
   Lifecycle capacity is reserved before keeper mutation; a locally
   acknowledged reference report returns success only after its append.
3. The coordinator link performs one ordered barrier on every connection:
   protocol hello → durable session-event replay, one exact ACK at a time →
   authoritative worker snapshot → live traffic. Ordinary replaceable metadata
   remains bounded and coalescible rather than entering the durable outbox.
4. The **coordinator** validates sequence/identity and appends each event in one
   SQLite transaction. Public events also update the public `sessions`
   projection and publish after commit; `agent_reference` updates only a
   sequence-aware private recovery projection.
5. The **browser** receives and folds public events into the Solid store using
   the same `foldEvent()` function as the coordinator projection. Private
   reference events are excluded from live delivery, backfill, and its cutoff.
6. On cold start or a recovery reset, the browser hydrates a socket-bound
   current-state snapshot. Reconnect backfill then replays ordered public events
   above its last persisted folded event id before switching to live delivery.

The snapshot in the worker barrier is itself a sequenced reconciliation event.
It repairs coordinator drift after downtime; it is not an out-of-band database
replacement and cannot erase private conversation recovery state.

## The terminal data plane

**One browser socket, one terminal-view command.** View activity, geometry and
PTY input all travel upstream on the existing Sync WebSocket. A terminal view
is identified by the authenticated socket and a random `view_id`; monotonically
increasing revisions make resize, hide and heartbeat commands idempotent.
Input remains session-scoped and independently admitted, with `view_id` used
only for attribution. Raw PTY bytes never enter the browser.

**Three explicit replicas.** The worker's wterm core is the authoritative
terminal. `TerminalScreenHub` holds one complete viewport-only coordinator
replica per watched session. `apps/web/src/store/terminal-stream.ts` holds one
browser replica per
session and fans owned frame shells to mounted `CellGridRenderer` subscribers.
`CellTerminal` only measures, publishes view activity, forwards input and
attaches a renderer; component mount or visibility is never the continuity
authority. Renderer detach, tab switches and Sync reconnects therefore cannot
discard the browser's current baseline.

**Coordinator-owned membership and geometry.** `TerminalViewHub` is the only
view registry and the only SCD calculator. For every session it independently
takes the minimum active columns and rows. Explicit hide, authorization loss
and durable session close remove a view immediately. A broken transport parks
its views until their existing 15-second lease expires; five-second heartbeats
and a one-second sweep let the normal one-second reconnect replace the parked
socket without resizing the PTY. The first view, an effective-size change, the
last-view disable, re-enable, worker replacement or unavailable-state retry
mints a UUID stream ID. With no active views, the worker keeps the last PTY
geometry and core but gates cell emission.

**Full before delta.** Every stream generation begins with one complete
authoritative full. A delta is accepted only when its stream ID, grid epoch,
dimensions and `base_seq` match the current replica and its `seq` is the exact
successor. Any gap invalidates that cache and latches one snapshot request.
Status frames and partial chunks never establish a baseline. Full repair
replaces the canonical replica atomically while each renderer keeps its last
complete DOM until the replacement is ready.

Foreground visibility is also a liveness contract. The browser republishes
each desired terminal view every 5 seconds and requires its generation-matched
view-state ACK within 15 seconds. Independently, a visible active pane that
accepts neither a baseline nor a stream delta for 20 seconds issues a
generation-bound baseline challenge. If no proof arrives within 10 seconds,
the browser requests Sync generation recovery and redials with bounded
backoff. Hidden tabs may park; returning to the foreground repairs the stream
without a page reload.

**Bounded resumable fan-out.** Full frames larger than 1 MiB are split on whole
row boundaries. Every chunk shares one snapshot ID and identical scalar
metadata; indices are contiguous, and each viewport row `0..rows-1` must occur
exactly once. Duplicate rows are invalid even when byte-identical. Assemblers
also enforce the 256-row, 65,536-span, 1,024-link, 1-MiB-part and 64-MiB-total
limits plus a ten-second inter-chunk timeout. The coordinator installs a
separate snapshot cursor per socket, materializing only the next chunk when the
existing queue and ACK windows have room. View-state and cell frames share the
same per-session terminal lane, so new-stream cells cannot overtake their state
predecessor. A slow socket can restart from the coordinator's current immutable
full without blocking another viewer.

**Resize at the keeper's ordered boundary.** `DTerminalStreamState` carries the
coordinator's already-aggregated geometry to one worker stream state per
channel. The keeper's acknowledged resize result is the synchronization point:
bytes before it parse at the old size, the callback synchronously calls
`wtermCore.resize(cols, rows)` on the existing core, and later `PtyOut` parses at
the new size. The resize invalidates only the cell emission epoch and forces a
full baseline; ordinary live resize never rebuilds a core from the raw ring.
Keeper-history replay is reserved for genuine worker adoption when no
in-memory core exists. An unprovable boundary fails closed rather than parsing
bytes at guessed geometry.

**Proven outcomes and independent input.** Worker stream results retain the
keeper's committed/rejected/ambiguous write proof and a classified failure
kind. Proven pre-write rejection may retry once under a new stream ID;
session/core/boundary failures become unavailable or enter explicit adoption
without rolling back healthy view membership. Input never queues behind a
resize result. Browser `input_seq` and worker request IDs correlate results at
their own hops; the worker allocates the keeper's monotonically increasing
per-channel key, so two devices may both send local sequence 1 without
collision or loss of FIFO ordering.

**Canonical model vs painted DOM.** These remain different clocks and
`apps/web/src/lib/terminalDiagSnapshot.ts` reports both: view ID/revision/lease state,
coordinator stream ID, browser replica epoch/sequence, and renderer reconciled
epoch/sequence. A renderer frozen for a reader may intentionally trail the
replica. A same-width, same-epoch full repair is a new tail checkpoint:
already-painted immutable history rows and their DOM nodes survive while the
live viewport is replaced. Width/epoch changes retain the global reader anchor
and refetch it when necessary.

**Live vs reading.** `CellGridRenderer` carries an explicit `ReaderIntent` plus
holds for selection and an armed link. Passive output and composer drafting do
not cancel a reader. One admitted local keystroke calls
`prepareLiveInteraction()`, adopts pending state and returns to the live tail as
one transition. Leaving the surface ends the reading interval, but it does not
delete the session replica.

**The application decides mouse and focus forwarding.** Cell frames carry the
core's `mouse_tracking`, `mouse_sgr`, `focus_events`, `cursor_keys_app` and
`bracketed_paste` modes. The browser forwards only what the application
requested, using SGR-1006 or bounded legacy X10 as appropriate. Alternate
screen occupancy alone never captures mouse input.

**Publication remains durable and ordered.** Session events commit before their
authenticated worker/channel binding is installed and before `sessionBus`
publication. The announced-channel barrier still preserves first-frame order.
Loss or overflow invalidates `TerminalScreenHub` and requests one full after
the route is announced; it never fabricates a browser view. Browser flow
control still uses cumulative delivery ACKs, bounded queues and reconnect
without page reload.

Every session remains a shell PTY; agent CLIs such as `omp`, Claude Code, or Codex run inside it manually or through terminal launcher configuration.
Roost never spawns, supervises, or owns an agent process, conversation, transcript, tool call, or approval model.
It may expose volatile worker-observed state, accept occupant-fenced text for the same ordinary PTY, retain an integration-supplied opaque conversation reference as private recovery metadata, and use that reference for one narrowly fenced worker-owned restore input after involuntary PTY loss. None of these surfaces creates a structured agent session.

## Agent conversation references (private recovery metadata)

The official OMP integration reports a versioned opaque reference through a
separate acknowledged local method. The worker revalidates the session
capability, kernel peer PID, and fresh agent-process ancestry before accepting
it; the report cannot choose a provider or executable. An official
`session_file` is stored as kind `path` only while it is absolute — POSIX or
Windows shape — otherwise the official `session_id` offered in the same call is
stored as kind `id`. Values are well-formed Unicode free of control characters,
bounded at 512 UTF-8 bytes for an id and 4,096 for a path, and are never
opened, normalized, indexed, rendered, or logged.
`apps/shared/src/agent-conversation-reference.ts` owns every one of those
rules, so a reference that could not be resumed is never stored.

Set, replacement, and explicit clear are private durable `SessionEvent`s
ordered by the worker outbox `client_seq`, independently of volatile agent
status. The worker authors one clear itself: when the reporting agent process
leaves a still-live session, the status detector emits exactly one durable
`agent_reference: null` for that session, so a later restore cannot resume a
conversation the user already ended.
The coordinator persists the newest reference and sequence in private
session recovery columns. Snapshots cannot erase them, lower or duplicate
sequences cannot change them, and closing the session deletes them. The exact
owning worker receives one recovery row for each open session; browsers,
device/CLI session listings, Sync live/backfill lanes, search, logs, and audit
never receive the opaque value.

An involuntary-loss restore is worker-local and OMP-specific. Keeper adoption
always runs first, and successful adoption sends zero resume input. Only after
adoption fails, the ordinary replacement shell exists, and its `respawned`
event is durably admitted may `apps/worker/src/agent-conversation-restore.ts`
resolve the stored reference through its versioned, fixed OMP descriptor. The
descriptor supplies the `omp` executable and the `--resume=` option form
(OMP has no `--session` flag); the stored opaque value only ever completes
that one argv element, quoted with the canonical POSIX shell quoting utility.
The worker submits the rendered command plus one CR as exactly one
acknowledged input batch to the replacement shell. A reference already claimed
by an earlier session in the same reconciliation pass is skipped, so two panes
cannot resume one conversation; a claim is released again when that session's
write is rejected before any keeper byte, so the next session holding the same
reference still resumes it. Neither the opaque value nor the rendered command
enters structured logs.

Integration data cannot choose executable or template text. Accepted,
rejected, and ambiguous input results are terminal for that boot attempt and
are never retried or routed back through respawn/tombstone handling. A
non-accepted result that reports written bytes is followed by exactly one
worker-owned line-discard byte (`0x03`), because `--resume=` matches an id by
prefix and a truncated command left on the prompt would attach one Enter to a
different conversation; that cancel never re-sends the resume command and
never affects session lifecycle. Every outcome retains the reference until the
integration later replaces or clears it.

`ROOST_AGENT_CONVERSATION_RESTORE` is a strict worker-local `0|1` setting.
Absent and `0` mean disabled on every platform. `1` enables the path only on
POSIX; Windows rejects it as unsupported. The default remains disabled pending
actual official-OMP POSIX real-stack qualification—implementation and unit
coverage are not that qualification and do not justify a default-on claim.

## Agent status (volatile, metadata only)

Roost labels a shell PTY `working`, `blocked` (needs input), or `idle`. This is terminal metadata, not a structured agent session or execution model.
Dashboard-authorized RPCs can read it, await an observed state transition, or use it as the exact fence for one PTY input; they never control an agent through a separate channel.

Detection lives entirely on the **worker**:

- A periodic `ps` scan identifies known agent binaries in the session process tree (`apps/worker/src/agent-status/process-scan.ts`).
- OMP and Pi report lifecycle, including "needs input" and retry grace, over a per-worker local endpoint. The server kernel-attests the accepted socket's peer PID, then a fresh process-tree scan must prove that exact process is the current known agent under the capability's session; process identity and ordering never come from report fields (`apps/worker/src/agent-status/report-server.ts`).
- Sessions without an integration fall back to their own screen and OSC title/progress against pinned manifests (`apps/worker/src/agent-status/manifests.ts`).

An integration report beats the screen; a silent integration's lease expires after 30 s and the session falls back automatically.
The worker publishes one effective row per session. `status_epoch` identifies a registry lifetime, `occupant_id` a verified process incarnation, and `source` an integration or screen observation; revisions are monotonic within that identity.
PID stays worker-private. Identity fields are volatile observation and fencing state, not process handles, credentials, or conversation identifiers.
Only a fully identified integration row is `promptable`; screen and identityless legacy rows remain readable with `promptable=false`.

Nothing about status is persisted. Frames travel worker → coordinator (`WAgentStatus`) → an in-memory hub ordered by epoch, occupant, and revision → `Sync` (`AgentStatusFrame`) → browser.
A fresh `Sync` connection gets the hub snapshot, and session close drops its row, so worker, coordinator, and browser restarts converge without stale badges.

`AgentStatusGet`, `AgentStatusList`, and `AgentStatusWait` authorize the dashboard actor before reading or entering the hub; missing and foreign sessions share not-found behavior. Waits register before current-state inspection, pin an exact epoch and occupant, and resolve from that inspection or an accepted hub update, timeout, replacement, or session close—never output scraping or polling. The registry caps waits at 32 per session and 2,048 process-wide.
`SessionsPrompt` names `session_id`, exact `expected_status_epoch`, `expected_occupant_id`, and safe-`uint64` `expected_revision`, plus nonempty `text` and optional wait configuration. Text is capped at 16,384 UTF-8 bytes. Wait configuration is all absent or a nonempty unique subset of `idle|working|blocked` plus `wait_timeout_ms` in `1..300000`.
The coordinator's `apps/coord/src/connect/agent-prompt-control.ts` registers `waitForAgentStatus` before enqueueing dedicated `DAgentPrompt` tag 16 with request/session/input sequence, exact identity and revision, original text, and relative budget. `WInputResult` remains the upstream write truth; the coordinator consumes the waiter only after a definite rejection and awaits it after accepted or ambiguous input.
`apps/worker/src/agent-prompt-control.ts` refreshes private process proof before admission, then immediately before `beginInput` rechecks the live session/channel, deadline and current connection, integration source, exact epoch/occupant/revision, the same refreshed process, and state `idle|working`.
All fence failures at the final pre-`beginInput` check are rejections with zero keeper writes; a failure after admission is ambiguous. The worker uses `apps/shared/src/terminal-input.ts`, matching the browser's newline normalization and, when bracketed paste is active, its ESC-stripping wrapper; it then appends one CR and performs one keeper write. `SessionsInput` remains raw bytes with no fence, transformation, implicit Enter, or semantic change.
The response keeps exact input outcome (`accepted|rejected|ambiguous`) separate from optional wait outcome (`matched|timed_out|occupant_changed|session_closed`) and exposes only a reason of at most 200 characters and `written_bytes` of at most 16,397. Prompt text and agent status messages are never logged, audited, or stored, and an ambiguous write is never retried.
`roost api agent-status <session> [--json]`, `roost api agents [--json]`, `roost api agent-wait <session> --until <states> --timeout <duration>`, and `roost api agent-prompt <session> <text> [--wait --until <states> --timeout <duration>]` expose this PID-free surface.

**Notification boundary.** The coordinator classifies background `working → blocked` and `working|blocked → idle` transitions and, after a 1 s cancellable delay, sends Web Push to subscribed devices not viewing that session.
Push subscriptions are the one persisted piece (`push_subscriptions`); in-app toast, unseen title badge, optional sound, and per-browser-profile claim remain browser-local.
Opening the session cancels a pending notification and acknowledges its revision.

## Terminal fidelity (the hard part)

Streaming raw bytes to a browser terminal looks simple and corrupts in
practice. The browser re-parses the byte stream at whatever width its own
window happens to be, and **re-parse at a new width is the corruption**: a
terminal core's row resize is asymmetric and lossy — shrinking pushes rows into
scrollback, growing fills with blanks, and neither reverses. No terminal
library reflows a TUI grid to a new width; they all freeze instead. Reconnects
on top of that duplicate or drop output. The only structural fix is to stop
reflowing on the client, so Roost uses the model server-side terminal
multiplexers use:

- The **worker** holds the one authoritative grid per session and rebuilds it at
  a single agreed width on resize.
- The **browser** paints that grid as-is. It parses no VT and never re-reflows;
  surplus pane space is **letterboxed** — rows stay `cols` characters wide and
  the container centres them instead of stretching
  (`apps/web/src/lib/cellRenderer.ts`). The accepted tradeoff: plain shell
  history no longer rewraps to a narrower device, it scrolls sideways.
- The agreed width is the **SCD** (smallest common denominator) across active
  views, so no viewer is clipped. `TerminalViewHub` owns membership and computes
  the column and row minima independently; the worker receives only that
  aggregate stream geometry. Leases absorb reconnect wobble, and letterboxing
  absorbs pixel differences without competing resize owners.
- **Alt-screen owns the viewport and carries no scrollback**, so in that mode
  there is nothing to corrupt. The frame states it (`CellGridFrame.altScreen`);
  the renderer hides the history sheet and locks scrolling while it is set, and
  restores both on leaving.
- The cell payload has **one source of truth**, `apps/shared/src/cell/`:
  `CellSpan` (a run of cells sharing one style, whose style fields mirror the
  core's own `CellData`), `CellRow` (index plus right-trimmed spans), and
  `CellGridFrame` (cols, rows, cursor, alt-screen, viewport rows, scrollback
  append, totals, stream ID, base sequence, sequence and epoch). The protos
  mirror that shape as either a complete frame or bounded row chunks from
  worker → coordinator → browser.
- Delivery is **stream-addressed and resumable by exact sequence**. A fresh
  stream and any grid-incompatible renewal begin with a viewport-only full
  frame (`baseSeq === 0`, `sbBase === scrollbackTotal`). A compatible same-grid
  renewal may include at most `SB_RENEWAL_HISTORY_ROWS` retained tail rows so a
  reconnect preserves the recent painted window without an RPC. Deltas are
  accepted only when stream ID, epoch, dimensions and `baseSeq` match the
  installed replica and `seq` is its exact successor. Any mismatch invalidates
  the recipient cursor and requests one full snapshot. `TerminalScreenHub`
  keeps the coordinator's canonical replica, while each browser's
  `apps/web/src/store/terminal-stream.ts` replica survives renderer detach and
  Sync reconnect. Per-byte sequence numbers remain one layer lower in the
  keeper's per-channel ring so a restarted worker can re-adopt a live PTY.
- Retained history beyond any bounded renewal tail is **demand-paged**: it is
  fetched only on explicit scroll or find (`SessionsGetScrollbackCells`,
  guarded by `scrollback_total`). A cold attach lands at the bottom having
  fetched none of it, scrolling inside the held window issues no RPC, and
  crossing the seam fetches under the current epoch. A resize while parked
  off-bottom keeps the reader's position and first
  visible row, but de-materialises the rows behind the seam: the retired
  epoch's held window is dropped and a spacer preserves `scrollHeight`. That is
  reversible on the next demand fetch, which is why a render-stress run on the
  main screen has to let the pane settle before it starts — `runRenderStress`
  captures one marker baseline up front and flags any later change of range.
- The **core** is `@wterm/core` 0.3.4, loaded through
  `apps/shared/src/wterm-core-factory.ts` from a locally patched WASM build
  committed at `apps/shared/wasm/wterm-roost.wasm`. Its sha256 sits beside it
  in `apps/shared/wasm/wterm-roost.wasm.sha256`, and `scripts/rebuild-wterm-wasm.sh` reproduces
  the build. Loading is fail-fast: `verifyRoostWasm` rehashes the bytes against
  that digest and checks every 0.3.4 bridge export by name, throwing instead of
  returning a degraded core.
- Column occupancy is **explicit on the wire**: `PbCellSpan.columns` states how
  many terminal columns a span owns, so a double-width glyph is one atomic
  two-column span and no phantom continuation cell is ever emitted.
- The scrollback origin is **authoritative, never inferred**:
  `cell/emitter.ts::scrollbackOrigin` reads the core's
  `getScrollbackDiscardedCount()` and throws if a core cannot supply it, so
  absolute history indices can never re-alias.

This is the part of the codebase with the most scar tissue; the recurring
failure modes and their fixes are catalogued in `CLAUDE.md`.

## Tenant isolation

Coordinator startup creates one local tenant and automatically selects its sole
dashboard: `apps/coord/src/self-hosted-tenant.ts` runs unconditionally at boot
and is the only tenancy invariant. Every resource query, Sync subscription,
worker principal, and terminal route carries that persisted dashboard
boundary.

Authorization never trusts a dashboard id supplied by the browser: it resolves
the persisted account-device membership on the server. Browser dashboard
changes take effect only after server confirmation, clear old scoped state
first, and fence stale async work by Sync generation.

## Portable browser-local pane layouts
Active pane trees and runtime leaf/split UUIDs persist only in each browser profile under `roost.paneLayout.v1`. `UiReportState` exposes an off-terminal route or a route resolved to an open coordinator-admitted session; unresolved and optimistic `/s/:id` route fields are blank until hydration/admission schedules another report. When an open route session identifies a folder, the report also carries its browser-owned folder key plus a typed `LayoutDocumentV1` containing only admitted members, never runtime IDs or a second JSON layout shape.
Copy, download, and reporting serialize the same strict V1 document with deterministic preorder leaf/slot keys and inclusive `0.1..0.9` ratios. One bounded parser rejects excessive UTF-8 identifiers, recursion, nodes, slots, or bindings before recursive conversion. Local import/apply and remote apply additionally validate current live-folder membership; remote apply rejects any pending/tombstoned optimistic member. Successful application materializes fresh runtime IDs, commits once through `applyLayoutDocument`, and attempts post-commit navigation to the focused selection. Open tabs do not consume storage events or live-fold one another's layouts.
`UiApplyLayout` is the sole acknowledged exception to browser-local control: its dashboard-authorized caller sends a nonempty browser fingerprint/tab tuple, and the CLI derives exactly one such tuple from `UiListStates` or rejects an absent/ambiguous tab before apply. Sync rejects tab IDs over 256 UTF-8 bytes before socket state, and the coordinator bounds live targets to 32 distinct tuples per fingerprint and 256 per dashboard with one generic capacity rejection. It reserves only the selected tuple's current authenticated read/write Sync-v2 socket plus a fresh correlation ID before publication, so a stale fingerprint cannot redirect to another browser that later reuses the tab ID. The page rechecks the exact tab/socket/correlation and its URL-active live folder, then acknowledges `applied` after the commit and navigation attempt; `applied` proves the commit, not successful navigation completion, while invalid current membership returns `rejected`.
Wrong, stale, duplicate, and late results are ignored; cancellation removes the pending request, and socket close/replacement or timeout returns `target_gone` without retry. `target_gone` means the acknowledgement is unavailable, not that execution did not occur. UI commands are never seeded, and read-only worker Sync subscribers receive neither UI state nor command frames. The eight `UiDispatch` commands remain fire-and-forget: `delivered` is exactly the dashboard Sync-subscriber count at publication, never an execution or acknowledgement count.

## Resilience model

- **Browser drops:** Sync resumes from the last persisted folded event id,
  then visible terminal liveness challenges repair any stalled pane.
- **Worker drops:** keeper processes preserve PTYs. Reconnect performs durable
  lifecycle replay, authoritative snapshot reconciliation, then live traffic.
- **Coordinator drops:** workers retain PTYs and their durable lifecycle
  outboxes; browsers redial while visible. The coordinator reopens the
  transactionally persisted event log/projection, and reconnecting worker
  snapshots repair drift.
- **Worker process crashes:** keepers survive independently. The restarted
  worker adopts them and emits `respawned` only when a terminal was actually
  replaced.

The design goal is not “nothing ever disconnects.” It is “a disconnect cannot
silently lose a PTY lifecycle edge, and every visible terminal either proves
progress or triggers bounded recovery.”

## Key entry points

- **Web:** `entry.ts` scrubs URL-carried credentials before loading
  `main.tsx`; `routes.ts` owns route guards; `store/sync.ts` owns Sync;
  `store/auth-boundary.ts` owns credential-boundary teardown.
  `CellTerminal.tsx` composes the eight `cell-terminal-*` behavior leaves, and
  `lib/cellRenderer.ts` paints the canvas grid.
- **Coordinator:** `main.ts` owns process lifecycle; `connect/router.ts`
  composes the single Connect-RPC service from 18 handler factories;
  `connect/ws-auth-deadline.ts` owns both authenticated WebSocket deadlines.
  `event-{transaction,projection,query}.ts`, `connect/sync-feed.ts`,
  `connect/terminal-screen-hub.ts`, and
  `connect/auth-principal.ts` own persistence, delivery, terminal replicas,
  and persisted-principal resolution respectively.
- **Worker:** `main.ts` is the entry point; `session-manager.ts` owns
  keeper-backed sessions; `transport/session-event-store.ts` owns the bounded
  SQLite lifecycle outbox; `transport/coord-link-unacked.ts` owns the
  replay/snapshot/live barrier; `transport/coord-link.ts`,
  `keeper/multiplexed-client.ts`, and `fsm.ts` own the remote link, local
  keeper transport, and connection state.
- **CLI:** `main.ts` dispatches commands; `quickstart-endpoint.ts` validates the
  one declared front-door origin; `push.ts` and `push-fleet-rollout.ts` own
  atomic rollout.
- **Shared:** `proto/roost/v1/` and `src/gen/roost/v1/` are the source and
  generated contracts; `src/wire/event{,-proto}.ts` own the canonical event
  fold/adapters; `src/cell.ts` owns the grid model. `package.json` is the
  authoritative subpath export map.
