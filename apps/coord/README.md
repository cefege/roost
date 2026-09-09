# apps/coord — coordinator control plane (Bun)

The only writer of the SQLite database, and the only process both workers and browsers talk to. Request/response is
Connect-RPC (`CoordinatorService`, POST-only, binary protobuf); the streams are raw Bun WebSockets —
`/ws/coord-worker/:fp` for workers, `/ws/coord-sync` for browsers. Neither is a Connect bidi: Connect bidi cannot
hold a stable full-duplex stream under Bun (see the `src/connect/worker-service.ts` header and
`docs/FAILURE-INDEX.md`).

Path references are relative to `apps/coord/` unless they start at the repo root (`apps/…`, `scripts/…`, `smoke/…`, `docs/…`).

## Entry point — three layers

**`src/main.ts` — process composition.** Loads `CoordConfig`, opens
`bun:sqlite`, migrates/backfills tenant state, imports self-hosted authorized
keys, starts maintenance owners, constructs the coordinator write gate, and
constructs auth, Connect, worker-WS, and Sync-WS dependencies. It calls
`createCoord()`, then `startBunCoordinatorListeners()`.

**`src/bun-coordinator-listeners.ts` — the Bun listener boundary.** Owns the
single `Bun.serve` call, `server.requestIP()` → `resolveCallerOrigin`, the
`/api/db-export` route (its only special non-Connect route), both WebSocket
upgrades, the ONE multiplexed
`websocket` object dispatching on `ws.data.kind`, the shared 4 MiB frame cap,
and `idleTimeout: 120`. The SPA fallback is injected from `src/spa.ts` +
`src/web-embed.generated.ts`.

**`src/coord-factory.ts::createCoord(deps)` — the portable protocol layer.** Returns `{ fetch, dispose }`, where
`fetch` is `(Request, CoordHandlerContext?) => Promise<Response>` and touches no Bun API. Owns OPTIONS
preflight, the rate limiter, Connect dispatch (`src/connect/bun-handler.ts`), `/api/db-export`, the `/api/*` 404,
the SPA hand-off, the non-Connect audit row, and the security/CORS response wrap; also starts the three
coord-authoritative hubs `src/terminal-title-hub.ts`, `src/last-activity-hub.ts`, `src/agent-status-hub.ts`. A
runtime that cannot read the filesystem injects `ctx.spa`/`ctx.dbExport`.

The listener binds `cfg.bind` (`ROOST_COORDINATOR_BIND`), default
`127.0.0.1:4103`, and always serves plaintext: TLS, DNS, and public
reachability belong to the front door the operator puts in front of it. Under
`ROOST_TRUST_PROXY=1` the bind must be `127.0.0.1:<port>`, because that proxy is
then the only trusted source of `X-Forwarded-For`, whose FIRST entry
`src/middleware/caller-origin.ts` reads as the caller address. The coordinator
carries no public deny list: the front door owns that policy (see
`GETTING_STARTED.md`), denying `/internal/*` and `/api/db-export` while
`/ws/coord-worker/*` stays open for workers that dial the same origin.
`dbExportResponse` answers 403 `{"error":"on-host only"}` unless the resolved
caller is on-host, which makes it the one request that proves whether
`X-Forwarded-For` reaches the coordinator intact.

## The 16 handler domains

`src/connect/router.ts` is **pure wiring**: it installs the auth interceptor and
spreads 16 domain factories into a **single**
`router.service(CoordinatorService, {…})` literal. No handler logic or
per-domain state lives there.

**Load-bearing:** it must stay one `router.service()` call. Connect stubs every absent method with an
unimplemented-throw, so a second `router.service()` call registers a second impl that shadows every method the first
provided. Add a domain with another `...makeXHandlers(deps)` spread, never with another `router.service()`.

