# apps/coord — coordinator control plane (Bun)

The only writer of the SQLite database, and the only process both workers and browsers talk to. Request/response is
Connect-RPC (`CoordinatorService`, POST-only, binary protobuf); the streams are raw Bun WebSockets —
`/ws/coord-worker/:fp` for workers, `/ws/coord-sync` for browsers. Neither is a Connect bidi: Connect bidi cannot
hold a stable full-duplex stream under Bun (see the `src/connect/worker-service.ts` header and
`docs/FAILURE-INDEX.md`).

Path references are relative to `apps/coord/` unless they start at the repo root (`apps/…`, `scripts/…`, `smoke/…`, `docs/…`).

## Entry point — two layers

**`src/main.ts` — the Bun-specific wrapper.** Loads `CoordConfig`, opens `bun:sqlite`, runs `src/db/migrate.ts`
(backing up first if the DB already existed), and, in self-hosted mode, creates or validates the sole internal
account/organization/dashboard before scoped-credential enforcement. It then imports `authorized_keys.roost`,
validates and associates those keys with the tenant, runs the startup janitor, loads the coord Ed25519 key
(`src/coord-key.ts`), builds the `CoordinatorMoveOrchestrator`, calls `createCoord`, then `Bun.serve`. Everything
Bun-only lives here and nowhere else: TLS from `cfg.tlsCertPath`/
`cfg.tlsKeyPath`, `server.requestIP()` → `resolveCallerOrigin`, the SPA static fallback (`src/spa.ts` +
`src/web-embed.generated.ts`), `server.upgrade` for both WebSockets, the ONE multiplexed Bun `websocket` object
dispatching on `ws.data.kind`, `idleTimeout: 120`, and SIGTERM/SIGINT shutdown.

**`src/coord-factory.ts::createCoord(deps)` — the portable protocol layer.** Returns `{ fetch, dispose }`, where
`fetch` is `(Request, CoordHandlerContext?) => Promise<Response>` and touches no Bun API. Owns OPTIONS
preflight, the rate limiter, Connect dispatch (`src/connect/bun-handler.ts`), `/api/db-export`, the `/api/*` 404,
the SPA hand-off, the non-Connect audit row, and the security/CORS response wrap; also starts the three
coord-authoritative hubs `src/terminal-title-hub.ts`, `src/last-activity-hub.ts`, `src/agent-status-hub.ts`. A
runtime that cannot read the filesystem injects `ctx.spa`/`ctx.dbExport`.

Listeners: the main one binds `cfg.bind`, default `0.0.0.0:4102` (`ROOST_COORDINATOR_BIND`), with TLS only when both
cert and key paths are set; under `ROOST_TRUST_PROXY=1` that bind must be `127.0.0.1:<port>`, because tailscale
serve fronts it and is then the only trusted source of `X-Forwarded-For`. An optional public edge binds
`cfg.publicBind` (`ROOST_PUBLIC_BIND`), differs from `bind`, and runs either the exact Cloudflare Access policy or
the managed default-deny policy. Public binds are loopback-only except for `ROOST_MANAGED_CONTAINER=1`, whose
strict profile requires `0.0.0.0:4104` inside an unpublished Docker network so only Caddy can reach it. Both
listeners reuse one 4 MiB-bounded WebSocket handler and hijack the two WS paths before `coord.fetch` sees them.
Managed containers reject relocation, export, and deploy at both routing and handler layers.

## The 14 handler domains

`src/connect/router.ts` is **pure wiring**: it installs the auth interceptor and
spreads 14 domain factories into a **single** `router.service(CoordinatorService, {…})`
literal. No handler logic, no per-domain state.

**Load-bearing:** it must stay one `router.service()` call. Connect stubs every absent method with an
unimplemented-throw, so a second `router.service()` call registers a second impl that shadows every method the first
provided. Add a domain with another `...makeXHandlers(deps)` spread, never with another `router.service()`.

