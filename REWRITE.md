<!-- AUDIENCE: human -->

> **Status (2026-07-30):** R0–R10 = the completed rewrite roadmap and are historical in full: prose, code sketches, APIs, state models, tests, and deliverables below may describe retired architecture, including `AgentState`, ClaudeBridge, hooks, screen-scraping, Swarm/status UI, and direct worker WSS. None is a current product promise. **R11 (cell-shipping terminal model, §R11 below) is LIVE architecture**, not a plan — see `apps/shared/src/cell/`. Roost is terminal-only: every live session is a shell PTY, including sessions that run agent CLIs. There is no structured agent/session API, HTML transcript, approval UI, or Roost-managed OMP dependency. Live truth is `CLAUDE.md` + its L11 index. The "throw it away / rewrite" framing below is the 2026-06-11 starting point, not current guidance.
# Roost v2 — rewrite plan (FINAL, post-audit 2026-06-11)

The current codebase is a 23-phase accretion of features built without a shared
spec. The audit found 75 staleness bugs in the SPA state layer alone; the same
shape exists across worker + coord. The product is not in production. No users.
**Throw it away. Rewrite for Claude-Code throughput.**

This document is the spine. Every decision is evidence-cited. No flip-flopping.

---

## R-INDEX

- **R0** core decisions (locked, evidence-cited)
- **R1** keep/throw line-items
- **R2** architecture (one diagram)
- **R3** data model
- **R4** phases
- **R5** invariant tests
- **R6** kill-switches
- **R7** what this buys
- **R8** in-scope-v2 vs deferred features
- **R9** completed verification passes

---

## R0 — CORE-DECISIONS (locked)

### R0.1-ONE-LANGUAGE-ONE-RUNTIME — Bun + TypeScript