| domain | file | surface |
| --- | --- | --- |
| transcription | `src/connect/handlers-transcription.ts` | dashboard-admin Deepgram config/get/set/test + stored-key handoff |
| agent-config | `src/connect/handlers-agent-config.ts` | default launch-button agent command, `app_settings`-backed, universal across devices |
| agent-status | `src/connect/handlers-agent-status.ts` | dashboard-authorized volatile status Get/List and occupant-pinned event waits |
| agent-prompt | `src/connect/handlers-agent-prompt.ts` | dashboard-authorized `SessionsPrompt`, exact observed-status admission, and optional post-input status wait |
| attachments | `src/connect/handlers-attachments.ts` | worker-forwarded read/read-chunk/list/mkdir + attachment upload/probe/list/delete |
| mcp | `src/connect/handlers-mcp.ts` | MCP relay CRUD and publication, with a bus delta per mutation |
| auth | `src/connect/handlers-auth.ts` | facade over `src/connect/handlers-auth-bootstrap.ts`, `src/connect/handlers-pairing.ts`, and `src/connect/handlers-devices.ts`: identity/access, bootstrap redemption, pairing, device rotation/revocation, logout |
| worker-update | `src/connect/handlers-workers-update.ts` | the coordinator-held keeper update boundary: drain, reauthorize, canonical open-session snapshot, then the authenticated worker action |
| system | `src/connect/handlers-system.ts` | health, db-export URL, metrics, the SPA diag-log batch sink, state snapshot, audit-log query |
| workspaces | `src/connect/handlers-workspaces.ts` | version-CAS workspace rows, set-sessions, orphan GC |
| tasks | `src/connect/handlers-tasks.ts` | claimable task queue: list/enqueue/next-pending/set-state/cancel |
| workers | `src/connect/handlers-workers.ts` | registry lifecycle; composes deploy start/output from `src/connect/handlers-workers-deploy.ts` |
| sessions | `src/connect/handlers-sessions.ts` | list/attach/kill/rename/input/cursor/assignment plus owning-worker-only private recovery metadata; composes spawn from `src/connect/handler-session-spawn.ts`, terminal cell/search/cancel RPCs from `src/connect/handlers-sessions-scrollback.ts`, and authorized global terminal search from `src/connect/handlers-sessions-global-search.ts`; resize is socket-bound |
| streaming | `src/connect/handlers-streaming.ts` | only the `sync` stub (below) |
| ui | `src/connect/handlers-ui.ts` | typed per-tab reports/listing, bounded composition-owned TTL retention in `ui-state-owner.ts`, canonical legacy-command admission in `ui-legacy-command.ts`, and exact bounded fingerprint/tab apply admission in `ui-layout-apply-owner.ts`; the spatial model stays browser-local |
| push | `src/connect/handlers-push.ts` | VAPID public key + Web Push subscribe/unsubscribe (`push_subscriptions`) |

## Module map

- `src/connect/` — everything protocol-facing: the 16 handler domains and
  focused facade leaves, auth interceptor, both split WS transports, Sync
  feed/scheduler, terminal view/screen hubs, raw terminal input lane, guarded
  agent-prompt orchestration, worker facade, announced-channel barrier, and
  pending spawns.
- SQLite access — `src/db/connection.ts` (Kysely over `kysely-bun-sqlite`, WAL + busy timeout), `src/db/schema.ts` (the `DB`
  interface), `src/db/migrate.ts` (custom runner over `apps/coord/migrations/*.sql`, throws on any failure), `src/db/snapshot.ts`
  (online SQLite copy backing `/api/db-export`).
- Request middleware — `src/middleware/security.ts` (CSP/CORS/X-Frame-Options + `writeAuditLog`), `src/middleware/caller-origin.ts`
  (listener trust chosen at boot from config, never sniffed from headers: `direct` or `trusted-proxy`),
  and `src/middleware/rate-limit.ts`. On-host detection is
  not a standalone middleware: `src/middleware/caller-origin.ts` owns
  `CallerOrigin.onHost`.
- `src/coordinator-write-gate.ts` — the keeper-update write fence (below).
- `src/router/pending-rpcs.ts` — correlation table for browser→worker RPCs needing a reply; a UUID-keyed entry is
  resolved by the worker's upstream `rpc_ok`/`rpc_error` frame, deadline-bounded. `src/connect/global-search-cursors.ts`
  owns bounded per-router, device/tab/dashboard/options-bound global-search
  continuations, cumulative progress, active searches, and cancellation
  tombstones; `src/connect/global-search-options.ts` clamps public page limits.
  `src/connect/global-search-worker-lanes.ts` bounds and serializes the
  server-wide per-worker search lane; `src/connect/global-search-fanout.ts`
  owns authorized session selection, fair one-batch-per-worker partitioning,
  and strict result validation. `src/connect/global-search-cancel.ts` owns the
  tombstone-before-discovery cancellation path.
- `src/connect/ui-layout-apply-owner.ts` owns the sole acknowledged UI command's
  exact target-tab/live Sync-v2 socket selection, correlation registration
  before publication, result fencing, and
  cancellation/close/replacement/timeout settlement.
