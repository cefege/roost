<!-- AUDIENCE: claude -->
<!-- Coordinator map: domain folders own handlers; root composition owns process/listener wiring. -->
<!-- Protocol meaning is authoritative under protocol/spec; this README records coordinator ownership. -->

# apps/coord — Bun coordinator

The coordinator is the SQLite writer and control-plane authority. `main.ts` composes configuration, database, auth, RPC, worker WS, Sync WS, and maintenance owners. `bun-coordinator-listeners.ts` owns the single `Bun.serve` boundary. `coord-factory.ts::createCoord()` owns the portable request/response composition and injected runtime seams. The coordinator is not a direct terminal or attachment data peer; it authorizes and signals the carriers described by the protocol specs.

Protocol contract index: [`protocol/README.md`](../../protocol/README.md). Surface-specific authority: [`protocol/spec/coordinator-rpc.md`](../../protocol/spec/coordinator-rpc.md), [`protocol/spec/sync.md`](../../protocol/spec/sync.md), [`protocol/spec/worker-link.md`](../../protocol/spec/worker-link.md), [`protocol/spec/direct-terminal.md`](../../protocol/spec/direct-terminal.md), [`protocol/spec/attachments.md`](../../protocol/spec/attachments.md), [`protocol/spec/auth-and-pairing.md`](../../protocol/spec/auth-and-pairing.md), [`protocol/spec/agent-metadata.md`](../../protocol/spec/agent-metadata.md), [`protocol/spec/session-events.md`](../../protocol/spec/session-events.md), and [`protocol/spec/terminal-stream.md`](../../protocol/spec/terminal-stream.md).

## Entry point

`apps/coord/src/main.ts` loads `@roost/host/config`, opens and migrates SQLite, imports authorized keys, starts maintenance owners, constructs the write gate and auth/transport dependencies, calls `createCoord()`, and starts the Bun listener. `apps/coord/src/bun-coordinator-listeners.ts` owns the actual bind, request-admission gate, caller origin, db export route, WebSocket upgrades, multiplexed dispatch, frame cap, and SPA fallback. `apps/coord/src/coord-factory.ts` owns portable HTTP composition, including Connect dispatch, `/api/*` routing, SPA handoff, CORS/security wrapping, and injected `ctx.spa`/`ctx.dbExport` seams.

The default bind is `127.0.0.1:4103`. Public TLS, DNS, and front-door policy remain operator-owned. `/api/db-export` is on-host only. The listener never substitutes coordinator-owned data bytes for a direct worker carrier.

## Module map

One row per current owned source, migration, or test directory.

