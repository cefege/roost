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
  Browser (Solid SPA on any device that can reach the selected HTTPS origin)
       │
       │  unary Connect-RPC + protobuf Sync WebSocket (HTTPS)
       ▼
  Coordinator (Bun)
       │  automatic: Tailscale Serve :4102 → HTTP 127.0.0.1:4103
       │  direct: Bun TLS on the operator-selected port
       │  optional automatic-mode Cloudflare listener: HTTP 127.0.0.1:4104
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

Automatic and direct HTTPS are separate self-hosted contracts. Direct
quickstart does not call Tailscale for the coordinator/local worker/browser;
the current extra-worker enrollment front door still performs a Tailscale
preflight.

## Product processes and shared protocol

### `apps/web` — the browser client

The SolidJS SPA owns operator interaction: workspaces, sessions, terminal
painting, settings, managed-auth routes, and dashboard selection. It consumes
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

### `apps/roost-cli` — install, health, rollout, and managed operations

The `roost` binary installs the local coordinator/worker pair, reports
`status` and `doctor`, performs journaled fleet rollout, and exposes the
operator-only managed account commands.

### `apps/shared` — the protocol contract

Wire schemas, generated protobuf types, event folding, terminal cells,
configuration, logging, and platform rules live here. The package is
subpath-only: consumers import the exported concern rather than a barrel.

## Event sourcing

Durable session state is an ordered event log, not a replaceable standalone
snapshot.

1. The **worker** emits typed lifecycle and metadata events.
2. For `opened`, `closed`, and `respawned`, the worker reserves capacity before
   mutating the keeper and commits the event to its SQLite
   `SessionEventStore` with full synchronization.
3. The coordinator link performs one ordered barrier on every connection:
   protocol hello → durable lifecycle replay, one exact ACK at a time →
   authoritative worker snapshot → live traffic. Replaceable metadata is
   bounded and coalescible rather than written to the durable outbox.
4. The **coordinator** validates sequence/identity, appends the event and
   updates its `sessions` projection in one SQLite transaction, then publishes
   the committed event.
5. The **browser** folds that event into the Solid store using the same
   `foldEvent()` function as the coordinator projection.
6. On cold start or a recovery reset, the browser hydrates a socket-bound
   current-state snapshot. Reconnect backfill then replays ordered events above
   its last persisted folded event id before switching to live delivery.

The snapshot in the worker barrier is itself a sequenced reconciliation event.
It repairs coordinator drift after downtime; it is not an out-of-band database
replacement.

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

Every session remains a shell PTY. Agent CLIs such as `omp`, Claude Code, or
Codex may be launched inside it, manually or through terminal launcher
configuration. Roost never spawns, supervises, or owns an agent session.

## Agent status (volatile, metadata only)

Roost labels a shell PTY with the state of whatever coding agent happens to be
running inside it — `working`, `blocked` (needs input), `idle`. This is
metadata about a terminal, not a structured agent session: no transcript, no
composer, no agent RPC.

Detection lives entirely on the **worker**:

- A periodic `ps` scan identifies a known agent binary in a session's process
  tree (`apps/worker/src/agent-status/process-scan.ts`).
- Agents Roost owns an integration for (OMP, Pi) report their own lifecycle —
  including "needs input" and retry grace — over a per-worker Unix socket. Every
  spawned PTY gets `ROOST_AGENT_SOCKET_PATH` + `ROOST_SESSION_ID`, and the
  server validates that the reporting pid really belongs to that session
  (`apps/worker/src/agent-status/report-server.ts`).
- Terminals with no integration (other agents, sessions that predate an
  install) fall back to scanning the session's own screen and OSC title/progress
  against pinned per-agent manifests (`apps/worker/src/agent-status/manifests.ts`).

An integration report beats the screen; a silent integration's lease expires
after 30 s and the session falls back automatically. The worker publishes one
*effective* state per session with a monotonic revision.