- Top level: `src/event-log.ts` (stable event facade),
  `src/event-transaction.ts` (durable append/projection transaction),
  `src/pending-event-publications.ts` (bounded post-commit recovery and ordered
  publication), `src/byte-hub.ts` (durable worker/channel routing),
  `src/agent-conversation-recovery.ts` (sequence-aware private reference
  projection), `src/session-event-visibility.ts` (the fail-closed public/private
  event boundary), `src/connect/session-list-projection.ts` (separate public and
  owning-worker recovery queries),
  `src/connect/terminal-view-hub.ts` (browser membership and SCD geometry),
  `src/connect/terminal-screen-hub.ts` (canonical cell replica and resumable
  per-socket cursors), `src/buses.ts` (`BoundedBus<T>`, one per non-terminal
  domain), `src/jwt.ts`, `src/authorized-keys.ts`,
  `src/agent-status-hub.ts` (live projection and bounded occupant waiters),
  `src/agent-status-order.ts` (epoch/occupant admission and retirement), and
  `src/agent-status-push-scheduler.ts` (debounced transition pushes).
  Web Push owners are `src/push-dispatch.ts`, `src/push-sender.ts`, and
  `src/vapid.ts`. `src/deploy-jobs.ts` owns the
  generic job registry + POSIX `roost deploy` subprocess; remaining owners are
  `src/backup.ts`, `src/audit-retention.ts`, `src/sse.ts`
  (`busToAsyncIterable`, consumed by the deploy-output stream),
  `src/presence-hub.ts`, `src/spa.ts`, and `src/telemetry.ts`.
  The paused Windows path keeps signed-update bookkeeping entirely in
  `src/windows-update-deploy-jobs.ts`, `src/windows-update-deploy-runtime.ts`,
  `src/windows-update-deploy-record.ts`, and
  `src/windows-update-manifest.ts`; `src/deploy-jobs.ts` does **not** own it.

### `src/coordinator-write-gate.ts` — the keeper-update fence

`CoordinatorWriteGate` is a REQUIRED dependency of every coordinator mutation path, never an option: it is
what stops a keeper update from racing a session-creating write, and that race loses live PTYs.
`acquire()` returns a `WriteLease` for an ordinary durable mutation and throws `Code.Unavailable` while the
fence is taken. `acquireExclusive(owner)` drains the admitted leases, then blocks new ones for as long as
`src/connect/handlers-workers-update.ts` needs to prove the keeper empty.
`acquireCompletion()` is admitted during the drain and refused only once the exclusive lease is held, because
a lifecycle projection finishing a command admitted before the drain cannot create a keeper channel by
itself, and refusing it would deadlock the drain against the RPC lease it is waiting on.

Three orderings depend on the fence; breaking any of them loses live PTYs:

- `src/connect/worker-frame-dispatch.ts` does NOT ACK a durable worker event while the exclusive lease is
  held. The worker's outbox replays that event after the keeper update instead of the coordinator losing it.
- `respawnMissingForWorker()` (`src/connect/worker-respawn.ts`) waits through `acquireAfterExclusive()`, so a
  worker reconnect cannot recreate a channel while the coordinator is proving the keeper empty.
- Terminal writes take the ordinary lease only AFTER entering the per-sender/session FIFO lane. Acquiring it
  before the lane deadlocks the exclusive drain.

## The split transports

- `src/connect/sync-ws-upgrade.ts` owns browser handshake validation,
  JWT/principal/dashboard scope, query/cursor parsing, and deadline selection.
  `src/connect/sync-ws-handler.ts` owns only the admitted live socket: feed,
  keepalive, delivery/scheduling composition, callbacks, and cleanup.
- Browser and worker sockets share `src/connect/ws-auth-deadline.ts`, which
  re-arms authentication expiry beyond the platform timer maximum.
  `src/connect/worker-ws-upgrade.ts` authenticates/binds the worker principal;
  `src/connect/worker-ws-handler.ts` owns live admission, reauth, and teardown.
  Ping/pong liveness deadlines remain separate in
  `src/connect/worker-conn-keepalive.ts`.
  `src/connect/worker-agent-status-frame.ts` preserves the optional observed
  identity triple when the admitted worker frame enters the status hub.
- Browser delivery leaves: `src/connect/sync-ws-client-ingress.ts` decodes
  client frames; `src/connect/sync-ws-v1-delivery.ts` owns ACK/backpressure
  windows; `src/connect/sync-ws-v2-scheduler.ts` is the stable facade over
  `src/connect/sync-ws-v2-egress.ts`, `src/connect/sync-ws-v2-queue.ts`,
  `src/connect/sync-ws-v2-control.ts`, `src/connect/sync-ws-v2-terminal.ts`,
  and `src/connect/sync-ws-v2-terminal-ready.ts` (the per-socket terminal
  ready ring); `src/connect/sync-ws-v2-state.ts` owns socket
  vocabulary/generations; `src/connect/sync-ws-v2-commands.ts` is the only
  browser-frame mutation path.
