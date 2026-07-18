<!-- AUDIENCE: claude -->
# phase-24 — SHIPPED, then superseded by crpc5/crpc6

**Status:** transport collapse this plan describes shipped, then
superseded by crpc5 (worker CoordLink rewritten as Connect bidi) and
crpc6 (raw WSS hub deleted). Preserved for historical context.

---

# phase-24 — single transport per seam, coord↔worker bidir WS

Author 2026-06-12: "everything in TS, why do we still have these problems?
The browser↔worker seam is fucked, no clarity, even Claude can't grok the
connections." Approved + scope: Mac-only (drop Linux branches), Tailscale-only
(drop NAT/internet hardening).

Root cause (audit in this turn): the v2 rewrite collapsed the LANGUAGE
(all TS) but not the TOPOLOGY. Five process types, three transports
(`tRPC HTTPS+SSE` + browser↔worker raw WSS + worker↔keeper UDS), two auth
realms (`aud="roost-coordinator"` + `aud="worker-direct"`). TS catches frame
SHAPE but not connection LIFECYCLE, transport CHOICE, or projection
EQUIVALENCE. Every recurring L11 bug lives at a seam.

## end state

- ONE transport per seam:
  - browser ↔ coord = tRPC v11 (HTTPS + WS subscriptions, single link)
  - coord ↔ worker = ONE WSS, worker dials OUTBOUND to coord, bidir frames
- ONE auth realm per seam:
  - browser ed25519 → coord (`aud="roost-coordinator"`)
  - worker ed25519 → coord (`aud="roost-coordinator"`)
  - `aud="worker-direct"` DELETED. No browser-mints-then-dials-worker dance.
- worker has NO inbound port. `apps/worker/src/ws-server.ts` DELETED.
- PTY bytes ride coord↔worker WS upstream → coord fan-out → browser via
  tRPC `sessions.bytes` subscription. Binary frame format unchanged
  (`DIR_FROM_PTY` / `DIR_TO_PTY` + 2-byte BE channel_id + raw bytes).
- worker becomes STATELESS w.r.t. browser identity. Per-socket state
  (`keeperClients`, `channelViewers`, `viewerId`) moves to coord.
- event-sourced spine unchanged. ONE `foldEvent` from
  `apps/shared/src/wire/event.ts`, three callers (coord projector, SPA
  store, worker reconciliation), one property test asserting equivalence.
- ONE knob for topology: `ROOST_COORD_TAILNET_URL` (replaces
  `ROOST_REACHABLE_ADDR` + worker-direct ws_url composition).

## protocol — coord↔worker bidir WSS

Wire schema: `apps/shared/src/wire/coord-worker.ts` (NEW). Two
discriminated unions + a reused binary frame:

**Upstream (worker → coord), `CoordWorkerUpstream`:**
- `hello` — initial dial-in, carries `worker_fp`, `version`
- `pong` — keepalive
- `event` — wraps a `SessionEvent` (replaces today's `sessions.emit` mutation)
- `agent-patch` — `{ session_id, patch }` (replaces worker→browser direct `agent-patch`)
- `presence` — `{ channel_id, payload }` (snapshot/delta/leave, opaque payload)
- `rpc-ok` — `{ request_id, data }` (echo of coord-correlated browser command)
- `rpc-error` — `{ request_id, message }`

**Downstream (coord → worker), `CoordWorkerDownstream`:**
- `hello-ack` — `{ coord_pubkey_b64, coord_pubkey_kid }` (replaces today's
  heartbeat side-channel for coord identity)
- `ping` — keepalive
- `browser-command` — wraps a `ClientControlFrame` with
  `{ browser_id, viewer_id, request_id }` so worker can mint
  correlated rpc replies without knowing browser identity

**Binary frame (bidir, same byte layout as today's browser↔worker
binary):** 2-byte BE `channel_id` + 1-byte direction (0=from-pty,
1=to-pty) + raw bytes. Upstream carries only `DIR_FROM_PTY`; downstream
carries only `DIR_TO_PTY`. The direction byte stays for byte-format
equivalence — migration target reuses the same parser.

## browser↔coord additions

Browser stops opening worker-direct WS. New tRPC procedures on coord:
- `sessions.spawn` — `{ kind: "shell"|"claude", folder, initial_mode? }`
  returns `{ session_id, channel_id }`. Coord forwards as
  `browser-command` to the matching worker.
- `sessions.kill` — `{ session_id }`. Coord forwards.
- `sessions.input` — `{ session_id, bytes_b64 }`. Coord forwards as
  binary frame. Mutation per keystroke chunk (xterm batches writes;
  tRPC v11 batches mutations on the wsLink).
- `sessions.bytes` — subscription, yields `Uint8Array` chunks for a
  given `session_id`. Coord fan-out hub multiplexed.
- `sessions.attach` / `sessions.detach` — mutations. Coord tracks
  `(browser_id, session_id) → subscribed` and routes presence.
- `sessions.presence` — subscription, yields presence-snapshot/delta/leave.

`workers.connect` DELETED. `worker-direct` JWT realm DELETED.

## migration order (additive, smoke green after each)

**phase-24a — coord↔worker bidir WS, parallel to existing path:**
- 24a-1 (THIS COMMIT, "phase-24: the plan"): land design doc + wire
  schema + property test. ZERO consumers. Smoke unaffected.
- 24a-2: coord-side `WorkerHub` (accept worker dial-in at new path
  `/ws/coord-worker/:fp`). Authenticates worker JWT (existing
  `aud="roost-coordinator"`). Routes upstream `event` frames into
  existing `appendEvent` pipeline (parity with `sessions.emit`).
- 24a-3: worker-side `CoordLink` (outbound dial). Heartbeat + event
  emit STILL via tRPC (today's path) until 24a-4. CoordLink only
  carries a hello/pong loop initially.
- 24a-4: shift worker event emission from `sessions.emit` mutation to
  `CoordLink.send({ kind: "event", event })`. Delete `sessions.emit`
  caller from `session-manager.ts`. tRPC mutation kept for back-compat
  one commit, then removed.
- 24a-5: shift presence + agent-patch through CoordLink. Browser still
  receives via worker-direct WS.

**phase-24b — coord byte fan-out + browser tRPC subscriptions:**
- 24b-1: coord exposes `sessions.bytes` + `sessions.presence`
  subscriptions reading from fan-out hub. Browser subscribes in PARALLEL
  to worker-direct (both paths active; smoke asserts both deliver same
  bytes for one session).
- 24b-2: coord adds `sessions.spawn` / `kill` / `input` / `attach`
  / `detach` mutations. Browser starts calling these alongside
  worker-direct sends (idempotent / dedup at coord).
- 24b-3: browser switches `Terminal.tsx` byte source to tRPC sub.
  worker-direct WS still open for control frames.

**phase-24c — kill worker-direct path:**
- 24c-1: browser stops opening worker-direct WS entirely. All
  control via coord mutations, all bytes via coord subscription.
- 24c-2: delete `apps/web/src/ws/worker-direct.ts`.
- 24c-3: delete `apps/web/src/store/events-ws.ts`. All event streams
  via tRPC subscriptions per `apps/shared/src/router.ts`.

**phase-24d — kill worker-direct realm on coord + worker:**
- 24d-1: delete `apps/worker/src/ws-server.ts`. Worker has no inbound.
- 24d-2: delete coord `workers.connect` query + `aud="worker-direct"`
  mint code in `coord-key.ts`.
- 24d-3: delete `ROOST_REACHABLE_ADDR` from `WorkerConfig`; replace with
  `ROOST_COORD_TAILNET_URL` knob on worker side (one env var).

**phase-24e — invariants enforced by lint:**
- `new WebSocket(` allowed only in `apps/worker/src/transport/CoordLink.ts`
  and in browser tRPC client (`apps/web/src/trpc.ts`).
- `Bun.serve({ ..., websocket })` allowed only in `apps/coord/src/main.ts`.
- Every `switch (frame.kind)` on `CoordWorkerUpstream` /
  `CoordWorkerDownstream` / `ClientControlFrame` / `ServerControlFrame`
  / `SessionEvent` ends with `assertNever(frame)`.
- No top-level `let _ws` / `let _reconnectTimer` patterns in
  `apps/web/src/store/`.
- `scripts/lint-roost.ts` enforces all of the above.

**phase-24f — equivalence + frame-chain tests:**
- `apps/shared/tests/foldEvent.equivalence.test.ts` — fast-check
  property: `foldAll(events)` matches across shared-module / coord
  projection / SPA projection on random `SessionEvent[]` (length 0..50).
- `/roost-smoke` skill — strengthen to assert the frame chain at the
  protocol level (`browser→coord mutation: sessions.spawn` →
  `coord→worker downstream: browser-command(spawn-shell)` →
  `worker→coord upstream: event(opened)` → `worker→coord upstream:
  binary(DIR_FROM_PTY)` → `coord→browser subscription: sessions.bytes`),
  not just the visible terminal text.

**phase-24g — delete `apps_legacy/`** after 24f stable for 1 week.

## what STAYS untouched

- keeper subprocess per session (Node, node-pty pinned). Internal hop,
  never a wire seam. `feedback_worker_deploy_macos_repairs.md`
  repairs unchanged.
- Claude bridge subprocess per session. Internal hop.
- ed25519 per side, EdDSA JWTs, `kid` = SHA-256 of pubkey.
- SQLite + Kysely + bun:sqlite on coord. Event log + projection
  transaction (R0.3) unchanged.
- `@roost/shared` as wire source of truth.
- LaunchAgents + macOS deploy story.

## blockers from audit (this turn)

10 items in the explorer audit are load-bearing; the migration order
above addresses each. The four most critical:

1. **Per-socket state on worker** (`ws-server.ts:30,43`) → moves to coord
   in 24b. Worker becomes stateless w.r.t. browsers.
2. **`workers.connect.sub` claim** mints with coord caller's fingerprint,
   not browser's — bug today, deleted entirely in 24d-2.
3. **Coord pubkey delivery to browser** for worker-direct verification
   — obsolete in 24c (no worker-direct path).
4. **Inbound `attach`/`detach`/`kill` frames** moved to coord mutations
   in 24b-2.

## commit budget

phase-24a (4 sub-commits) + 24b (3) + 24c (3) + 24d (3) + 24e (1) +
24f (2) + 24g (1) = 17 commits. Each additive, smoke green at each,
no big-bang. Estimated wall clock: multi-session grind.