| domain | file | surface |
| --- | --- | --- |
| transcription | `src/connect/handlers-transcription.ts` | Deepgram voice-dictation config + short-lived token grant |
| agent-config | `src/connect/handlers-agent-config.ts` | default launch-button agent command, `app_settings`-backed, universal across devices |
| attachments | `src/connect/handlers-attachments.ts` | file read / list-dir + attachment save/list/delete, forwarded to the session's worker |
| mcp | `src/connect/handlers-mcp.ts` | MCP relay CRUD and publication, with a bus delta per mutation |
| auth | `src/connect/handlers-auth.ts` | coord identity, scoped browser/worker grant mint/redeem, pairing flow |
| system | `src/connect/handlers-system.ts` | health, db-export URL, metrics, the SPA diag-log batch sink, state snapshot, audit-log query |
| workspaces | `src/connect/handlers-workspaces.ts` | version-CAS workspace rows, set-sessions, orphan GC |
| tasks | `src/connect/handlers-tasks.ts` | claimable task queue: list/enqueue/next-pending/set-state/cancel |
| workers | `src/connect/handlers-workers.ts` | worker registry list/register/heartbeat/rename/delete + deploy-job start and output stream |
| sessions | `src/connect/handlers-sessions.ts` | session lifecycle + input/resize/cursor-pos; the two scrollback reads live in `src/connect/handlers-sessions-scrollback.ts` and spread into this same object, never a second `router.service()` |
| streaming | `src/connect/handlers-streaming.ts` | only the `sync` stub (below) |
| ui | `src/connect/handlers-ui.ts` | ui-cc relay: `uiReportState`/`uiListStates`/`uiDispatch`. The spatial model stays browser-local; coord relays, never interprets |
| coordinator-move | `src/connect/handlers-coordinator-move.ts` | preflight/start/status over `coord-move/`; plain `Error` from the orchestrator is translated to `ConnectError` here, at the RPC boundary |
| push | `src/connect/handlers-push.ts` | VAPID public key + Web Push subscribe/unsubscribe (`push_subscriptions`) |

## Module map

- `src/connect/` — everything protocol-facing: the 14 handler domains,
  the auth interceptor, both WS transports and their split protocol modules,
  Sync feed/scheduler, terminal view/screen hubs, terminal input lane, worker
  facade, announced-channel barrier and pending spawns.
- SQLite access — `src/db/connection.ts` (Kysely over `kysely-bun-sqlite`, WAL + busy timeout), `src/db/schema.ts` (the `DB`
  interface), `src/db/migrate.ts` (custom runner over `apps/coord/migrations/*.sql`, throws on any failure), `src/db/snapshot.ts`
  (online SQLite copy for db-export and coord move).
- Request middleware — `src/middleware/security.ts` (CSP/CORS/X-Frame-Options + `writeAuditLog`), `src/middleware/caller-origin.ts` (per-listener
  trust chosen at boot from config, never sniffed from headers), `src/middleware/public-surface.ts` (the Cloudflare-Access edge
  listener + its deny lists), `src/middleware/coordinator-availability.ts` (410 for a retired coordinator, except GET and a short
  discovery allow-list), `src/middleware/rate-limit.ts`, `src/middleware/cf-access.ts` (Access JWT + JWKS cache). **There is no
  `loopback-only.ts`** — on-host detection is `src/middleware/caller-origin.ts`, read off `CallerOrigin.onHost`.
- `src/coord-move/` — live coordinator relocation (below).
- `src/router/pending-rpcs.ts` — correlation table for browser→worker RPCs needing a reply; a UUID-keyed entry is
  resolved by the worker's upstream `rpc_ok`/`rpc_error` frame, deadline-bounded.
- Top level: `src/event-log.ts` (append + project + publish), `src/byte-hub.ts` (durable
  worker/channel routing), `src/connect/terminal-view-hub.ts` (browser membership
  and SCD geometry), `src/connect/terminal-screen-hub.ts` (canonical cell replica
  and resumable per-socket cursors), `src/buses.ts` (`BoundedBus<T>`, one per
  non-terminal domain), `src/jwt.ts`, `src/coord-key.ts`, `src/authorized-keys.ts`,
  `src/agent-status-hub.ts` with `src/push-dispatch.ts`/`src/push-sender.ts`/`src/vapid.ts`,
  `src/deploy-jobs.ts` (generic job registry + the POSIX `roost deploy` subprocess),
  `src/backup.ts`, `src/audit-retention.ts`, `src/sse.ts` (`busToAsyncIterable`, consumed by
  the deploy-output stream), `src/presence-hub.ts`, `src/spa.ts` and `src/telemetry.ts`.