- **The v1 ACK-windowed path is supported back-compat, not dead code.**
  `apps/coord/tests/sync-ws-keepalive-flow-control.test.ts` asserts "legacy
  sockets remain unsequenced and unenforced". Do not delete
  `src/connect/sync-ws-v1-delivery.ts` without the deployed-client story.
- `src/connect/handlers-streaming.ts` keeps **only** the `sync` RPC stub, which
  throws `Code.Unimplemented` **by design** — the real transport is the
  WebSocket above, and `src/bun-coordinator-listeners.ts` additionally returns
  410 for that path at the fetch layer so Connect never opens the stream. The
  feed is
  `src/connect/sync-feed.ts` (bus subscription + durable event paging),
  `src/connect/sync-feed-frames.ts` (payload→`FirehoseFrame` adapters + lane
  metadata), `src/connect/sync-feed-v1-seed.ts` (bounded ACK-paced v1 seed), and
  `src/connect/sync-feed-seed.ts` (retained-snapshot seeding).
- Sync v2 advertises exactly seven application domains: terminal, workers, workspaces, tasks, MCP, pair, and audit.
  Audit alone is lazy; every other domain is subscribed at socket creation.
- `src/connect/session-control.ts` is a **re-export barrel** over
  `src/connect/terminal-control-lane.ts` (per-session ordering and generation cancellation)
  and `src/connect/input-control.ts` (including its bounded audit queue). Terminal view
  membership lives only in `src/connect/terminal-view-hub.ts`; terminal cell continuity
  lives only in `src/connect/terminal-screen-hub.ts`. The barrel owns no session lifecycle.

`src/connect/agent-prompt-control.ts` owns guarded prompt orchestration. It
validates the exact status fence, registers `waitForAgentStatus` before sending
one `DAgentPrompt`, aborts and consumes that waiter only for a definite
pre-write rejection, and awaits it for accepted or ambiguous input. It returns
input and optional wait outcomes separately; it never retries an ambiguous
write. Raw `SessionsInput` remains in `src/connect/input-control.ts` and keeps
its byte-for-byte semantics.

## Invariants

- **Post-commit publication order is load-bearing.**
  `src/event-transaction.ts::appendEvent` inserts the event and folds the
  `sessions` projection in one transaction. After commit,
  `src/pending-event-publications.ts::publishCommittedEvent` applies
  `applyDurableChannelIndex` before `sessionBus.publish`, then publishes any
  cascade workspace deltas. Publishing inside the transaction or bus-first can
  expose `opened`/`respawned`/`snapshot` before a worker/channel route exists.
  `src/event-log.ts` is only the stable facade.
- **Agent conversation references commit privately.** An authenticated
  `agent_reference` event is deduplicated and updates its session recovery JSON
  and worker `client_seq` in the same transaction, but never enters
  `sessionBus`. Every durable browser query and its maximum cutoff exclude the
  event, and the final Sync frame adapter refuses it. Snapshots omit both
  private columns, lower/equal sequences cannot replace or clear newer state,
  and session close deletes the row. Only the exact owning worker's restricted
  `SessionsList(worker_fp=self,status=open)` response contains one recovery row
  per returned session; browser/device callers receive none.
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
- **Prompt content is never coordinator telemetry or storage.** `SessionsPrompt`
  may log only non-content session, fence, and outcome metadata; prompt text and
  agent status messages are never logged, placed in `audit_log`, persisted, or
  echoed in its response. The request is capped at 16,384 UTF-8 bytes, and its
  optional wait is all-or-none with a `1..300000` ms timeout.
- **Acknowledged layout apply has one exact target generation.** `UiApplyLayout`
  requires a nonempty target fingerprint/tab tuple plus dashboard-owned document
  bindings. Sync upgrade rejects a tab ID over 256 UTF-8 bytes before it enters
  socket state. The live-target owner admits at most 32 distinct tuples per
  fingerprint globally and 256 per dashboard; either exhaustion closes the new
  socket with the same generic reason, while exact-tuple replacement does not
  consume another slot. The five-minute UI-state projection is discovery data,
  not target liveness; an earlier fingerprint remains pinned if another browser
  reports or opens the same tab ID. Pending state exists before publication to
  exactly that tuple's current authenticated read/write Sync-v2 socket. Only the
  same dashboard, fingerprint, tab, socket, and correlation may settle it as
  `applied` or `rejected`; wrong, stale, duplicate, and late results are ignored.
  Cancellation removes the pending entry; socket close/replacement or the
  deadline settles `target_gone`, with no retry. That outcome means an
  acknowledgement is unavailable, not that the browser did not execute.
