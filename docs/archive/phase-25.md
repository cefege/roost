<!-- AUDIENCE: claude -->
# phase-25 — SHIPPED

**Status:** SHIPPED on `v2` (scrollback replay, presence emit,
worker-restart, teeSink retire, schema cleanup). Preserved for
historical context.

---

# phase-25 — finish what phase-24 left on the table

Phase-24 collapsed the transport (one bidir WSS per seam, worker
outbound-only, tRPC subscriptions, lint guards). It deferred three
classes of work: (a) features that need worker-side hooks now that the
inbound surface is gone, (b) perf bandaids that were acceptable for a
green smoke but expensive at scale, (c) safety nets to retire once
production stability is proven. This plan covers (a) and (c). Perf
upgrades (b) live in phase-26.

Each commit is additive, smoke green at each, no big-bang per
`feedback_no_complete_redesigns.md` and CLAUDE.md §13.

## phase-25a — scrollback replay on attach

Today `sessions.attach` returns `replay_offset: 0` hardcoded
(`apps/coord/src/router/sessions.ts:130`). UI symptom: refresh a
terminal pane, blank grid until live PTY output resumes. Keeper has the
ring buffer; coord-routed attach never asks for it.

- **25a-1** wire schema: extend `CoordWorkerDownstream.browser-command`
  attach frame with the optional `from_offset` field (it already has it
  on the `ClientControlFrame.attach` variant — confirm passthrough).
- **25a-2** keeper protocol: add a `ReplayFromOffset` frame type +
  encoder/decoder in `apps/worker/src/keeper/protocol.ts`. Keeper
  side reads from ring buffer.
- **25a-3** worker `SessionManager.attach(channelId, fromOffset)`
  routes via the persistent `outputClient` — sends `ReplayFromOffset`,
  collects bytes until "replay-end" marker, replies rpc-ok with the
  bytes + a real `replay_offset` value.
- **25a-4** coord `sessions.attach` mutation forwards `from_offset`
  downstream + returns the real reply data.
- **25a-5** `Terminal.tsx` passes the `from_offset` from local state
  (last seen byte count) AND writes the returned replay bytes to wterm
  before subscribing to live `sessions.bytes`.
- **25a-6** smoke addition: open pane, type "echo hello", close+reopen
  same workspace URL, assert the wterm grid contains "hello" without a
  fresh shell prompt.

## phase-25b — worker-side presence emit

`presence-hub.ts` + `sessions.presence` subscription shipped in
phase-24d-3 cleanup. Worker no longer broadcasts
`presence-snapshot/delta/leave`. Multi-viewer ghost cursors are dark.

- **25b-1** `SessionManager` gains a viewer registry: `Map<sessionId,
  Map<viewerId, { col, row, label }>>`. On `cursorPos` browser-command
  (forwarded by coord), update the entry + emit
  `CoordWorkerUpstream.presence` frame with payload =
  `presence-delta`. On viewer subscription (the `sessions.presence`
  tRPC sub coord-side): coord sends a synthetic
  `browser-command:attach-presence-viewer` so worker knows about a new
  viewer; worker replies with a `presence-snapshot`. On unsubscribe
  (subscription close) → coord forwards a leave frame; worker emits
  `presence-leave`.
- **25b-2** `Terminal.tsx` re-introduces a presence subscription
  alongside the bytes sub. `GhostCursorOverlay` populated from the
  delta stream.
- **25b-3** smoke addition: two browser tabs on the same session, type
  in one, assert the other shows a ghost cursor at the typed column.

## phase-25c — worker-restart session resume

Sessions in DB (`status="open"`) don't get a reconstructed
`outputClient` after worker boot. Keeper UDS sockets exist
(`/Users/you/Library/Application Support/RoostWorkerV2/sessions/<ch>.sock`)
but byte fanout doesn't reconnect.

- **25c-1** worker boot: query coord
  `sessions.list({ worker_fp: self, status: "open" })` AFTER coordLink
  open. For each row, attempt `attachOutputClient(channelId,
  inferredSocketPath)`. Sockets matched by channel number; if the
  socket file doesn't exist, emit `closed` for that session (ghost
  cleanup).
- **25c-2** persist the keeper PID alongside the socket path on
  initial spawn so resume can verify the keeper is still alive (kill
  -0 PID).