Signed Windows-update job bookkeeping lives entirely in `src/windows-update-deploy-jobs.ts`, `src/windows-update-deploy-runtime.ts`,
  `src/windows-update-deploy-record.ts` and `src/windows-update-manifest.ts` — `src/deploy-jobs.ts` does **not** own it.

### `src/coord-move/` — live coordinator relocation

Moves a running coordinator to another machine without losing sessions: the source drains writes behind a write
gate, snapshots SQLite + coord key + `authorized_keys` to the target, stages every worker onto the target URL, waits
for them to reconnect there, then commits — or rolls back, including the phases where the target may already have
self-committed. `src/main.ts` calls `move.recover()` **after** `Bun.serve`, because recovery reads worker liveness from
the worker-WS registry that server populates.

- `src/coord-move/orchestrator.ts` — the SOURCE half plus the `CoordinatorMoveService` interface
  (`preflight`/`start`/`status`/`current`/`recover`/`internal*`/`gate`) and the blocker taxonomy.
- `src/coord-move/target-orchestrator.ts` — the TARGET half plus the plumbing both halves share (handoff lookup, snapshot
  projection, background-run bookkeeping). A base class on purpose, not a collaborator: one `run` mutex is read by
  both `start()` and `internalCommit()`, and `internalAbort()` clears the auto-commit/retry timers.
- `src/coord-move/state.ts` — the 10 `MOVE_PHASES`, the zod-validated `HandoffState`, `isTerminalPhase`, and `HandoffStateStore`:
  durable single-writer JSON at `cfg.handoffPath`, fsyncing file and directory.
- `src/coord-move/write-gate.ts` — `CoordinatorWriteGate`, modes `active | source_draining | target_pending | retired`; `acquire()`
  hands out a lease or throws `Code.Unavailable`, and `beginDrain()` resolves once outstanding leases hit zero.
- `src/coord-move/runtime.ts` — the `CoordinatorMoveRuntime` port (target check/prepare, worker stage/activate/commit/abort,
  snapshot copy, reconnect + wait, target status/commit/abort/health) plus `MoveSnapshot`/`MoveWorker`; types only.
- `src/coord-move/bun-runtime.ts` — the Bun implementation of that port: `src/connect/worker-send.ts` frames, 1 MiB snapshot chunking off
  `src/db/snapshot.ts`, a 3-attempt `fetch` to the target's `/internal/coord-handoff/*`, worker reconnect via close.
- `src/coord-move/internal-http.ts` — the target-side surface for `/internal/coord-handoff/{status,commit,abort}`. Credentials ride
  `x-roost-handoff-id` + `x-roost-handoff-secret`; unknown id or bad secret is 401, a precondition failure is 412,
  because `abortTarget` branches on that distinction.

## The split transports

- `src/connect/sync-ws-handler.ts` is the browser Sync firehose **entry point and socket wiring only**: upgrade plus
  JWT-in-`roost-auth`-subprotocol auth, the `?since=<eventId>` backfill cursor, the 30 s `KEEPALIVE_INTERVAL_MS`,
  and the `open`/`message`/`drain`/`close` callbacks. The protocols it multiplexes sit beside it —
  `src/connect/sync-ws-deadline.ts` (Access reauth deadline, re-armed across the platform timer maximum),
  `src/connect/sync-ws-v1-delivery.ts`, `src/connect/sync-ws-v2-state.ts` (per-socket vocabulary + generation allocator),
  `src/connect/sync-ws-v2-scheduler.ts` (weighted lane egress), `src/connect/sync-ws-v2-commands.ts` (the only path by which a browser frame
  mutates coordinator state).
- **The v1 ACK-windowed path is supported back-compat, not dead code.** `apps/coord/tests/sync-ws-keepalive.test.ts`
  asserts "legacy sockets remain unsequenced and unenforced". Do not delete `src/connect/sync-ws-v1-delivery.ts` without the
  deployed-client story.
- `src/connect/handlers-streaming.ts` keeps **only** the `sync` RPC stub, which throws `Code.Unimplemented` **by
  design** — the real transport is the WebSocket above, and `src/main.ts` additionally returns 410 for that path at the
  fetch layer so Connect never opens the stream. The feed is `src/connect/sync-feed.ts` (bus subscription + durable
  event paging), `src/connect/sync-feed-frames.ts` (stateless payload→`FirehoseFrame` adapters + lane metadata) and
  `src/connect/sync-feed-seed.ts` (retained-snapshot seeding).
