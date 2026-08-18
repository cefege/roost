<!-- AUDIENCE: human -->
# Architecture

A tour of how Roost fits together. For build/run, see
[`GETTING_STARTED.md`](GETTING_STARTED.md); for terms,
[`GLOSSARY.md`](GLOSSARY.md). The exhaustive in-repo reference (for LLM
collaborators) is [`CLAUDE.md`](CLAUDE.md).

Roost is three TypeScript apps on [Bun](https://bun.sh), plus a shared wire
package. Protobuf messages travel over Connect-RPC for browser requests and
raw WebSockets for long-lived browser sync and outbound worker transport.

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
  terminal pane is `CellGridRenderer` (`lib/cellRenderer.ts`) painting
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

**One browser control socket.** Everything a pane sends upstream rides the same
Sync WebSocket, as a typed `SyncClientFrame` command — `viewport` or `input`
(`apps/shared/proto/roost/v1/sync.proto`). There is no second browser stream
for keystrokes. The coordinator admits a command only from the socket id it
issued (`connect/sync-ws-handler.ts`), forwards it to the session's worker, and
answers with exactly one typed result frame: `viewportAccepted` /
`viewportRejected` / `viewportAmbiguous`, `inputAccepted` / `inputRejected` /
`inputAmbiguous`. The worker writes keystrokes into the keeper, which writes
the PTY. Raw output returns only as far as the coordinator: the worker sends
authoritative cell-grid snapshots and deltas whose spans carry the core's own
per-cell OSC 8 link identity (`PbCellSpan.link_uri`/`link_key`), and those
cells — never raw PTY bytes — cross the Sync socket downward. Nothing derives
hyperlinks from the byte stream and nothing matches link text.
Only visible panes claim cell delivery; a
mounted offscreen pane keeps its last grid without receiving cells, then
reclaims an authoritative snapshot when revealed.

**Proven outcomes, nested budgets.** A worker result carries a
`TerminalWritePhase` (`PRE_WRITE`, `WRITTEN`, `UNKNOWN`) beside its status, so
"rejected" means *proven* untouched and anything unproven is reported as
ambiguous rather than guessed either way; on ambiguity the coordinator keeps
the viewer's provisional cell subscription instead of rolling it back. Every
command frame carries a *relative* `budget_ms` — the coordinator's remaining
wait minus a 750 ms return-trip reserve (`connect/worker-send.ts`) — which the
worker measures on its own monotonic clock from frame receipt, so the two hosts
never compare wall clocks. Each hop is strictly inside the one outside it, so an
inner expiry always reports back while its outer waiter is still listening, but
the two paths have different chains. **Input:** the keeper's 2.5 s per-command
watchdog (`COMMAND_RESULT_TIMEOUT_MS`, `keeper/keeper-pool-io.ts`) < the
worker's `budget_ms` slice < the coordinator's `INPUT_CONTROL_TIMEOUT_MS` (5 s)
< the browser's 10 s result timeout. **Viewport:** the same 2.5 s keeper
watchdog inside the transaction's per-phase bounds (the largest,
`keeper_written`, is 6 s), all clamped by both `budget_ms` and the
`VIEWPORT_TXN_BUDGET_MS` 7 s whole-transaction ceiling < the coordinator's
`VIEWPORT_CONTROL_TIMEOUT_MS` (8 s) < the browser's 10 s.

**One desired viewport per mount.** `acquireTerminalViewportOwner(sessionId)`
(`apps/web/src/ws/sync-outbound-viewport.ts`, re-exported from
`apps/web/src/ws/sync-outbound.ts`) is the only place a pane's desired
geometry lives; the backing per-session state is the one `viewportSessions`
map in `apps/web/src/ws/sync-outbound-viewport-registry.ts`. The owner holds a
token, so a stale mount's late claim cannot mutate a newer one; it exposes
`claim`, `heartbeat`, `noteFullFrame`, `subscribeStatus`, and `dispose`,
retries a rejected or lost claim on a bounded 250 ms → 2 s ladder, and reports
a typed status (`pending`, `retrying`, `repairing`, `ready`, `rejected`,
`superseded`). Cell-delivery membership is a matching tokenized claim,
`acquireCellMountClaim` (`store/sync-dispatch.ts`). Components keep no private
"last sent" or "has a frame" liveness beside those.

**Canonical model vs painted DOM.** These are deliberately different clocks,
and every terminal probe reports both
(`apps/web/src/lib/terminalDiagSnapshot.ts`):
`wire_received` is what the socket decoded, `handler_canonical` is the grid
epoch and seq the pane folded, `dom_reconciled` is what actually got painted,
and `reconcile_block_reason` names why the last two differ. A pane that is
canonically current but intentionally frozen for a reader is a healthy state,
not a stall — and a pane whose DOM watermark trails with no reason is a bug.