- **25c-3** smoke addition: spawn a pane, type "echo hello" so it's in
  scrollback, restart worker (`launchctl kickstart -k`), reload
  browser, attach with scrollback replay (25a) — assert "hello" still
  visible.

## phase-25d — retire teeSink (after a stable observation window)

`apps/worker/src/event-sink.ts` `teeSink(coordLinkSink, trpcSink)`
emits every event on BOTH wires during the migration. Now that the
CoordLink path is live-verified, cut to single-wire emit.

- **25d-1** flip `sink = coordLinkSink(coordLink)` in
  `apps/worker/src/main.ts`. Delete `trpcSink` from `event-sink.ts`
  and the `client.sessions.emit.mutate` caller. Smoke must remain
  green for 24h before merge.
- **25d-2** delete coord `sessions.emit` mutation + its skeleton
  declaration in `apps/shared/src/router.ts`. Worker `coord-client.ts`
  loses the dependency on the tRPC type's `sessions.emit` shape.
- **25d-3** delete coord `/api/events` legacy upgrade path
  (`apps/coord/src/router/events.ts` + `handleEventsUpgrade` +
  `eventsWebSocket` in main.ts dispatcher). Older SPA bundles cached
  in tabs are no longer supported.

## phase-25e — Worker schema cleanup

`reachable_addr / ssh_port / ws_listen_port / ws_scheme` are
`.optional()` on `apps/shared/src/wire/worker.ts` and present in DB
schema. Drop entirely.

- **25e-1** new migration `apps/coord/migrations/0003_drop_worker_inbound_fields.sql`
  → `ALTER TABLE workers DROP COLUMN reachable_addr; DROP COLUMN
  ssh_port; DROP COLUMN ws_listen_port; DROP COLUMN ws_scheme;`. SQLite
  has limited DROP COLUMN support pre-3.35; if needed, recreate
  workers table without those columns + copy data.
- **25e-2** delete the fields from `apps/coord/src/db/schema.ts`
  `WorkersTable`, `apps/shared/src/wire/worker.ts` `Worker` schema,
  `apps/shared/src/config.ts` `WorkerConfig`, and
  `apps/worker/src/install.ts` register/redeem payloads.
- **25e-3** coord `workers.register` + `auth.redeemWorker` shed the
  optional inputs.

## phase-25f — Files RPC timeout

`apps/coord/src/router/files.ts::routeViaHub` inherits
`createPendingRpc`'s 30s default. Original `workerWssRpc` was 10s,
tuned for cwd-picker UX (user gives up).

- **25f-1** `createPendingRpc<T>(timeoutMs?)` already supports
  override. `files.read` + `files.listDir` pass `timeoutMs: 10_000`.
- **25f-2** add a `sessions.spawn` timeout of 15_000 (matches old
  spawnSession.ts `SPAWN_TIMEOUT_MS`).

## housekeeping (not gated on the above)

- **stale TS errors** in `apps/coord/src/event-log.ts:217` (WorkspaceId
  brand), `apps/web/src/App.tsx:79` (RouteSectionProps), `apps/web/src/
  store/projector.ts:74` (SessionId brand). Pre-existing. Touch when
  editing those files.
- **stale Playwright e2e** under `apps/web/e2e/` — deprecated per
  `feedback_playwright_only_no_humanchrome.md`. Delete the directory.
- **legacy LaunchAgents** `com.roost.coordinator` (PID 99746) +
  `com.roost.worker` (PID 5856) — pre-v2 cruft. `launchctl unload
  ~/Library/LaunchAgents/com.roost.{coordinator,worker}.plist` once
  confirmed unused, then delete the plist files.

## ordering

25a → 25b → 25c are independent features; ship in any order.
25d (retire teeSink) blocks on 24h of stable phase-25a smoke.
25e (schema cleanup) blocks on 25d (older binaries rolling back can't
  populate the dropped columns; teeSink path was the back-compat shim).
25f (timeouts) and housekeeping are independent; ship anytime.

## not in scope (deferred to phase-26)

- persistent `KeeperClient` pool replacing `sendInputOnce` per
  keystroke
- `cursorPos` batching window
- multiplexed `wsLink` (one socket for all 6 subs vs reconnect storm
  on JWT refresh every ~4.5 min)

## commit budget

25a × 6 + 25b × 3 + 25c × 3 + 25d × 3 + 25e × 3 + 25f × 2 =
20 commits. Each additive, smoke green, no big-bang. Estimated wall
clock: 2 sessions.