Nothing about status is persisted. Frames travel worker → coordinator
(`WAgentStatus`) → an in-memory, revision-ordered hub → the `Sync` stream
(`AgentStatusFrame`) → the browser store. A fresh `Sync` connection is seeded
from the hub snapshot, and a session close drops the record, so a worker,
coordinator, or browser restart converges instead of leaving a stale badge.

**Notification boundary.** The coordinator classifies only background
transitions (`working → blocked`, `working|blocked → idle`) and, after a 1 s
cancellable delay, sends Web Push to subscribed devices that are not currently
viewing that session; push subscriptions are the one persisted piece
(`push_subscriptions`). Everything else is browser-local: the in-app toast,
the unseen title badge, the optional sound, and a per-browser-profile claim so
two tabs of the same profile deliver one notification. Opening the session
cancels a pending notification and acknowledges its revision.

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

## Tenant and managed isolation

Self-hosted v0.5.0 creates one local tenant and automatically selects its sole
dashboard. Every resource query, Sync subscription, worker principal, and
terminal route still carries that persisted dashboard boundary.

The managed implementation extends the same boundary rather than trusting a
dashboard id supplied by the browser. Authentication resolves the persisted
account membership on the server; browser dashboard changes take effect only
after server confirmation, clear old scoped state first, and fence stale
async work by Sync generation.

On the qualified Linux operator host, a root provisioner drives one exact-spec
non-root coordinator container per account from a digest-pinned immutable
image. Writable state, keys, worker credentials, and the 64-hex route key are
separate per instance. Future edge Caddy would strip that opaque route before
forwarding to container port 4104; the route is a selector, never authority.
The four-file managed profile exercises the isolation and lifecycle boundary.

That implementation is **qualified, not publicly launched** in v0.5.0.
Accounts are operator-created and production email signup and Google auth
remain off. There are no production managed containers, published managed
image, active shared-dashboard route, or public signup surface.

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

- **Web:** `entry.ts` scrubs managed credential fragments before loading
  `main.tsx`; `routes.ts` owns route guards; `store/sync.ts` owns Sync;
  `store/dashboard-selection.ts` owns server-confirmed dashboard cutover.
  `CellTerminal.tsx` composes the eight `cell-terminal-*` behavior leaves, and
  `lib/cellRenderer.ts` paints the canvas grid.
- **Coordinator:** `main.ts` owns process lifecycle; `connect/router.ts`
  composes the single Connect-RPC service from 18 handler factories;
  `connect/ws-auth-deadline.ts` owns both authenticated WebSocket deadlines.
  `event-{transaction,projection,query}.ts`, `connect/sync-feed.ts`,
  `connect/terminal-screen-hub.ts`, and
  `connect/{auth-principal,dashboard-authorization}.ts` own persistence,
  delivery, terminal replicas, and persisted-principal scope respectively.
- **Worker:** `main.ts` is the entry point; `session-manager.ts` owns
  keeper-backed sessions; `transport/session-event-store.ts` owns the bounded
  SQLite lifecycle outbox; `transport/coord-link-unacked.ts` owns the
  replay/snapshot/live barrier; `transport/coord-link.ts`,
  `keeper/multiplexed-client.ts`, and `fsm.ts` own the remote link, local
  keeper transport, and connection state.
- **CLI:** `main.ts` dispatches commands; `quickstart-endpoint.ts` owns the two
  network contracts; `push.ts` and `push-fleet-rollout.ts` own atomic rollout.
  `saas/`, `saas-auth/`, and `saas-provisioner/` own managed operations,
  gateway authentication, and the privilege-separated provisioning bridge.
- **Shared:** `proto/roost/v1/` and `src/proto/roost/v1/` are the source and
  generated contracts; `src/wire/event{,-proto}.ts` own the canonical event
  fold/adapters; `src/cell.ts` owns the grid model. `package.json` is the
  authoritative subpath export map.
