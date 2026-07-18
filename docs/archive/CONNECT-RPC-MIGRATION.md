# Connect-RPC migration — SHIPPED 2026-06-15

**Status:** SHIPPED end-to-end. crpc1-crpc6 merged on `v2`. Raw WSS hub
deleted (0790d4da); tRPC + H3 + raw scrollback/input lanes retired
(51ba5989). Wire is Connect + protobuf binary everywhere. See
`FEATURES/README.md` for the shipped inventory; this file preserves the
original plan as historical context.

---

# Connect-RPC migration — planned (historical)

**Status (original):** planned, not started. Trigger: scale past ~5 carve-out endpoints, or
multi-Mac multi-session ops cost exceeds the hybrid-wire maintenance tax.

**Target scale that motivates this:** 3-4 worker Macs × 20-30 sessions each
= 60-120 active terminals. At that scale the per-endpoint carve-outs (raw
WS for input, raw HTTP for snapshot, tRPC SSE for firehose, tRPC JSON for
control) compound into operational complexity.

## Why migrate

Today's hybrid wire:

| Lane | Wire | Use |
|---|---|---|
| `/api/trpc/*` HTTP+JSON | tRPC mutation | control: sessions, presence, workspaces, tasks, webhooks, permissions, mcp, auth, pair |
| `/api/trpc/sync.firehose` SSE | tRPC subscription | live PTY bytes + deltas (JSON `{"0":x,"1":x,...}` array bloat) |
| `/ws/browser-input` raw WS | hand-rolled JSON-frame wire | keystroke + mouse-tracking input (pb8) |
| `/api/scrollback/:sid` raw HTTP | hand-rolled binary endpoint | scrollback snapshot (pb16) |
| `/ws/coord-worker/:fp` raw WS | hand-rolled JSON + binary hub | coord ↔ worker dispatch |

Five lanes, four auth shims (tRPC middleware + WS upgrade JWT + HTTP
Authorization header + worker mintJwt), four error semantics, four
observability hooks. Each new feature requires a "which lane?" decision.

Connect-RPC collapses this to one framework, four call shapes:

| Call shape | Replaces today's |
|---|---|
| unary | tRPC mutations/queries |
| server-streaming | tRPC SSE subscriptions |
| client-streaming | `/ws/browser-input` (mostly write-only from browser) |
| bidi-streaming | hub WS (worker ↔ coord both directions) |

All over HTTP/2, all protobuf wire (binary native, no base64 anywhere),
all typed end-to-end via protobuf-generated TypeScript, all interceptor
auth (one Authorization check applies everywhere).

## Concrete wins

| Today | After Connect | Win |
|---|---|---|
| 4 auth paths, easy to skip one on a new endpoint | 1 interceptor, applies to all calls | correctness |
| Live bytes through tRPC SSE `{"0":x,"1":x,...}` (5× bloat) | protobuf binary message | ~80% wire reduction on live byte stream |
| Snapshot single-shot HTTP response | server-streaming chunks | first byte paints in ~50 ms (vs ~500 ms today) |
| Hand-typed headers (X-Start-Seq etc) in pb16 | protobuf message fields | type safety |
| Hub WS frame format hand-written | protobuf bidi-stream | less wire code to maintain |
| 5 carve-outs to maintain | 1 framework | mental + onboarding cost |

## What it would NOT fix

- **Browser memory ceiling at 60+ terminals** — still need lazy mount /
  LRU eviction. Connect doesn't reduce per-wterm memory.
- **Worker single-threaded base64** — irrelevant after Connect (no base64).
- **wterm.write parse cost on the SPA** — same regardless of wire.

## Migration plan — proposed phases

### crpc1 — proof of concept (1 day)

Migrate ONE endpoint to Connect: `sessions.list`. Verify the toolchain
end-to-end with Bun + Connect-Node + Connect-Web.

- Install `@connectrpc/connect-node`, `@connectrpc/connect-web`,
  `@bufbuild/protoc-gen-es`
- Add `.proto` schemas under `apps/shared/proto/`
- Generate TS clients/servers via `buf generate`
- Mount Connect handler in coord alongside existing tRPC routes
- Swap one SPA call site

