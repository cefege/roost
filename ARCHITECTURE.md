<!-- AUDIENCE: human -->
# Architecture

A tour of how Roost fits together. For build/run, see
[`GETTING_STARTED.md`](GETTING_STARTED.md); for terms,
[`GLOSSARY.md`](GLOSSARY.md). The exhaustive in-repo reference (for LLM
collaborators) is [`CLAUDE.md`](CLAUDE.md).

Roost is three TypeScript apps on [Bun](https://bun.sh), plus a shared wire
package. Everything speaks one RPC framework end to end
([Connect-RPC](https://connectrpc.com) + protobuf).

```text
  Browser  (Solid SPA, any device on your tailnet)
     │
     │  Connect-RPC over HTTP/2, protobuf binary
     │    · unary            list calls, mutations
     │    · Sync stream      live deltas for 8 domains in one connection
     │    · inputStream      keystrokes (client-streaming)
     │    · scrollback       history, resumable from a byte offset
     ▼
 ┌────────────────────────────────────────────┐
 │ Coordinator  (Bun · one Mac · :4102)        │
 │   append-only `events` table  (SQLite)      │
 │     → projected into a `sessions` table     │
 │   EdDSA-JWT auth · fans deltas to every tab │
 └────────────────┬───────────────────────────┘
                  │  raw WebSocket · protobuf frames · worker dials outbound
     ┌────────────┼────────────────────┐
     ▼            ▼                     ▼
  Worker       Worker                Worker     (Bun · one per Mac)
  Mac A        Mac B                 Mac C
     │  a keeper subprocess hosts every PTY over one UDS and
     │  outlives worker restarts; hooks (Claude Code) or
     │  screen-scrape (pi, oh-my-pi) track each agent's state
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
  the coordinator, owns the PTYs (via a single multiplexed keeper subprocess),
  runs a per-channel state machine, and turns each agent's hooks (Claude Code) or screen-scraped TUI state (pi, oh-my-pi) into agent
  events. There is no inbound port on a worker.
- **Shared** (`apps/shared/`) — the wire source of truth: protobuf definitions,
  generated TS, Zod schemas, and the `foldEvent` reducer (below).

## Event sourcing is the spine

A session's state is never sent as a snapshot that can drift. The worker emits
small `SessionEvent`s (opened, attached, cwd changed, agent state changed,
closed). The coordinator appends each to the `events` table and folds it into
the `sessions` projection inside one SQLite transaction. The browser folds the
same events into its Solid store.

Both sides fold with the **same** `foldEvent` function from `@roost/shared`. So
the server's projection and the browser's view agree by construction, not by
careful hand-mirroring. A reconnecting browser sends the last event id it saw
and gets exactly the events it missed.

## Two hot paths

**A. Terminal bytes (keystroke → screen).** The browser streams keystrokes up a
single long-lived `inputStream` RPC. The coordinator forwards them over the
worker's WebSocket; the worker writes them into the keeper, which writes the
PTY. Output flows back the same way and rides the `Sync` stream's bytes channel
out to every browser watching that session.

**B. Agent state (agent turn → sidebar).** Claude Code fires hooks; pi and oh-my-pi are screen-scraped from the rendered grid.
The worker turns those into `agent` `SessionEvent`s, which travel up as
proto frames, get appended + projected by the coordinator, and reach the SPA on
the `Sync` stream. The sidebar chip (working / needs-input / idle, plus model
and cost) is a selector over that projected state.

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
- **Browser disconnects** — the `Sync` stream reconnects and backfills from the
  last event id; scrollback re-fetches from the last byte offset.
- **Coordinator restarts** — workers redial, browsers reconnect, and every
  session is re-projected from the event log. No state is lost because the log
  is the source of truth.

## Entry points

- **Web:** `apps/web/src/main.tsx` (mount) · `routes.ts` ·
  `store/{root,projector,sync,selectors}.ts` · `connect.ts` (RPC client) ·
  `components/CellTerminal.tsx` · `ws/input-channel.ts`
- **Coordinator:** `apps/coord/src/main.ts` (Bun wrapper) ·
  `coord-factory.ts` (`createCoord`) · `connect/router.ts` +
  `connect/handlers-*.ts` · `connect/auth-interceptor.ts` · `event-log.ts` ·
  `buses.ts` · `db/`
- **Worker:** `apps/worker/src/main.ts` · `session-manager.ts` ·
  `transport/CoordLink.ts` · `keeper/multiplexed-main.ts` · `claude/hooks.ts` ·
  `detect/` (agent screen-scrape) · `fsm.ts`
- **Shared:** `apps/shared/proto/roost/v1/*.proto` · `src/wire/event.ts`
  (`foldEvent`)