- Sync v2 advertises exactly seven application domains: terminal, workers, workspaces, tasks, MCP, pair, and audit.
  Audit alone is lazy; every other domain is subscribed at socket creation.
- `src/connect/session-control.ts` is a **re-export barrel** over
  `src/connect/terminal-control-lane.ts` (per-session ordering and generation cancellation)
  and `src/connect/input-control.ts` (including its bounded audit queue). Terminal view
  membership lives only in `src/connect/terminal-view-hub.ts`; terminal cell continuity
  lives only in `src/connect/terminal-screen-hub.ts`. The barrel owns no session lifecycle.

## Invariants

- **Post-commit publication order is load-bearing.** `src/event-log.ts::appendEvent` inserts into `events` and folds the
  `sessions` projection in one transaction, then publishes strictly after commit, in this order:
  `applyDurableChannelIndex` (the `src/byte-hub.ts` channel index), then `sessionBus`. Publishing from inside the
  transaction raced Sync fan-out ahead of the binding, so a tab could observe `opened`/`respawned`/`snapshot` before
  its worker/channel route existed and send the first claim or keystroke into a channel no keeper owned. Do not
  reorder it, and never mutate the channel index from a `sessionBus` subscription.
- **`_channelToSession` is private to `src/byte-hub.ts`.** Terminal hubs receive an
  already-resolved session ID through their narrow routing APIs; they never read
  or mutate the channel index.
- **Every mutation whose domain has a `*Bus` must publish after its DB write**, in that domain's own
  `handlers-<domain>.ts`: `publishTaskState(row)` in `src/connect/handlers-tasks.ts`, `workspaceBus.publish` in
  `src/connect/handlers-workspaces.ts`, and `mcpBus` in `src/connect/handlers-mcp.ts`. Omit it and the write lands
  while every other browser shows stale state until reload.
- **`writeAuditLog` runs inside the auth interceptor's `try/finally`.** `src/connect/auth-interceptor.ts` is the
  only place a verified `caller_fp` exists, so the per-RPC row must be written there; the same `finally` releases
  the coordinator write lease. Non-Connect paths audit in `src/coord-factory.ts` with `callerFp: null`.
- **Rate limiting matches exact mutation routes, never a path prefix.** `src/middleware/rate-limit.ts` keys on a `ReadonlySet` of
  full RPC paths, so `*List`/`*Read` calls cannot burn the mutation budget. Connect emits every unary RPC as POST,
  so the GET/HEAD/OPTIONS early return in `checkRateLimit` never fires for an RPC — the exact-name set is the guard.
- **`events` is append-only; `sessions` is a projection of it.** Never edit an event row. Session state changes by
  appending an event and letting `foldEvent` (shared with the SPA through `@roost/shared/wire`) recompute the row,
  so browser and coordinator projections agree by construction. Closed sessions are deleted, not parked; live `open`
  rows are never reaped on a wall-clock cutoff.

## Testing

- `bun test apps/coord/tests/` — 48 files, 299 tests. `tests/coord-e2e.test.ts` boots a coordinator through `createCoord`
  against in-memory SQLite and drives `coord.fetch(...)` directly: no `Bun.serve`, no port allocation, no network.
- The coordinator half of the terminal flow is pinned by
  `tests/terminal-view-hub.test.ts`, `tests/terminal-screen-hub.test.ts`,
  `tests/sync-ws-v2-scheduler.test.ts`, `tests/coord-bidi.test.ts`,
  `tests/durable-publication.test.ts`, `tests/announced-channel-barrier.test.ts`,
  `tests/sync-ws-keepalive.test.ts`, `tests/sync-ws-deadline.test.ts`,
  `tests/worker-ws-transport.test.ts` and `tests/worker-bidi-event.test.ts`. The browser end
  is `smoke/terminal/*.spec.ts` (`bun run test:terminal`).
- `bun run test:unit` runs the fast tier across all apps; `bun run lint` enforces the 400-line file cap and the
  `console.*` ratchet.
- Run it with `bun apps/coord/src/main.ts` (env parsed by `CoordConfig` in `apps/shared/src/config.ts`); install as
  a service with `bash apps/coord/scripts/install.sh install` (launchd on macOS, systemd --user on Linux).