**Live vs reading.** `CellGridRenderer` carries an explicit `ReaderIntent` —
`"live"` or `"reading"` — plus a composed hold mask for an active selection and
an armed link (`apps/web/src/lib/cellRenderer.ts`). Passive output and composer
drafting never cancel a reader. One admitted local keystroke does:
`prepareLiveInteraction()` clears both holds, adopts the reader-pending frame,
and re-pins the bottom as a single transition, so an admitted input causes at
most one repair. Leaving the visible surface — park, `pagehide`, unmount —
ends the reading interval, so a pane you return to presents the newest
canonical frame rather than the one its reader froze.

**The application decides what happens to the mouse.** Every cell frame carries
the modes the core read out of the PTY stream — `mouse_tracking` (DECSET
1000/1002), `mouse_sgr` (1006) and `focus_events` (1004) — beside the older
`cursor_keys_app`/`bracketed_paste` bits. The browser forwards a pointer gesture
only when the app actually asked (`mouseForwardEnabled() && mouseTracking() !== 0`;
the user preference survives as the override, defaulting on) and encodes it the
way the app asked: SGR-1006, or legacy X10 with coordinates clamped at 223
(`apps/web/src/lib/terminalMouse.ts`, a pure function so the decision is
unit-testable). Mode 1000 reports press and release only; 1002 adds motion while
a button is held. With `focus_events` armed the pane reports `ESC [ I` / `ESC [ O`
on real focus and blur. Alt-screen occupancy is NOT the question and no longer
gates anything: `vim`, `less` and `man` occupy it without requesting the mouse,
and forwarding to them swallowed the click with no native fallback.

**Support-grade terminal telemetry.** Three things the core knows and nothing
else can reconstruct are sampled per session: escape sequences it reported as
unhandled (`terminal.unhandled_sequence`, the "renders wrong in Roost, fine in
iTerm" lane — deduplicated per session and reset when a rebuild mints a fresh
core), OSC 8 hyperlink-table saturation (`terminal.hyperlink_saturated`, after
which new distinct links silently degrade to plain text), and why history is
missing — genuine eviction versus a resize-forced replay bounded by the raw ring
(`ScrollbackHistoryFloor` on the scrollback-cells response). The core logs only
unhandled CSI finals, so an empty list is not proof of full support.

**Worker: a viewport change is a transaction.** `session-terminal-txn.ts` walks
the phases named in `session-resize-capture.ts::TerminalTxnPhase` — validating
→ admitted → keeper_written → pty_resized → grid_rebuilt → settled — each with
its own bounded deadline under the transaction's ceiling. Only `validating` may
fail as a definite rejection; past that a failure is ambiguous unless the
keeper proves the PTY was never resized. Authoritative size and resize sequence
are recovered from the keeper (`GetTerminalState`) rather than remembered, and
the core is rebuilt exactly once per committed resize. Two lanes keep that off
the typing path (`session-control-lanes.ts`): `terminalControlChains` gives
whole transactions mutual exclusion per channel, while `keeperAdmissionLane`
only preserves receive order for keeper writes and is released at the
write-ordering boundary instead of the ACK, so PTY input never queues behind a
pending resize result.

**Coordinator: publication is durable and ordered.** `appendEvent`
(`apps/coord/src/event-log.ts`) commits the `events` insert and the `sessions`
fold in one SQLite transaction, then — strictly after commit — installs that
event's exact authenticated worker/channel binding (`applyDurableChannelIndex`
in `byte-hub.ts`) and only then publishes on `sessionBus`. No tab can observe
`opened`, `respawned`, or `snapshot` before the route its first claim or
keystroke needs exists, and superseded worker generations are fenced. Frames
for a just-announced channel queue on the announcement barrier
(`connect/announced-channel-barrier.ts`), which drains cell and binary frames
in arrival order; if it overflows or times out while a viewer is watching, the
coordinator marks that exact route for repair (`byte-hub.ts`) and replays a
heartbeat-shaped claim with `held_cell_seq = 0`, which the worker answers with
one authoritative full frame.

**Guarded browser delivery.** A browser opts into application flow control with
exact `flow=1` negotiation and cumulatively acknowledges delivery sequence
numbers only after synchronous dispatch. Independently of the native WebSocket
buffer, the coordinator caps unacknowledged work at 512 frames, 4 MiB, and a
3-second oldest-frame age. Backpressure closes the stale socket; the browser
immediately reconnects without reloading, and visible terminals recover through
the same authoritative snapshot path.

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
  tree (`agent-status/process-scan.ts`).