**Runtime: Bun v1.3.14+** (Anthropic-owned since 2025-12-02,
https://bun.com/blog/bun-joins-anthropic). One tool replaces Node + pnpm +
esbuild + tsx + node:sqlite + ws library.

**Per-app stack**:
- **Worker**: Bun + `Bun.Terminal` (PTY API, stable since v1.3.5)
- **Coord**: Bun + H3 (official Bun adapter) + bun:sqlite + Kysely via
  `dylanblokhuis/kysely-bun-sqlite` dialect
- **SPA**: Vite + `vite-plugin-solid` + `@solidjs/router` 0.16.x

**Why Bun**:
1. Anthropic ownership → strategic alignment with Claude Code, zero funding risk
2. Bun ships `llms.txt` at https://bun.sh/llms.txt — direct AI fluency signal
3. `Bun.Terminal` eliminates node-pty native binary sidecar
4. One tool replaces five → R0.8 ONE-CLI simplification
5. Built-in `Bun.serve` WebSocket → no `ws` library
6. `trpc-bun` native adapter for tRPC v11
7. `bun test` + fast-check officially supported

**Concerns + mitigations**:
- Rust rewrite v1.3.14 (2026-05-13) is 1 month old → pin `.bun-version`; smoke
  test in R4.-1 before any rewrite work
- Playwright runs on Bun in compat mode → **run Playwright on Node**
  separately; server-under-test is Bun, test driver is Node, they speak HTTP/WS
- oven-sh/bun#5774 (child process.exit kills parent) → verified in R4.-1 smoke

### R0.2-ONE-WIRE-SPEC — Connect-RPC + protobuf (post-crpc6)

- **`@connectrpc/connect` v2** for ALL RPCs over HTTP/2:
  - Unary: `coordClient.X({...})` — protobuf binary on the wire
  - Server-streaming: `coordClient.sync({ sinceEventId })` firehose +
    `coordClient.scrollback({ session_id, last_seq })`
  - Client-streaming: `coordClient.inputStream(asyncIter)` for keystrokes
  - Bidi: `WorkerService.Attach` — worker dials outbound at boot;
    in-band JWT rotation via `WRefreshJwt` so the stream stays open
    for hours _(SUPERSEDED: reverted to a raw Bun WebSocket — Bun can't hold
    a Connect bidi; see CLAUDE.md L11 + the R2 banner)_
- All wire types defined in `apps/shared/proto/roost/v1/*.proto`. Generated
  TS at `apps/shared/src/gen/roost/v1/*_pb.ts` via `buf generate`.
- In-app shape stays Zod (`apps/shared/src/wire/`) for ergonomics +
  branded types. `src/wire/event-proto.ts` adapts between Zod and proto
  for typed `FirehoseFrame.session_event` variants (opened/closed/
  attached/detached/cwd/workspace_assigned).
- tRPC + raw `/ws/coord-worker` + `/ws/browser-input` + `/api/scrollback`
  all RETIRED in crpc6 / T1.1. Pure Connect end-to-end.

### R0.3-EVENT-SOURCED-SESSIONS

Coord has ONE `events` table (append-only `SessionEvent` rows). The `sessions`
table is a materialized projection. Browser state is also a projection. Truth
flows: worker emits event → coord appends → coord projects → coord broadcasts
→ browser projects. No store mutates independently. The m5 wedge class is
**mechanically impossible** because there are no parallel mutators.

### R0.4-ONE-STORE-WEB — Solid native single root

Single `createStore`-backed root store on the web. Many derived selectors.
Components subscribe to selectors, never mutate. **Solid native primitives**,
NOT Zustand (which is React-shaped). The audit-derived helpers
(`isSnapStaleByKind`, `isSnapStaleByTimestamp`) get absorbed into the
projection: stale-ness is impossible because there's one fold.

### R0.5-STATECHART-LIFECYCLE

Channel lifecycle is a finite state machine. States: `spawned → attached →
agent-running → agent-needs-input → agent-idle → closed`. Transitions are
explicit. Channel close is fired by ONE place.

**Library choice**: hand-rolled FSM (~50 LOC) or `@xstate/store` (<1 kB) — NOT
full XState (~16.7 kB overkill for 6 states).

### R0.6-INVARIANT-TESTS-IN-CI

- `bun test` + fast-check for property tests (worker + coord)
- `bun test` for SPA unit tests (Vite still drives dev HMR + build)
- **Playwright on Node** for real-flow e2e (separate package.json script)
- A phase is done when its invariants pass. "Feature works" is not enough.

### R0.7-PORT-KEEPERS-TO-TS

Per-session detached Bun subprocess keeper. Survives R-PTY-1 (browser
disconnect), R-PTY-2 (worker restart), R-PTY-3 (multi-viewer).

**Shape**: `apps/worker/src/keeper/main.ts` is spawned per session via
`Bun.spawn({detached:true, stdio:'ignore'})` + `.unref()`. Each keeper owns
its PTY via `Bun.Terminal`, exposes a UDS protocol the worker connects to.
Multi-viewer = N WS subscribers to worker's PTY-byte fan-out.

**~800 TS LOC** (port of ~1500 Rust LOC). Multi-viewer supported natively.

**Server-side multiplexers rejected** with evidence: control-mode is another terminal emulator's 10-year bug
source; vendoring saves nothing over `Bun.spawn` + `Bun.Terminal`.

### R0.8-ONE-CLI — `bun run roost`

`apps/roost-cli/` exposes `roost` via package.json `bin` field. Invocation:
`bun run roost <subcommand>` from root.

**Subcommands** (concrete):
- `roost dev` — start coord (:4102) + worker (:2224) + SPA Vite dev (:5174)
- `roost test` — `bun test` for worker/coord/shared + SPA +
  `node ./node_modules/.bin/playwright test` for e2e (sequential, dep-ordered)
- `roost deploy <tailnet-host>` — SSH+rsync worker.ts + package.json +
  bun.lockb to `~/Library/Application Support/RoostWorker/`; restart
  LaunchAgent via `ssh $host launchctl kickstart -k gui/$UID/com.roost.worker`
- `roost logs <app>` — tail `~/Library/Logs/Roost{Worker,Coord}/main.{out,err}.log`
- `roost reset` — stop LaunchAgents, wipe `coordinator_v2.db`,
  `authorized_keys.roost`, `coord_verifying_key`, `bun install`

### R0.9-DECLARATIVE-SCHEMA — Kysely + hand-written SQL migrations

- `kysely` v0.29.2+ as typed query builder
- `dylanblokhuis/kysely-bun-sqlite` dialect for bun:sqlite (matches R0.1
  runtime)
- Hand-written `.sql` migration files via custom MigrationProvider
- Kysely ships `llms-full.txt` for direct Claude fluency

**Mandatory operational wrapper** (codified):
```ts
// apps/coord/src/db/migrate.ts
const result = await migrator.migrateToLatest()
if (result.error) throw new Error(`migration failed: ${result.error}`)
```
(Kysely's `migrateToLatest()` does NOT throw on failure — issue #1008.)

### R0.10-CI-ON-EVERY-COMMIT

GH Actions runs `roost test` on every push. Branch protection requires green.

### R0.11-STATE-DOT-MD

`STATE.md` at repo root, auto-updated by `Stop` hook after each session.

**Concrete hook command** (in `.claude/settings.json`):
```json
{
  "hooks": {
    "Stop": "bash -c 'cd /Users/you/Code/roost && bun run roost state > STATE.md'"
  }
}
```

`roost state` outputs: current branch, last 5 commits, git status summary,
last test run result, current open work (from REWRITE.md task tracker if
present), known broken items.

### R0.12-TYPED-CONFIG

One Zod schema in `apps/shared/config.ts`. Carried fields (concrete):
- `coordinatorUrl: string` (worker → coord URL)
- `bootstrapToken: string | undefined` (one-shot first-boot worker registration)
- `reachableAddr: string` (worker's tailnet FQDN for browser direct WS)
- `wsListenPort: number` (default 2224)
- `wsScheme: 'ws' | 'wss'` (auto-detected from cert presence)
- `tlsCertPath, tlsKeyPath: string | undefined`
- `coordVerifyingKeyPath: string` (pin)
- `label: string` (worker display name)

Fails at boot if invalid.

### R0.13-DEPLOY-IS-RSYNC

Bun runs `.ts` directly. Deploy = rsync `worker.ts` + `package.json` +
`bun.lockb` to target; `bun install --production`; restart LaunchAgent.

**No native binary sidecar** (`Bun.Terminal` replaces node-pty).
**No bundle step required** (Bun runs TypeScript natively).

### R0.14-LOAD-BEARING-PROTOCOL-ONLY

Wire spec starts with the load-bearing subset only. 14/27 ControlRequest dead
+ 7/20 ControlPush dead → not ported. ~50% protocol surface reduction.

### R0.15-CORRELATION-IDS

Every event/log/HTTP call carries a `trace_id`. **Concrete propagation**:
- HTTP header: `x-roost-trace-id`
- tRPC context: `ctx.traceId`
- WS frame: `traceId` field on Zod-validated control frames
- Log lines: `{ts, level, trace_id, ...}` JSON (Bun structured logging)

Browser generates per user action. Worker + coord propagate via the same
header/field. `grep trace_id=abc123 ~/Library/Logs/Roost*/*.log` reconstructs.

### R0.16-BRANDED-IDENTITY-TYPES

`WorkerFp`, `SessionId`, `ChannelId`, `WorkspaceId`, `TraceId` are all
branded TS types (`string & { readonly __brand: '...' }`). Constructors
validate at creation. Mixing them = `tsc` error. Zero runtime cost.

### R0.17-CHANNEL-LIFECYCLE-BUS

ONE `channelLifecycle.on(e => …)` bus. Every store registers once at module
init. Adding a new per-channel store = ONE register call. Cleanup is
structural — forgetting becomes impossible.

### R0.18-URL-AS-NAVIGATION-STATE

URL is source of truth for nav state. Replaces three module-level signals.

**URL scheme**:
```
/                                              → redirect to first workspace
/w/:workspaceId                                → workspace view
/w/:workspaceId/t/:channelId                   → terminal tab
/swarm                                         → fleet view
/swarm/t/:workerFp/:channelId                  → swarm-focused tab
/queue                                         → queue/tasks view
/inbox                                         → permission inbox view
/settings/(keys|workers|workspaces|permissions|webhooks|themes) → panes
/help                                          → help overlay
/file/:workerFp/*path                          → file viewer
/search?q=…                                    → global search
```

Internal only (Tailscale HTTPS via `tailscale serve`). No SEO concern.

---

## R1 — KEEP / THROW (line-item, audit-corrected)

### R1.1-KEEP

- SQLite (now via Kysely)
- Bootstrap-token flow + EdDSA JWT auth (security-reviewed)
- Loopback `/api/authorize-browser` (first-boot self-registration)
- Tap-to-pair (5 endpoints) for second-browser onboarding
- Tailscale FQDN + `tailscale serve` HTTPS front
- wterm renderer (framework-agnostic, untouched)
- `roost-claude` shim + Claude `--settings` hook injection
- Solid 1.x + plain Vite + `@solidjs/router` 0.16.x
- H3 + tRPC SSE subscriptions + tRPC fetch adapter (single port, no
  second ws.Server listener)
- LaunchAgent installers (Bun-flavored, simpler than Rust deploy)
- DriftBadge + git_sha plumbing (just shipped, works)
- Security middleware: CSP, CORS allow-list, X-Frame-Options, audit_log table
- Tasks queue + completion-promise + webhook tokens (see R8)
- Permission rules + auto-decide engine + permission inbox (see R8)
- MCP relay registry (CRUD + SSE fan-out to workers)
- Feature flags (`/api/flags` + ROOST_FLAG_* env merge)
- Admin DB export/import (loopback-only; only backup mechanism)
- Host metrics (in heartbeat body; in-memory cache on coord; sidebar row)
- Snapshot reconciliation (worker re-announces sessions on coord reconnect)
- Sub-agent rows (Task tool spawning)

### R1.2-THROW

- The entire `apps/web/src/lib/stores/` (12 stores) → one root store
- The entire `apps/web/src/lib/*Router.ts` (12 routers) → one event consumer
- `apps/worker/crates/idea-worker/` (Rust, ~18k LOC) → `apps/worker/` (Bun TS)
- `apps/worker/crates/idea-protocol/` → `apps/shared/wire/` (Zod)
- The 1500-LOC Rust PTY keeper subsystem → 800-LOC TS keeper subprocess per R0.7
- `apps/web/server/routes/workspacesV1.ts` + `workspaces_doc` table → v2 only
- 14 dead ControlRequest variants + 7 dead ControlPush variants
- `lib/metrics.ts:13-19` (5 ws_proxy_* dead counters)
- `/api/workers/register` alias mount
- `/api/audit-log` query endpoint (keep table writes for forensics)
- The 4 hand-rolled `if (f.type === "channel-closed")` branches → FSM transition
- Drizzle (rejected in favor of Kysely)
- esbuild bundle pipeline (Bun runs TS directly)
- pnpm + pnpm-lock.yaml (→ bun + bun.lockb)
- tsx loader (Bun native TS)
- ws library (Bun.serve native WSS)
- Scattered shell scripts → single `roost` CLI
- Codesign-on-deploy dance (no native binary to sign)

### R1.3-LOC-DELTA

- Worker: ~18,000 Rust → ~2,000 TS (Rust→Bun port; Bun.Terminal eliminates
  ~500 LOC of PTY plumbing; keeper preserved per R0.7)
- SPA state layer: ~3,500 → ~800 (12 stores+routers collapse to one)
- Coord: ~5,600 → ~3,000 (drop v1 workspaces, dead routes, codegen)
- Protocol crate: ~1,820 → 0 (folded into Zod wire spec)
- Scripts: ~700 bash → ~200 TS in roost CLI
- **Total: ~30k → ~6k LOC**

---

## R2 — ARCHITECTURE (post-Connect + T1.1/T1.4/T2.2/T3.1)

> SUPERSEDED SNAPSHOT — original v2 plan; live truth = CLAUDE.md + its L11
> index. Two reversals since this was drawn: (1) **Worker↔Coord is a raw Bun
> WebSocket** (`/ws/coord-worker/:fp`), NOT Connect bidi `WorkerService.Attach`
> — Bun can't hold a Connect bidi (L11 `project_worker_coord_raw_ws_not_connect_bidi.md`).
> (2) **No `ClaudeBridge`, stream-json transcript parser, hooks, or
> screen-scrape status integration.** Every program, including an agent CLI,
> runs inside a shell PTY and renders in the terminal emulator. `tRPC` /
> `DirectWSS` mentions below are pre-Connect historical.

```
┌──────────────────────────┐                  ┌──────────────────────────┐
│ Browser (Solid SPA, Vite)│                  │ Worker (Bun, per Mac)    │
│                          │                  │                          │
│  store (single root)     │                  │  SessionManager + FSM    │
│   └─ projections         │                  │  ClaudeBridge            │
│   └─ selectors → JSX     │                  │  HookListener (UDS)      │
│  @solidjs/router         │                  │  Keeper subproc(es)      │
│  Connect-Web client      │                  │  Connect-Node CoordLink  │
└────────┬─────────────────┘                  └────────┬─────────────────┘
         │ Connect-RPC over HTTP/2 (protobuf binary)   │
         │                                             │
         │ • unary (workers/sessions/workspaces/…)     │
         │ • server-stream: Sync firehose (8 buses)    │ Connect bidi
         │   + Scrollback                              │ WorkerService.Attach
         │ • client-stream: InputStream (keystrokes)   │ (single long-lived
         │ • EdDSA JWT on every request                │  HTTP/2 stream;
         │                                             │  in-band JWT refresh)
         ▼                                             ▼
┌──────────────────────────────────────────────────────┐
│ Coord (Bun.serve native fetch — no H3, no tRPC)      │
│                                                      │
│  createCoord(deps) factory → fetch(req, ctx?)        │
│  • ConnectRouter (~68 RPCs + Sync + InputStream      │
│    + Scrollback + WorkerService.Attach)              │
│  • Auth interceptor (JWT → caller on ContextValues)  │
│  • OTEL spans per RPC (env-gated by OTLP endpoint)   │
│  • Rate limit, CSP/CORS, audit log                   │
│  • Event log + projector (with _event_id cursor      │
│    so reconnect backfill is gap-free)                │
│  • In-memory BoundedBus per domain                   │
│  • bun:sqlite + Kysely                               │
└──────────────────────────────────────────────────────┘
```

**Transports** (one framework end-to-end):
- Browser ↔ Coord: Connect over HTTP/2, protobuf binary. Unary + server/
  client-streaming. SPA mints EdDSA JWT in WebCrypto; coord verifies via
  interceptor. Reconnect-aware via Sync `since_event_id` cursor (T1.4).
- Worker ↔ Coord: Connect bidi (WorkerService.Attach). Worker dials
  outbound; stream stays open for hours via in-band `WRefreshJwt`
  rotation (T2.2). Proto-typed CoordWorkerUp/Down oneofs.
- Multi-runtime ready: protocol layer in `coord-factory.ts::createCoord`
  is a pure `(Request, ctx?) => Promise<Response>` handler. Bun-specific
  concerns (TLS, server.requestIP, SPA static, file-backed DB) live in
  `main.ts`; other runtimes inject their own `ctx.spa` and `ctx.dbExport`.

---

## R3 — DATA MODEL (atomic, audit-corrected)

A `Session` is the atomic unit. Everything else derives.

```ts
// apps/shared/wire/session.ts — single source of truth

type Worker = {
  fp: WorkerFp                    // hex SHA-256, canonical identity
  label: string
  reachable_addr: string          // tailnet FQDN
  ssh_port: number
  ws_listen_port: number          // direct WS port
  ws_scheme: 'ws' | 'wss'         // for HTTPS mixed-content discrimination
  os: 'darwin' | 'linux'
  git_sha: string | null          // drift badge
  host_metrics: HostMetrics | null  // cpu/mem/disk/net live snap
  registered_at_ms: number
  last_seen_ms: number
}

type Session = {
  id: SessionId                   // uuid
  worker_fp: WorkerFp
  channel: ChannelId              // worker-local PTY id
  kind: 'shell' | 'claude'
  cwd: string
  workspace_id: WorkspaceId | null  // null = orphan (sidebar Inbox)
  status: 'open' | 'closed'
  agent: AgentState | null        // null for plain shell
  created_at: number
  closed_at: number | null
}

type AgentState = {
  kind: 'claude'
  mode: 'default' | 'acceptEdits' | 'plan' | 'bypassPermissions' | 'dontAsk' | 'auto'
  model: string
  status: 'running' | 'needs-input' | 'idle' | 'done'
  tokens: { in: number, out: number, cached: number }
  cost_usd: number                // selectors derive cost chip from this + model
  last_message: { role: 'user'|'assistant'|'thinking', text: string, ts: number } | null
  current_tool: { name: string, input_summary: string } | null
  current_block: { id: number, command: string | null } | null
  permission_request: { id: string, snippet: string, options: string[] } | null
  sub_agents: SubAgentRow[]       // Task-tool spawned children
}

type SubAgentRow = {
  parent_message_id: string
  child_session_id: string
  label: string
  status: AgentState['status']
}
```

### R3.1-EVENT-LOG (audit-corrected with snapshot)

```ts
type SessionEvent =
  | { kind: 'opened',            session_id, worker_fp, channel, kind: 'shell'|'claude', cwd, ts }
  | { kind: 'closed',            session_id, ts, exit_code: number | null }
  | { kind: 'attached',          session_id, ts }
  | { kind: 'detached',          session_id, ts }
  | { kind: 'cwd',               session_id, cwd, ts }
  | { kind: 'agent',             session_id, patch: Partial<AgentState>, ts }
  | { kind: 'workspace_assigned',session_id, workspace_id: WorkspaceId | null, ts }
  | { kind: 'snapshot',          worker_fp, sessions: Session[], ts }
    // ^ worker re-announces all live sessions on coord reconnect.
    //   Coord reconciles: any DB session for this worker NOT in the snapshot
    //   gets a synthetic 'closed' event appended. Replays still deterministic.
```

Coord persists every event. `Session` projection = fold(events). Browser SSE
subscribes to event stream; client folds the same events. Determinism is the
invariant test in R5.2.

---

## R4 — PHASES

### R4.-1 — BUN-SMOKE-TEST (binary go/no-go, ~1 hour)

**BEFORE any rewrite work.** Verifies Bun is viable for our use case.

10-line smoke spec in `smoke/bun_smoke.test.ts`:
1. `Bun.Terminal` PTY round-trip (spawn shell, write, read, resize, close)
2. `Bun.spawn({detached:true, stdio:'ignore'}).unref()` subprocess survives
   parent exit on macOS (verifies oven-sh/bun#5774 is resolved at 1.3.14)
3. `Bun.serve` WSS with Zod-validated frame round-trip
4. `Kysely + dylanblokhuis/kysely-bun-sqlite` SELECT + INSERT + tx rollback
5. `bun test` + fast-check property test passes
6. `H3` route on Bun returns 200
7. tRPC v11 query via `trpc-bun` returns expected shape

**Pass criteria**: all 7 pass at pinned `.bun-version` (v1.3.14).

**Fail fallback (concrete)**:
- Branch `rewrite/node-fallback` exists pre-smoke
- R0.1 reverts to Node + node-pty (prebuilt-multiarch fork)
- R0.7 keeper uses node-pty instead of Bun.Terminal
- R0.13 deploys `worker.mjs` + `node-pty/build/Release/pty.node` sidecar
- Other R0.x decisions unchanged
- Adds ~3 days to rewrite (esbuild bundle + native binary in deploy)

**Deliverable**: `smoke/bun_smoke.test.ts` exists; result documented in
`STATE.md`; LOCK or FALLBACK choice committed.

### R4.0 — SPEC-THE-WIRE (~1 day)
> HISTORICAL ONLY — the `AgentState` wire sketch below was retired. Current
> sessions are shell PTYs and have no structured agent state.


- Create `apps/shared/wire/` with Zod schemas for: Session, Worker,
  AgentState, SessionEvent, ControlFrame, AuthClaim, Workspace, Task,
  WebhookToken, PermissionRule, McpRelay
- Set up tRPC router skeleton (no impls)
- Set up `apps/shared/config.ts` Zod config schema
- **Deliverable**: `bun run typecheck` passes against the spec; `apps/shared/`
  is the source of truth for every wire shape

### R4.1 — NEW-COORD (~4 days)

- tRPC routes replacing the 19 hand-rolled routes
- SessionEventLog: events table + append-only insert + fold helper
- SessionsProjector: consumes log → updates projection in same tx
- tRPC subscriptions: sessionEvents, workers, workspaces, tasks,
  permissionRules, mcpRelayEvents (each = separate SSE endpoint via
  `httpSubscriptionLink`)
- Auth: bootstrap-token + EdDSA JWT (keep current security model)
- Loopback `/api/authorize-browser` (first-boot browser self-registration)
- Tap-to-pair (5 endpoints)
- Tasks queue + completion-promise retry loop + webhook token bearer auth
- Permission rules CRUD + auto-decide on ControlPush::PermissionRequest
- MCP relay registry CRUD + SSE fan-out to workers
- Feature flags (`/api/flags` + env merge)
- Admin DB export/import (loopback-only; only backup path)
- Security middleware (CSP, CORS, X-Frame-Options, audit_log writes)
- Structured logging: `{ts, level, trace_id, ...}` JSON to
  `~/Library/Logs/RoostCoord/main.{out,err}.log`; rotation via macOS
  `log_rotate(8)` config
- DB: Kysely migrations in `apps/coord/migrations/*.sql` (hand-written);
  fresh start (new `coordinator_v2.db`); migrator wrapped to throw on error
- **Invariant tests**:
  - fold(events) determinism (R5.2)
  - Two SSE subscribers see byte-identical event order
  - Snapshot event correctly reconciles ghost sessions
- **Deliverable**: `bun run roost dev:coord` boots; tRPC playground shows all
  routes; can manually POST + see project + broadcast

### R4.2 — NEW-WORKER (~4 days)

> SUPERSEDED plan — see the R2 banner + CLAUDE.md. Shipped reality diverged:
> transport is raw-WS (not tRPC / DirectWSS), and the ClaudeBridge, hooks,
> stream-json parser, and screen-scrape status integration are all gone. Every
> program runs inside a shell PTY and renders in the terminal emulator.

- Bun + Bun.Terminal + Claude bridge in TS
- SessionFSM (hand-rolled per R0.5): one machine per channel; transitions emit
  SessionEvents that POST to coord via tRPC
- ClaudeBridge: spawn claude with `--settings <hooks_json>`, parse stream-json
  stdout, emit AgentState patches (mode/model/tokens/last_message/tool/block/
  sub_agents/permission_request)
- HookListener: UDS server for Claude's --settings hooks
- DirectWSS server (Bun.serve): accepts JWT-auth WSS from browser, multiplexes
  PTY bytes + control frames (Zod-validated)
- **Keeper subprocesses**: `apps/worker/src/keeper/main.ts` spawned per session
  via `Bun.spawn({detached:true, stdio:'ignore'})` + `.unref()`. Each keeper
  owns a `Bun.Terminal`, exposes UDS protocol (frame format: 4-byte BE
  length-prefix + JSON or raw bytes; message types: `attach`, `detach`,
  `input`, `output`, `resize`, `close`).
- LaunchAgent + deploy via `roost deploy <host>`
- Worker emits `snapshot` event on coord reconnect (R3.1)
- Config from Zod schema (R0.12): coordinator_url, bootstrap_token (one-shot),
  reachable_addr, ws_listen_port, ws_scheme, tls_cert_path, tls_key_path
- Structured logging to `~/Library/Logs/RoostWorker/main.{out,err}.log`
- **Invariant tests**:
  - Channel FSM determinism (R5.3)
  - WS drop + reconnect: state rebuilt from events on coord; zero divergence
  - Keeper subprocess survives parent exit (the R4.-1 smoke, now an invariant)
- **Deliverable**: `bun run roost dev:worker` boots; can spawn shell + claude
  via test harness; events arrive at coord

### R4.3 — NEW-WEB (~5 days)
> HISTORICAL ONLY — the selectors, Swarm/status UI, cost chips, and structured
> agent invariants below were retired by the terminal-only cutover.


- Single Solid `createStore`-backed root store
- Selectors: `sessionsByWorkspace`, `sessionsByAgentStatus`, `tokensFor`,
  `modeFor`, `costFor`, `subAgentsOf`, `currentToolOf`, etc.
- Routing via `@solidjs/router` (URL scheme per R0.18)
- Sidebar: All view + Swarm view both read from same `sessions` projection.
  Swarm filters `.where(agent != null)`. All groups by workspace_id with
  Inbox bucket for `workspace_id === null`. **By construction**, every Swarm
  session has a matching All-view row.
- Direct WSS to worker for terminal bytes + interactive control (raw + Zod)
- Settings panes: keys, workers, workspaces, permissions, webhooks, themes
- Onboarding component (first-boot, no workers registered → tap-to-pair or
  bootstrap-token flow)
- Help overlay + keybindings + global search + cwd picker + file viewer
- Cost chip + sub-agent nested rows (read from AgentState selectors)
- Workspace export/import sheet (deferred per R8)
- **Invariant tests** (Playwright on Node):
  - Spawn shell → visible in All within 500ms
  - Spawn claude → visible in Swarm within 500ms
  - Every Swarm session has matching All row (modulo grouping)
  - WS drop + reconnect → sidebar reconciles from coord events; no stale chips
- **Deliverable**: `bun run roost dev` (full stack) boots; UI works end-to-end

### R4.4 — INVARIANTS-IN-CI (~2 days)

- fast-check property tests for SessionEventLog folding
- Playwright real-flow scenarios for sidebar invariants (R5.4) + cross-Mac
  (R5.5)
- GH Actions workflow: `roost test` on every push
- Pre-commit hook: typecheck + unit (no Playwright; that's CI)
- Branch protection: status check 'invariants' required
- **Deliverable**: `.github/workflows/ci.yml` runs; no commit lands without
  green

### R4.5 — CUTOVER (~1 day)

- Stop legacy LaunchAgents (current Rust worker, current TS coord)
- Install new LaunchAgents
- **DB migration script** (concrete, runs as part of cutover):
  - Read tables from existing `coordinator.db`: workers, sessions
    (status='open' only), workspaces, workspace_sessions, bootstrap_tokens
    (used_at_ms IS NULL only), authorized_keys, pair_requests
    (status='pending' only), mcp_relays, permission_rules,
    webhook_tokens, tasks (state IN ('pending','claimed','running') only),
    feature_flags
  - Drop: audit_log (no rows preserved; rewire writes), workspaces_doc
    (already v2)
  - Write into `coordinator_v2.db` via new Kysely schema
  - For each open session: synthesize one `{kind: 'opened', ...}` event at
    t=now so the projection has a starting point
- Move `apps/` → `apps_legacy/`; new apps/ is the v2 tree
- Update `CLAUDE.md` to reflect new architecture
- **Deliverable**: live system runs on v2; legacy still installable from
  `apps_legacy/`; can be deleted later

**Total: ~17 days focused work** (spread over 3-5 calendar weeks).

---

## R5 — INVARIANT TEST LIST

### R5.1-WIRE-SPEC
- Every Zod schema round-trips
- tRPC router type-checks against client usage
- No type aliases resolving to same shape (drift canary)

### R5.2-EVENT-LOG
- fold(random event sequence of length N) is deterministic
- Replaying log from t=0 yields current sessions projection
- Two SSE subscribers see byte-identical event order
- Snapshot event correctly synthesizes 'closed' for ghost sessions

### R5.3-WORKER-CHANNEL-FSM
- Cannot transition `spawned → closed` without `attached` first
- `closed` is terminal
- Every closure emits exactly one `closed` event
- Keeper subprocess survives parent exit (the R4.-1 smoke as invariant)

### R5.4-WEB-SIDEBAR
- Swarm ⊆ All (modulo grouping)
- Spawning creates exactly 1 new tab within 500ms
- Closing removes tab within 500ms; no stale chips
- WS drop + reconnect: state byte-identical before/after
- Mode/model/tokens/cost chips match `selector(session)` for every visible row

### R5.5-CROSS-MAC
- Two workers + one coord: sessions appear with correct worker grouping
- Killing worker A: A's sessions go to status=closed; B untouched
- Restarting worker A + snapshot event: ghost sessions correctly closed; new
  sessions appear

---

## R6 — KILL-SWITCHES

- `R6.1` Each phase commits to its own branch. `main` stays at d4643a85 until
  invariants pass.
- `R6.2` Legacy LaunchAgents stay until R4.5. New apps bind different ports
  (4102 / 2224). For parallel testing: browser uses
  `localStorage["roost.coordinatorUrl"] = "https://coord-host.tailXXXXXX.ts.net:4102"`
  temporarily.
- `R6.3` Legacy `coordinator.db` stays. New uses `coordinator_v2.db`.
- `R6.4` If a phase regresses, revert the branch merge. `main` is always
  functional.
- `R6.5` After R4.5 + 1 week stable: delete `apps_legacy/`.

---

## R7 — WHAT THIS BUYS

### R7.1-FOR-FUTURE-CLAUDE
- One language to grep + edit (TS only)
- One runtime (Bun) — no Node/Rust split
- One schema source (`apps/shared/wire/`)
- One root store (Solid `createStore`)
- One FSM per channel
- Invariant tests catch entire bug classes in CI, not at runtime

### R7.2-FOR-MIHAI
- m5-style wedges mechanically impossible
- New chip = selector + JSX line; ~10 LOC, can't break anything else
- Wire shape change = one file
- URL nav state means bookmarkable terminals + shareable links (desktop ↔ phone)

### R7.3-FOR-VELOCITY
- `bun test` runs in <1s (no cargo, no vitest startup)
- One language = one type system, one debugger, one stack trace
- `bun run roost dev` reloads in <1s
- Adding the next feature = feature-cost, not feature-cost + parallel-tax

---

## R8 — IN-SCOPE-V2 vs DEFERRED (audit-decided)

### R8.1-IN-SCOPE-V2 (must work for v2 cutover)

- Workers registry + heartbeat + drift badge + host_metrics + ws_scheme
- Sessions registry + event log + projection + snapshot reconciliation
- Workspaces v2 CRUD + workspace_sessions junction
- Tasks queue + completion-promise + webhook token auth
- Permission rules engine + permission inbox
- MCP relay registry + SSE fan-out
- Bootstrap-token mint + redeem (worker + browser)
- Loopback browser self-registration + tap-to-pair
- Admin DB export/import (only backup mechanism)
- Feature flags
- Audit log (table writes only; query endpoint dropped)
- Sidebar: All view, Swarm view, Queue view, Inbox view
- Settings panes: keys, workers, workspaces, permissions, webhooks
- Direct WSS PTY byte stream + ControlFrame
- Multi-viewer concurrent (R-PTY-3)
- Mode/model/tokens/cost/last-message/current-tool/current-block/sub-agents chips
- Cwd picker, file viewer, global search, help overlay, keybindings
- DriftBadge + STATE.md auto-update + roost CLI

### R8.2-DEFERRED-TO-V3 (explicitly out of scope)

- Themes pane (use system default for v2)
- Workspace export/import sheet (CAS blob)
- Snapshot system (the old workspace-export thing, not the SessionEvent kind)
- Skill API (worker-side)
- Presence + cursor relay (multi-viewer works without it; cursor relay is QoL)
- WhatsNewDialog
- CompletedTray
- LogViewer component (use `roost logs <app>` CLI)
- MobileVoiceInput
- SessionChangesPane (git diff viewer)

These can land post-v2 without architectural rework. They're not load-bearing.

---

## R9 — COMPLETED VERIFICATION PASSES

### R9.1-DEVILS-ADVOCATE (R4.6, completed 2026-06-11)

A Sonnet agent attempted to refute every R0.x decision with concrete
evidence (GitHub issues, benchmarks, docs). Result: 12 SURVIVED, 0 REFUTED,
6 WEAKENED. The 6 weakened decisions all received evidence-cited caveats and
revisions documented in this file.

### R9.2-FINAL-AUDIT (completed 2026-06-11)

A Sonnet agent read this entire plan end-to-end, identified 15 internal
inconsistencies (stale text from revisions), 8 missing v2-scope features,
and 16 implementation-spec gaps. All resolved in this document.

### R9.3-BUN-SMOKE (R4.-1, PASSED)

Pre-phase smoke verified Bun PTY + detached subprocess + WSS + Kysely.
Live in production across coord + worker + keeper since R4.1+. tRPC
replaced by Connect-RPC in crpc6; node-pty replaced by `Bun.spawn`
`terminal:` in 2026-06-17 keeper refactor. Fallback branch not needed.

---

## R10 — POST-CUTOVER STATUS

R4.-1 through R4.5 all complete. `apps_legacy/` deleted in phase-24g.
Branch `v2` is the live trunk; no queued spine-phase. Active work
tracked per-commit (`phase-<slug>:` prefix); see `STATE.md` for the
current arc. Architectural changes since cutover:

- crpc1-crpc6: tRPC → Connect-RPC + protobuf, ws library → native
  Bun.serve, H3 → bare `Bun.serve` fetch handler
- T1.1 / T1.4 / T2.2 / T3.1: tab-id viewport claims, Sync since-id
  backfill, in-band WRefreshJwt rotation, scrollback as server-stream
- audit-t1 / audit-t2 / mv-tests / scd-shared-grid / scd-scrollback-refetch:
  multi-viewer hardening + smallest-common-denominator viewport

Add a new R-anchor (R11+) before reopening the spine for the next
load-bearing architectural shift.

## R11 — CELL-SHIPPING TERMINAL (cell-grid model) — endgame for history corruption

**R11.0-WHY.** Root cause of "terminal history always fucked up / afraid
to resize" (memory `project_terminal_history_corruption_viewport_slaved_pty`,
CLAUDE.md L11): the worker ships RAW BYTES; the browser re-parses them at
the client's width via `@wterm/core` WASM. **Re-parse-at-new-width IS the
corruption** — @wterm/core's row resize is asymmetric/lossy (shrink→scrollback,
grow→blanks, never reverses). Stop-bleeds shipped (hold-anchor hysteresis
`claimHysteresis.ts`; alt-screen freeze commit d745b1e3) suppress the
dominant trigger but do not close the class: a deliberate beyond-band resize
still re-derives. No library reflows a TUI grid to a new width (they all
freeze) — so the only structural fix is to stop reflowing on
the client.

**R11.1-MODEL (proven for our exact workload).** Worker owns the ONE
emulator (already true: `rec.wtermCore` fed every byte at
`session-manager.ts:361`, today only used for `serializeWTerm`). Worker ships
**pre-rendered styled CELLS**, not bytes. Client PAINTS cells, never parses
VT, never reflows; surplus viewport is LETTERBOXED. Grid = SCD/min across
viewers (unchanged, `feedback_viewport_scd_min_policy`). Alt-screen carries
NO scrollback (nothing to corrupt). Frozen tradeoff: plain-shell history no
longer reflows to a narrower device → sideways scroll. Accepted by Author
2026-06-22 ("balls to the wall, full cell-grid").

**R11.2-WIRE.** `apps/shared/src/cell/` is the cell source of truth.
`CellSpan{text,fg,bg,flags,fgRgb?,bgRgb?}` (style mirror of
`wterm-serialize` CellData), `CellRow{index,spans[]}`, `CellGridFrame
{cols,rows,cursor{row,col,visible},altScreen,full,viewportRows[],
scrollbackRows[]/scrollbackAppend[],scrollbackTotal,seq}`. Proto: `WCellGrid`
up-frame (worker_transport.proto) + `CellGridFrame` firehose variant
(sync.proto). Full frame on attach/resize; delta = changed viewport rows +
scrollback append + cursor.

**R11.3-PHASES.** cell-phase-1 shared cell types + `gridToCellFrame(core)` +
`diffGrid(prev,next)` (pure, property-tested vs `serializeWTerm`). cell-phase-2
proto + codegen + worker emitter behind per-session `ROOST_CELL_MODE` flag +
coord passthrough bus. cell-phase-3 SPA `CellGridRenderer` (paint spans into
`.term-scrollback-row` DOM, letterbox, apply deltas) + Terminal.tsx
integration behind flag. cell-phase-4 cutover: flip default, live `/roost-smoke`,
retire client reflow + `claimHysteresis` + `scrollbackReplayQueue` + seqno
splice + raw-ring/serialize split + drop browser `@wterm/core` WASM. cell-phase-5
R11 anchor (this) + full suite green.

**cell-phase-5 DONE 2026-08-08** (epoch-addressed viewport-only frames: FULL
frames carry `scrollbackRows=[]` + `sbBase === scrollbackTotal` + `gridEpoch`;
retained history only on reader demand via `SessionsGetScrollbackCells`;
`held_scrollback_total` retired). Evidence, all on the same tree with
`roost-worker` restarted onto it: `bun run typecheck` 5/5 packages; `bun
scripts/lint-roost.ts` 0 violations; `bun run test:unit` 889 pass / 0 fail /
4521 expect() / 118 files; `bun run test:terminal` 32 passed / 2 darwin-only
skipped. Live canaries at `https://ovh1-8c32g.tail67850e.ts.net:4102/` —
`roost-smoke: 5/5` plus real-keyboard `ROOST_LIVE_KEYBOARD_*` echo with
`paneFocused().focused === true`; `render-stress: PASS (80 iters, screen
"main")` on a 60-marker shell and on a 3000-marker deep session; multi-viewer
hammer-A-probe-B and hammer-B-probe-A both `duplicated: []` / `outOfOrder: 0`.
Deep-session contract observed live: cold attach → `atBottom`,
`lastFullFrameSbRows === 0`, `scrollbackBackfillRequestCount === 0`; scrolling
inside the held window issues no RPC, crossing the seam issues demand RPCs and
repaints rows 1..3000 with `missing: 0` and an unchanged `gridEpoch`; a resize
while off-bottom leaves `fromBottom` and the first visible marker untouched
while `cellFullFrameCount` rises, `gridEpoch` changes, and
`lastFullFrameSbRows` stays `0`; return-to-bottom re-latches `atBottom`.
A resize de-materializes rows behind the demand seam (the retired epoch's held
window is dropped, the spacer preserves `scrollHeight`) — reversible, so
`runRenderStress` on `main` must take its baseline after one settle cycle or it
reports `changedRange` with `duplicated: []` / `outOfOrder: 0`.

**R11.4-RISK.** Hottest path; Author runs Claude Code INSIDE an Roost keeper
PTY (`feedback_claude_code_runs_inside_roost_keeper_pty`). Build behind the
flag; NEVER flip default / delete byte path until the full bun suite + live
humanchrome smoke are green. Worker `roost push`/kickstart is keeper-safe;
no unilateral keeper restart.