| Directory | Owns | Must not own |
| --- | --- | --- |
| `apps/coord/src/` | Process composition, listener boundary, portable coord factory, write gate, startup janitor, build identity, gzip helper, and root-level event/authority adapters. | Domain handler implementation or worker terminal cell execution. |
| `apps/coord/src/agents/` | Agent status, status ordering/waits, prompt admission, conversation-reference recovery, and agent RPC handlers. | Worker process observation or browser presentation. |
| `apps/coord/src/attachments/` | Attachment RPC relay, short-lived grants, direct attachment signaling, status correlation, and worker send/dispatch frames. | Attachment bytes on a direct carrier or terminal grant authority. |
| `apps/coord/src/auth/` | JWT, authorized keys, bootstrap, pairing, devices, tenant/cf-access, auth interceptor, and auth deadline. | Web UI ceremony state or worker-side JWT key material. |
| `apps/coord/src/db/` | SQLite connection/schema, migration runner and validation, backup, audit retention, and export snapshot. | Worker durable event stores or protocol schemas. |
| `apps/coord/src/deploy/` | Deploy job registry, worker update/deploy handlers, catch-up convergence, and paused Windows coordinator update bookkeeping. | CLI operator UX or keeper protocol implementation. |
| `apps/coord/src/diagnostics/` | Worker/session diagnostic snapshots, snapshot fanout, and telemetry. | Terminal content capture storage owned by `terminal/capture/`. |
| `apps/coord/src/events/` | Durable event transaction/query/projection, buses, announced-channel barrier, and public visibility. | Session handlers or browser state projection. |
| `apps/coord/src/middleware/` | Security headers/audit, caller origin, coordinator request admission, and rate limiting. | Domain authorization decisions or listener construction. |
| `apps/coord/src/push/` | VAPID identity, push subscriptions, dispatch, and sender. | Browser push adapter or generic event bus policy. |
| `apps/coord/src/rpc/` | Connect handler facade, one `router.service(CoordinatorService, ...)` composition, Bun handler, system/transcription/streaming handlers, and router helpers. | A second service implementation or per-domain state. |
| `apps/coord/src/router/` | Browser-to-worker pending RPC correlation table. | Domain handler logic or worker connection state. |
| `apps/coord/src/search/` | Global terminal search cursors, options, fanout, cancellation, worker lanes, and handler. | Worker-local search implementation or browser search UI. |
| `apps/coord/src/sessions/` | Session/workspace/task/MCP handlers, spawn, pending spawns, and public session list projection. | Keeper/PTY process execution. |
| `apps/coord/src/sync/` | Sync WS upgrade/handler, v1 delivery, v2 scheduler/queue/control/terminal state, feed, retention, presence, and SSE support. | Connect-RPC handlers or worker link transport. |
| `apps/coord/src/terminal/` | Terminal metadata adapter/title hub and domain handoff between terminal subfolders. | A second wire schema or worker-side terminal core. |
| `apps/coord/src/terminal/capture/` | Authenticated terminal incident capture bridge, lease, recorder, and worker-call boundary. | Normal terminal stream delivery or content in general diagnostics. |
| `apps/coord/src/terminal/direct/` | Direct terminal grant owners, local-terminal grants, peer negotiation, direct result dispatch, and worker send frames. | Direct carrier byte transport or browser route election. |
| `apps/coord/src/terminal/input/` | Raw input/control lanes, Sync terminal controls, route results, retirements, and exact worker sends. | Browser input composition or worker PTY writes. |
| `apps/coord/src/terminal/screen/` | Canonical screen hub, resumable cursors, byte hub, stream dispatcher, cache, and scrollback handler. | Worker cell generation or direct peer packet framing. |
| `apps/coord/src/terminal/view/` | View hub, worker view relay/projection, stream controllers, and worker view sends. | Browser view geometry or worker session ownership. |
| `apps/coord/src/ui-state/` | UI reports, legacy command admission, bounded state retention, and acknowledged layout apply owner. | Browser-local pane topology or layout document parsing. |
| `apps/coord/src/workers/` | Worker registry/connection, worker WS upgrade/handler, heartbeat, frame queue/dispatch, respawn, and worker handlers. | Browser clients, keeper subprocess, or worker session implementation. |
| `apps/coord/migrations/` | Ordered SQL schema migrations and migration fixtures used by the coord database. | Runtime request handling or generated protobufs. |
| `apps/coord/tests/` | Coordinator unit, transport, domain, migration, and integration suites. | Web component or worker process tests. |
| `apps/coord/tests/agents/` | Agent status, prompt, wait, and private-reference tests. | Worker agent-status process tests. |
| `apps/coord/tests/attachments/` | Grant, direct signaling, status, and relay tests. | Browser attachment UI tests. |
| `apps/coord/tests/auth/` | JWT, pairing, device, tenant, and auth-principal tests. | Client ceremony tests. |
| `apps/coord/tests/db/` | Backup and audit-retention tests. | Migration fixtures outside the coord test root. |
| `apps/coord/tests/deploy/` | Coordinator deploy and Windows update dispatch tests. | CLI process tests. |
| `apps/coord/tests/diagnostics/` | Diagnostic snapshot and fanout tests. | Terminal content capture assembly tests. |
| `apps/coord/tests/events/` | Event log, publication barrier, and recovery-order tests. | Session handler tests outside the domain. |
| `apps/coord/tests/push/` | Push security, delivery, and sender tests. | Browser push adapter tests. |
| `apps/coord/tests/rpc/` | Bun Connect handler tests. | Domain-specific tests in their own folders. |
| `apps/coord/tests/search/` | Global search cursor, control, cancellation, and fixture tests. | Worker search tests. |
| `apps/coord/tests/sessions/` | Pending spawn tests. | PTY lifecycle tests. |
| `apps/coord/tests/sync/` | Sync upgrade, v1/v2 scheduler, backfill, flow, and terminal-control tests. | Web Sync state tests. |
| `apps/coord/tests/terminal/` | Terminal title/metadata and hop-deadline tests. | Direct, screen, input, and view tests below. |
| `apps/coord/tests/terminal/capture/` | Capture bridge, recorder, evidence, and fixture tests. | Ordinary terminal stream tests. |
| `apps/coord/tests/terminal/direct/` | Grant and peer negotiation tests. | Terminal screen or input tests. |
| `apps/coord/tests/terminal/input/` | Route result correlation tests. | Browser input router tests. |
| `apps/coord/tests/terminal/screen/` | Screen hub, stream dispatcher, cache, budget, and cursor tests. | Worker cell generation tests. |
| `apps/coord/tests/terminal/view/` | View hub, registry, owner, projection, and stream controller tests. | Browser view component tests. |
| `apps/coord/tests/ui-state/` | UI report, legacy command, and acknowledged apply tests. | Browser layout store tests. |
| `apps/coord/tests/workers/` | Worker WS, registry, heartbeat, respawn, tombstone, and worker transport tests. | CLI deployment tests. |

## Invariants

- `src/rpc/router.ts` installs one `router.service(CoordinatorService, ...)` literal and spreads each `make<Domain>Handlers(deps)`. A second service call shadows the first with unimplemented methods; never add one.
- `src/coordinator-write-gate.ts` is required for every coordinator mutation. Ordinary writes take a lease; keeper update takes an exclusive drain. Worker durable-event ACK, worker respawn, and terminal input ordering must preserve the documented fence ordering.
- Connect, Sync WS, and worker-link WS have separate admission and cleanup owners. Connect is request/response; Sync and worker link are raw WebSocket surfaces.
- The coordinator owns authorization, grants, correlation, durable state, and signaling. Direct carriers own their bytes only after the grant and worker identity fences pass.
- `src/terminal/screen/` is the canonical coordinator cell/screen projection and resumable cursor owner. Do not make a second projection in a handler or carrier.
- Durable event publication and visibility are fail-closed. Public projection code must not leak worker-only conversation-reference recovery data.
- Auth and pairing state are coordinator-owned. Plaintext requester tokens/codes and private conversation references do not enter Sync or public event projections.
- `@roost/protocol` is the only source for wire shapes. This map deliberately does not restate endpoint state machines; the linked `protocol/spec` files are authoritative.
