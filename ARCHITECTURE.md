<!-- AUDIENCE: human -->
# Architecture

A tour of how Roost fits together. For build/run, see
[`GETTING_STARTED.md`](GETTING_STARTED.md); for terms,
[`GLOSSARY.md`](GLOSSARY.md). The exhaustive in-repo reference (for LLM
collaborators) is [`CLAUDE.md`](CLAUDE.md).

Roost is three TypeScript apps on [Bun](https://bun.sh), plus a shared wire
package. Protobuf messages travel over Connect-RPC for browser requests and
raw WebSockets for long-lived browser sync and outbound worker transport.
Path references in this document are repo-root-relative.

```text
  Browser  (Solid SPA, any device on your tailnet)
     │
     │  Connect-RPC over HTTP/2 + protobuf Sync WebSocket
     │    · unary            list calls, mutations, scrollback-cell fetches
     │    · Sync WebSocket   down: session events, cell grids, terminal links
     │                       up:   typed viewport + input control frames
     ▼
 ┌────────────────────────────────────────────┐
 │ Coordinator  (Bun loopback · :4103)        │
 │ Tailscale Serve :4102 → 127.0.0.1:4103     │
 │ optional Cloudflare browser listener :4104 │
 │   append-only `events` table  (SQLite)      │
 │     → projected into a `sessions` table     │
 │   EdDSA-JWT auth · fans deltas to every tab │
 └────────────────┬───────────────────────────┘
                  │  `/ws/coord-worker/:fp` · protobuf · outbound dial
     ┌────────────┼────────────────────┐
     ▼            ▼                     ▼
  Worker       Worker                Worker     (Bun · one per Mac)
  Mac A        Mac B                 Mac C
     │  one keeper subprocess hosts every shell PTY over one UDS and
     │  outlives worker restarts.
```

## The three apps

- **Web** (`apps/web/`) — a [Solid](https://www.solidjs.com) SPA on plain
  Vite. One `createStore` root; components read derived selectors and never
  mutate the store directly. URL is the source of truth for navigation. A
  terminal pane is `CellGridRenderer` (`apps/web/src/lib/cellRenderer.ts`) painting
  worker-authored cell grids; the browser holds no VT core of its own. Served
  as static files by the coordinator in production.
- **Coordinator** (`apps/coord/`) — `Bun.serve` with Connect-RPC, Kysely over
  `bun:sqlite`. It is control-plane only: auth, the append-only event log, the
  `sessions` projection, and fan-out to browsers. It never holds PTY state of
  its own.
- **Worker** (`apps/worker/`) — runs once per Mac, purely outbound. It dials
  the coordinator and owns every shell PTY via one multiplexed keeper
  subprocess; there is no inbound worker port.
- **Shared** (`apps/shared/`) — the wire source of truth: protobuf definitions,
  generated TS, Zod schemas, and the `foldEvent` reducer (below).

## Event sourcing is the spine

A session's state is never sent as a snapshot that can drift. The worker emits
small `SessionEvent`s (opened, attached, cwd changed, closed). The coordinator
appends each to the `events` table and folds it into
the `sessions` projection inside one SQLite transaction. The browser folds the
same events into its Solid store.

Both sides fold with the **same** `foldEvent` function from
`@roost/shared/wire`. So the server's projection and the browser's view agree
by construction, not by careful hand-mirroring. A reconnecting browser sends
the last event id it saw and gets exactly the events it missed.

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
- Delivery is **stream-addressed and resumable by exact sequence**. Every stream
  begins with a viewport-only full frame (`baseSeq === 0`,
  `sbBase === scrollbackTotal`); deltas are accepted only when stream ID, epoch,
  dimensions and `baseSeq` match the installed replica and `seq` is its exact
  successor. Any mismatch invalidates the recipient cursor and requests one
  full snapshot. `TerminalScreenHub` keeps the coordinator's canonical replica,
  while each browser's `apps/web/src/store/terminal-stream.ts` replica survives renderer detach and
  Sync reconnect. Per-byte sequence numbers remain one layer lower in the
  keeper's per-channel ring so a restarted worker can re-adopt a live PTY.
- Retained history is **demand-paged**: it is fetched only on explicit scroll or
  find (`SessionsGetScrollbackCells`, guarded by `scrollback_total`). A cold
  attach lands at the bottom having fetched none of it, scrolling inside the
  held window issues no RPC, and crossing the seam fetches under the current
  epoch. A resize while parked off-bottom keeps the reader's position and first
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

## Resilience

- **Worker loses the coordinator** — it reconnects with backoff (500 ms → 30 s);
  PTYs keep running in the keeper; on reconnect it emits a snapshot to
  reconcile.
- **Worker process crashes** — the keeper is a separate process, so PTYs
  survive; the restarted worker reattaches over the UDS and re-adopts open
  sessions.
- **Browser disconnects** — the `Sync` WebSocket redials on capped monotonic
  backoff (1 s → `SYNC_REDIAL_MAX_MS` 30 s, `apps/web/src/store/sync-watchdog.ts`) and
  backfills missed events from the last event id. The delay is capped, the
  attempt count never is: only a hidden document sleeps, and one coalesced
  lifecycle wake (`visibilitychange`, `pageshow`, Page Lifecycle `resume`,
  `focus`) re-dials in place and replays every viewport owner, so recovery
  never needs a reload. Visible terminal panes reclaim the current
  authoritative grid while history remains demand-paged.
- **Coordinator restarts** — workers redial, browsers reconnect, and every
  session is re-projected from the event log. No state is lost because the log
  is the source of truth.

## Entry points

- **Web:** `apps/web/src/main.tsx` (mount) · `apps/web/src/routes.ts` ·
  `apps/web/src/store/{root,projector,sync,selectors}.ts` ·
  `apps/web/src/connect.ts` (RPC client) ·
  `apps/web/src/components/CellTerminal.tsx` +
  `apps/web/src/lib/cellRenderer.ts` ·
  `apps/web/src/ws/sync-outbound.ts` +
  `apps/web/src/store/sync-dispatch.ts` (terminal control) ·
  `apps/web/src/store/agent-status.ts` +
  `apps/web/src/components/AgentNotificationBridge.tsx` (status +
  notifications)
- **Coordinator:** `apps/coord/src/main.ts` (Bun wrapper) ·
  `apps/coord/src/coord-factory.ts` (`createCoord`) ·
  `apps/coord/src/connect/router.ts` +
  `apps/coord/src/connect/handlers-*.ts` ·
  `apps/coord/src/connect/auth-interceptor.ts` ·
  `apps/coord/src/event-log.ts` ·
  `apps/coord/src/byte-hub.ts` ·
  `apps/coord/src/connect/session-control.ts` +
  `apps/coord/src/connect/announced-channel-barrier.ts` (terminal control +
  barrier) · `apps/coord/src/buses.ts` · `apps/coord/src/db/` ·
  `apps/coord/src/agent-status-hub.ts` + `apps/coord/src/push-dispatch.ts`
- **Worker:** `apps/worker/src/main.ts` ·
  `apps/worker/src/session-manager.ts` ·
  `apps/worker/src/transport/coord-link.ts` ·
  `apps/worker/src/keeper/multiplexed-main.ts` · `apps/worker/src/fsm.ts` ·
  `apps/worker/src/session-terminal-txn.ts` +
  `apps/worker/src/session-control-lanes.ts` (viewport transaction + lanes) ·
  `apps/worker/src/agent-status/` (scanner, manifests, report server,
  integrations)
- **Shared:** `apps/shared/proto/roost/v1/*.proto` ·
  `apps/shared/src/wire/event.ts` (`foldEvent`) ·
  `apps/shared/src/wire/agent-status.ts` · `apps/shared/src/cell/` (cell wire
  + emitter) · `apps/shared/src/wterm-core-factory.ts` (pinned core + WASM
  verification)
