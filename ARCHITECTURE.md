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
     │    · unary            list calls, mutations
     │    · Sync WebSocket   live deltas, cells, compact terminal links
     │    · inputStream      keystrokes (client-streaming RPC)
     │    · scrollback       history, resumable from a byte offset
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
  mutate the store directly. URL is the source of truth for navigation. The
  terminal is `@wterm/dom`, a WASM VT core. Served as static files by the
  coordinator in production.
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

Both sides fold with the **same** `foldEvent` function from `@roost/shared`. So
the server's projection and the browser's view agree by construction, not by
careful hand-mirroring. A reconnecting browser sends the last event id it saw
and gets exactly the events it missed.

## The terminal data plane

**Terminal input and output.** The browser streams keystrokes up a single
long-lived `inputStream` RPC. The coordinator forwards them over the worker's
WebSocket; the worker writes them into the keeper, which writes the PTY. Raw
output returns only as far as the coordinator. The worker sends authoritative
cell-grid snapshots and deltas, while the coordinator derives compact OSC 8
text-to-URI mappings; those cells and links, never raw PTY bytes, cross the
browser `Sync` WebSocket. Only visible panes claim cell delivery. Mounted
offscreen panes keep their last grid without receiving cells, then reclaim an
authoritative snapshot when revealed.

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
practice: every window resize re-reflows history at a new width, and reconnects
duplicate or drop output. After a long fight with that corruption, Roost moved to
the model server-side terminal multiplexers use:

- The **worker** holds the one authoritative grid per session and rebuilds it at
  a single agreed width on resize.
- The **browser** renders that grid as-is and never re-reflows it.
- History is **resumable**: every byte carries a monotonic sequence number, so a
  client asks for "everything since seq N" and the stream splices back with no
  gap and no duplication.

This is the part of the codebase with the most scar tissue; the recurring
failure modes and their fixes are catalogued in `CLAUDE.md`.

## Resilience

- **Worker loses the coordinator** — it reconnects with backoff (500 ms → 30 s);
  PTYs keep running in the keeper; on reconnect it emits a snapshot to
  reconcile.
- **Worker process crashes** — the keeper is a separate process, so PTYs
  survive; the restarted worker reattaches over the UDS and re-adopts open
  sessions.
- **Browser disconnects** — the `Sync` WebSocket reconnects and backfills
  missed events from the last event id; visible terminal panes reclaim the
  current authoritative grid while history remains demand-paged.
- **Coordinator restarts** — workers redial, browsers reconnect, and every
  session is re-projected from the event log. No state is lost because the log
  is the source of truth.

## Entry points

- **Web:** `apps/web/src/main.tsx` (mount) · `routes.ts` ·
  `store/{root,projector,sync,selectors}.ts` · `connect.ts` (RPC client) ·
  `components/CellTerminal.tsx` · `ws/input-channel.ts` ·
  `store/agent-status.ts` + `components/AgentNotificationBridge.tsx` (status +
  notifications)
- **Coordinator:** `apps/coord/src/main.ts` (Bun wrapper) ·
  `coord-factory.ts` (`createCoord`) · `connect/router.ts` +
  `connect/handlers-*.ts` · `connect/auth-interceptor.ts` · `event-log.ts` ·
  `buses.ts` · `db/` · `agent-status-hub.ts` + `push-dispatch.ts`
- **Worker:** `apps/worker/src/main.ts` · `session-manager.ts` ·
  `transport/CoordLink.ts` · `keeper/multiplexed-main.ts` · `fsm.ts` ·
  `agent-status/` (scanner, manifests, report server, integrations)
- **Shared:** `apps/shared/proto/roost/v1/*.proto` · `src/wire/event.ts`
  (`foldEvent`) · `src/wire/agent-status.ts`