- Agents Roost owns an integration for (OMP, Pi) report their own lifecycle —
  including "needs input" and retry grace — over a per-worker Unix socket. Every
  spawned PTY gets `ROOST_AGENT_SOCKET_PATH` + `ROOST_SESSION_ID`, and the
  server validates that the reporting pid really belongs to that session
  (`agent-status/report-server.ts`).
- Terminals with no integration (other agents, sessions that predate an
  install) fall back to scanning the session's own screen and OSC title/progress
  against pinned per-agent manifests (`agent-status/manifests.ts`).

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
- The agreed width is the **SCD** (smallest common denominator) across live
  viewers, so no viewer is ever clipped
  (`apps/worker/src/session-viewport.ts`, `desiredViewportSize`). The withdraw
  grace, claim TTL and freshness cutoff that worker and coordinator must agree
  on live in `apps/shared/src/viewport.ts`. Momentary wobble is absorbed by
  that policy plus the letterbox, not by resizing the PTY.
- **Alt-screen owns the viewport and carries no scrollback**, so in that mode
  there is nothing to corrupt. The frame states it (`CellGridFrame.altScreen`);
  the renderer hides the history sheet and locks scrolling while it is set, and
  restores both on leaving.
- The cell payload has **one source of truth**, `apps/shared/src/cell/`:
  `CellSpan` (a run of cells sharing one style, whose style fields mirror the
  core's own `CellData`), `CellRow` (index plus right-trimmed spans), and
  `CellGridFrame` (cols, rows, cursor, alt-screen, viewport rows, scrollback
  append, totals, seq, epoch). The protos mirror that shape: `WCellGrid`
  worker → coordinator (`worker_transport.proto`) and `PbCellGridFrame` as the
  `FirehoseFrame.cell_grid` variant coordinator → browser (`sync.proto`).
- Delivery is **resumable by sequence and addressed by epoch**: every cell frame
  carries a monotonic `seq` and a `gridEpoch`, a viewer's claim carries the
  `held_cell_seq` it has already applied, and the worker answers a stale or
  unset sequence with one authoritative full frame — so a splice never
  duplicates or drops. A full frame is **viewport-only**: it carries no
  historical rows and reports `sbBase === scrollbackTotal`
  (`SB_SNAPSHOT_HISTORY_ROWS = 0`), so attaching to a hundred-thousand-line
  session costs one screen. A delta carries the changed viewport rows, the
  newly appended scrollback rows and the cursor. The epoch advances only on a
  *semantic* reframe — dimensions, alt-screen, a rewind, or the ring evicting
  past what the client holds (`cell/emitter.ts::nextCellFrame`) — because those
  are exactly the transitions after which an absolute row index stops naming
  the same row. Per-byte sequence numbers survive one layer lower, in the
  keeper's per-channel ring, so a restarted worker re-adopts a live PTY.
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
  in `wterm-roost.wasm.sha256`, and `scripts/rebuild-wterm-wasm.sh` reproduces
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
  backoff (1 s → `SYNC_REDIAL_MAX_MS` 30 s, `store/sync-watchdog.ts`) and
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

- **Web:** `apps/web/src/main.tsx` (mount) · `routes.ts` ·
  `store/{root,projector,sync,selectors}.ts` · `connect.ts` (RPC client) ·
  `components/CellTerminal.tsx` + `lib/cellRenderer.ts` ·
  `ws/sync-outbound.ts` + `store/sync-dispatch.ts` (terminal control) ·
  `store/agent-status.ts` + `components/AgentNotificationBridge.tsx` (status +
  notifications)
- **Coordinator:** `apps/coord/src/main.ts` (Bun wrapper) ·
  `coord-factory.ts` (`createCoord`) · `connect/router.ts` +
  `connect/handlers-*.ts` · `connect/auth-interceptor.ts` · `event-log.ts` ·
  `byte-hub.ts` · `connect/session-control.ts` +
  `connect/announced-channel-barrier.ts` (terminal control + barrier) ·
  `buses.ts` · `db/` · `agent-status-hub.ts` + `push-dispatch.ts`
- **Worker:** `apps/worker/src/main.ts` · `session-manager.ts` ·
  `transport/coord-link.ts` · `keeper/multiplexed-main.ts` · `fsm.ts` ·
  `session-terminal-txn.ts` + `session-control-lanes.ts` (viewport
  transaction + lanes) ·
  `agent-status/` (scanner, manifests, report server, integrations)
- **Shared:** `apps/shared/proto/roost/v1/*.proto` · `src/wire/event.ts`
  (`foldEvent`) · `src/wire/agent-status.ts` · `src/cell/` (cell wire +
  emitter) · `src/wterm-core-factory.ts` (pinned core + WASM verification)