- **UI ingress and report retention are bounded canonical state.** Reports,
  acknowledged documents, and all eight legacy commands are rebuilt from
  validated known fields, so retired or nested unknown protobuf fields cannot
  enter retention or the bus. UTF-8 text/document limits apply before
  publication, and legacy session/anchor/destination IDs are bounded before
  set construction or SQLite lookup. A global per-fingerprint cap/rate prevents
  one device from multiplying identities across dashboards; the independent
  per-dashboard cap bounds aggregate viewers. Capacity checks follow TTL reap
  and fail closed.
  Only successful new identities spend rate budget, while an existing tab's
  heartbeat remains admissible.
- **UI Sync frames are browser-only.** Read-only worker principals retain their
  dashboard Sync subscription for publication-count semantics but receive no
  live or seeded UI state and no UI command/apply frame.
- **`UiDispatch` is publication-only.** It refuses `applyLayout`; each of its
  eight commands returns exactly the selected dashboard's Sync subscriber
  count at `uiBus.publish`, regardless of targeting or execution. UI command
  frames are live-only and never enter a Sync seed.
- **Rate limiting matches exact mutation routes, never a path prefix.** `src/middleware/rate-limit.ts` keys on a `ReadonlySet` of
  full RPC paths, so `*List`/`*Read` calls cannot burn the mutation budget. Connect emits every unary RPC as POST,
  so the GET/HEAD/OPTIONS early return in `checkRateLimit` never fires for an RPC — the exact-name set is the guard.
- **`events` is append-only; public `sessions` is a projection of public
  events.** Never edit an event row. Public session state changes by appending
  an event and letting `foldEvent` (shared with the SPA through
  `@roost/shared/wire`) recompute the row, so browser and coordinator
  projections agree by construction. Private `agent_reference` uses its
  focused recovery projection above. Closed sessions are deleted, not parked;
  live `open` rows are never reaped on a wall-clock cutoff.
- **Agent-status revisions are scoped to one exact observed occupant.** A
  different non-retired `(status_epoch, occupant_id)` may start at a lower
  revision; displaced epochs/occupants are equality-fenced against late active
  and inactive frames. Identityless rollout frames never replace an identified
  status, and source is mutable provenance rather than an ordering token.
- **Agent-status waits are exact and event-driven.** A waiter is registered
  before the hub inspects current state, is fenced to one status epoch and
  occupant, and is removed on match, timeout, replacement, close, cancellation,
  or hub stop. The registry admits at most 32 waits per session and 2,048 total.

## Testing

- `bun test apps/coord/tests/` runs the recursive `**/*.test.ts` suites.
  `tests/coord-e2e.test.ts` boots a coordinator through `createCoord`
  against in-memory SQLite and drives `coord.fetch(...)` directly: no
  `Bun.serve`, port allocation, or network.
- The coordinator half of the terminal flow is pinned by
  `tests/terminal-view-hub.test.ts`, `tests/terminal-screen-hub.test.ts`,
  `tests/sync-ws-v2-scheduler.test.ts`, `tests/coord-bidi.test.ts`,
  `tests/durable-publication.test.ts`, `tests/announced-channel-barrier.test.ts`,
  `tests/sync-ws-keepalive.test.ts`,
  `tests/sync-ws-keepalive-flow-control.test.ts`,
  `tests/ws-auth-deadline.test.ts`, `tests/worker-ws-transport.test.ts`,
  `tests/worker-ws-transport-global-search.test.ts`,
  `tests/global-search-control.test.ts`, `tests/global-search-cursors.test.ts`,
  and `tests/worker-bidi-event.test.ts`. The browser end is
  `smoke/terminal/*.spec.ts` (`bun run test:terminal`).
- `bun run test:unit` runs the fast tier across all apps; `bun run lint` enforces the 400-line file cap and the
  `console.*` ratchet.
- Run it with `bun apps/coord/src/main.ts` (env parsed by `CoordConfig` in `apps/shared/src/config.ts`); install as
  a service with `bash apps/coord/scripts/install.sh install` (launchd on macOS, systemd --user on Linux).