Decision gate: does the build pipeline work cleanly? Are generated types
ergonomic? Does Bun runtime cooperate? If yes → proceed. If no → revisit.

### crpc2 — control plane (2-3 days)

Migrate every tRPC mutation/query to Connect unary:

- `workers.*`, `sessions.*` (list, spawn, kill, assignWorkspace, input,
  resize, etc.), `workspaces.*`, `tasks.*`, `webhooks.*`, `permissions.*`,
  `mcp.*`, `auth.*`, `pair.*`, `misc.*`
- Keep tRPC running in parallel during migration; cut over per-endpoint
- Delete tRPC mounts once all endpoints migrate

Auth: a single Connect interceptor checks `Authorization: Bearer <jwt>`
via the existing `verifyJwt` helper. Applies to all unary calls. No more
per-endpoint shims.

### crpc3 — firehose → server-streaming (1-2 days)

Replace `sync.firehose` tRPC SSE with a Connect server-streaming RPC.
Coord's `BoundedBus<T>` already exposes an `AsyncIterable<T>` shape — wire
it straight into the Connect handler. SPA reads the stream, dispatches
events to existing handlers.

Byte fan-out: live PTY bytes ride the same server-stream as discriminated
binary messages. No more `{"0":x,"1":x,...}` array encoding.

### crpc4 — input + scrollback (1 day)

- `/ws/browser-input` → Connect client-streaming `sessions.inputStream` —
  SPA opens once, pushes input frames forever.
- `/api/scrollback/:sid` → Connect server-streaming `sessions.scrollback` —
  worker pushes scrollback in chunks. SPA writes each chunk on arrival.
  Progressive paint, ~50 ms time-to-first-byte instead of ~500 ms blob.

Delete the hand-rolled wire files: `browser-input-ws.ts`,
`scrollback-http.ts`.

### crpc5 — coord ↔ worker hub (1-2 days)

The hub WS (`/ws/coord-worker/:fp`) becomes a Connect bidi-stream
worker-initiated by the worker. Worker dials connect, sends `hello`,
both sides exchange typed messages forever. Replaces the hand-written
binary frame format in `apps/shared/src/wire/control.ts`.

### crpc6 — cleanup (0.5 day)

- Delete `@trpc/*` dependencies
- Delete `apps/shared/src/router.ts`, `apps/shared/src/wire/control.ts`
- Delete `apps/coord/src/router/scrollback-http.ts`,
  `apps/coord/src/router/browser-input-ws.ts`,
  `apps/coord/src/router/coord-worker-hub.ts` (Connect handlers replace)
- Update `@roost/shared` to re-export from generated protobuf types
- Update CLAUDE.md sections about transport

**Total estimate:** 7-10 working days for one engineer.

## Risk register

| Risk | Mitigation |
|---|---|
| Bun + Connect-Node runtime compatibility | crpc1 PoC verifies before committing further; revert is trivial at that stage |
| Protobuf schema mistakes that change wire shape mid-flight | each endpoint migrates atomically (old tRPC stays until new Connect call replaces it client-side); cut over per-endpoint |
| Generated code in git noise | configure `buf generate` output into `.gen.ts` files with consistent path; gitignore is per-team preference |
| Auth migration error (a Connect endpoint skipping JWT check) | interceptor is global — one place to enforce; harder to skip than per-endpoint middleware |
| Performance regression in some endpoint | benchmark before/after on each phase; rollback per-phase if observed |

## Out of scope (deferred to later phases)

- Streaming uploads for file transfers
- Voice audio streaming
- Multi-region coord federation
- gRPC reflection / Connect introspection tooling

## Escape hatch

If the migration hits a wall (Bun incompatibility, protobuf tooling
mismatch, etc.), each phase can be reverted independently because the old
tRPC routes are kept alive during migration. Worst case: stop after crpc1
PoC, no production code affected.

## Decision triggers — when to start

Start crpc1 when ANY of these are true:
- Hybrid wire has ≥5 carve-out endpoints (currently 2: input WS, scrollback HTTP)
- A second active maintainer joins and needs to ramp
- Multi-tenant deployment is on the roadmap
- A new feature requires bidi-streaming or large file streaming
- Operational pain on the hybrid wire becomes a regular conversation

Until those triggers, keep using incremental carve-outs.
