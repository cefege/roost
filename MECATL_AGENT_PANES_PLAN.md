# Mecatl-backed agent panes (optional, alongside the PTY plane)

## Context

Literal ask: use Mecatl (`github.com/stacklok/mecatl`, Apache-2.0) as the agent
runtime on every Roost machine, so a Roost pane can be **either** today's PTY
terminal **or** a Mecatl-backed agent session — without re-implementing
anything Mecatl already owns, and without touching or gating the existing
terminal plane.

End state: a worker machine optionally supervises a loopback-bound `mecated`.
A browser can open an "agent" pane in a workspace folder; the pane shows the
conversation, tool cards, and permission approvals, and can prompt, approve,
steer, and cancel. Roost carries Mecatl's own event JSON opaquely over the two
sockets it already has (worker link, browser Sync) and never interprets,
folds, or persists agent conversation content.

## What Mecatl already owns — do NOT rebuild any of this

Verified from Mecatl's docs and source this session:

- Durable sessions, transcripts, snapshots, fork/clear/retry/compaction,
  retention (`/docs/features/session-continuity`).
- Ordered, at-least-once durable activity replay with a serializable cursor
  (`session.activity({from: cursor})`, `session.attach(runId)`), plus typed
  `CursorExpiredError` / `ActivityGapError` (`/docs/building/typescript-sdk/durable-activity`).
- The permission/approval model (`permission.ask` → `allow_once` /
  `allow_always` / `deny`), steering, cancellation, token budgets, compaction
  (`/docs/building/what-you-get/agent-loop`).
- Single-writer leases, provider/model selection, MCP tools, subagents/teams.
- A Bun-compatible TypeScript SDK: `@stacklok-oss/mecatl-sdk` (transport-neutral
  + browser HTTP/SSE) and `@stacklok-oss/mecatl-sdk/node` (Node/Bun gRPC,
  local-daemon ownership).

Consequences for this plan, and the reason it is small:

- **No new durable `SessionEvent` variant** and **no new coordinator table** for
  agent conversations. Roost persists no transcript, no tool result, no cursor.
- **No Roost-side approval, retry, or compaction model.** The browser renders
  Mecatl's ask and sends back Mecatl's verdict.
- **No Roost-side replay/backfill for agent content.** Reconnect resumes from a
  browser-held Mecatl cursor.
- Mecatl's own guidance is to put a same-origin backend-for-frontend in front
  of `mecated` so browser JavaScript never holds the server bearer token
  (`/docs/building/deployment/grpc-http`). Roost's coordinator already is that
  BFF, with device auth and single-install tenancy.

Roost contributes exactly four things Mecatl does not have: fleet identity and
device auth, machine routing (workers are outbound-only, so the browser cannot
reach a worker's `mecated` directly), workspace/folder placement, and the
multi-device browser UI.

## How it works — setup, user flow, and every hop

### What an operator does, once per machine

1. Install the runtime: `brew install stacklok/tap/mecatl` (or a release
   archive). Roost does not ship, download, or pin it.
2. Give that machine a model provider the normal Mecatl way —
   `~/.config/mecatl/auth.yaml` or a provider env var. Roost never reads or
   stores provider credentials.
3. Set `ROOST_MECATL=1` (optionally `ROOST_MECATL_ROOT=/path`) in the worker
   service environment and restart the worker.

Nothing else is per-machine. No port to open, no DNS, no TLS, no OIDC: the
daemon stays bound to loopback and is reached only through the worker that
already dials the coordinator.

### What the worker does at boot

Spawns exactly one `mecated serve` bound to `127.0.0.1:<worker-allocated
port>`, with its own random bearer in the child env, its store under the
worker data dir, and the worker's stdin pipe as Mecatl's parent-liveness
channel. It proves readiness with `GET /v1/compatibility` before reporting the
machine as agent-capable. Crash → bounded restart backoff; sessions survive in
`--store-dir`, which is why killing the daemon is safe and killing the keeper
is not.

### What the user does in the browser

The pane is a second way to work, chosen per task; terminals are untouched and
stay mounted behind it.

1. **Open** the activity-bar entry → `/agent`. A machine list appears, the same
   worker set the terminal sidebar shows, each with a `StatusDot`: `ready`,
   `disabled`, or `unavailable: <reason>`.
2. **Pick a machine.** The pane calls `sessions.list()` on that machine's
   daemon and shows its conversations — these are Mecatl's own durable
   sessions, so they are the same list `mecatui` would show if the operator
   sat at that machine, and they survive browser reloads, other devices, and
   worker restarts.
3. **Open or create.** `sessions.get(id)` restores history through
   `session.transcript()`; `sessions.create({})` starts a new one at the
   daemon's configured root.
4. **Prompt.** Typing and sending calls `session.run(text)`; the pane iterates
   the returned run: `message.delta` streams assistant text, `tool.call` /
   `tool.result` render tool cards, `result` ends the turn and re-enables the
   composer.
5. **Approve.** A `permission.ask` event renders an approval card — Allow once
   / Allow always / Deny — answered with `run.resolveAsk(askId, verdict)`.
   This is Mecatl's permission model surfaced verbatim; Roost adds no policy.
6. **Steer or cancel.** `run.cancel()` always. Mid-run steering rides
   `POST /v1/sessions/{id}/steer` (and `cancel-steer`), which the pane offers
   only when the session capability snapshot reports `steer` AND
   `/v1/compatibility` lists the `http_steer` feature; otherwise the composer
   queues the text and sends it as the next run after the terminal result.
7. **Leave and come back.** Closing the tab or locking the phone does not stop
   the run — it is executing on the machine. Reopening (same device or another
   paired one) lists the session, restores the transcript, and follows a still
   live run through `session.activity({ from: cursor })`. Two devices watching
   one session each get their own stream from the daemon; whoever answers an
   approval first wins, and an abandoned ask fails closed on Mecatl's side.

### Every hop of one prompt

```mermaid
sequenceDiagram
  participant B as Browser (Mecatl SDK)
  participant C as Coordinator (BFF)
  participant W as Worker
  participant M as mecated (loopback)
  B->>C: POST /api/mecatl/<workerFp>/v1/... (Roost device JWT)
  C->>C: verifyJwt + resolveCallerPrincipal + live worker row
  C->>W: DMecatlRelayRequest{request_id, method, path, headers, body}
  W->>M: fetch 127.0.0.1:<port> + Bearer <daemon token>
  M-->>W: SSE: message.delta, tool.call, permission.ask, result
  W-->>C: WMecatlRelayChunk{head} then body chunks then end
  C-->>B: streaming Response body
  B->>B: SDK decodes events; Solid renders transcript + approval card
```

Three credential boundaries hold by construction: the browser presents only its
Roost device JWT, the coordinator presents nothing to the daemon, and the
daemon bearer exists only inside the worker process and its child env.

### What Roost builds vs. what it gets for free

Builds: the daemon supervisor, one relay frame pair, one authenticated BFF
route, and the agent pane's UI (the only substantial new UI — Mecatl ships no
web client, just the SDK and typed events).

Free from Mecatl: the agent loop, tool catalog, permission engine and approval
flow, model/provider selection, transcripts, durable sessions, compaction,
fork/clear/retry, cursor-based replay, and session inventory.

### What this is not

It does not replace, gate, or slow the PTY plane; a machine with no `mecated`
behaves exactly as it does today. It does not make Roost an agent runtime:
Roost still spawns no agent process, owns no conversation, and stores no
transcript.

## Approach

Six steps, in order. Steps 1–2 are independent of 5; 3 needs 1+2; 4 needs 2;
5 needs 4; 6 needs 1. The tree builds and every existing gate passes after
each step. Nothing in this plan touches the terminal data plane, `CellTerminal`,
`TerminalDeck`, `SessionEvent`, `foldEvent`, the Sync domain set, or the
coordinator schema — the agent surface is a second, independent pane reached
by its own route.

### 1. Worker supervises one `mecated` per machine

New directory `apps/worker/src/mecatl/`, file `daemon.ts` (keep under 400
lines; split spawn vs. readiness if it grows).

Config — extend the single Zod object in `apps/worker/src/config.ts` (schema
at lines 20-42, env map `withDefaults` at 49-80) with exactly three keys,
using the strict-literal parse style of `parseKeeperForceLiveRetire`
(config.ts:106-112) — an unrecognized value is a hard error, never a truthy
arming:

- `mecatlEnabled: boolean` from `ROOST_MECATL`, accepted values `"0" | "1"`,
  absent = `false`.
- `mecatlBin: string | undefined` from `ROOST_MECATL_BIN`; when set it MUST be
  absolute (`isAbsolute`) or `loadWorkerConfig` throws.
- `mecatlRoot: string` from `ROOST_MECATL_ROOT`, default `homedir()`; MUST be
  absolute.

Register the three names in `apps/worker/src/service-definition-env.ts`
alongside `KEEPER_FORCE_LIVE_RETIRE_ENV` and `AGENT_CONVERSATION_RESTORE_ENV`
so an installed LaunchAgent/systemd unit keeps them; then add them wherever
those two constants already appear (`grep -n KEEPER_FORCE_LIVE_RETIRE_ENV`
returns exactly the worker config read, the service-definition module, and the
roost-cli deploy env composer).

`startMecatlDaemon(cfg)` — modeled on the keeper spawn in
`apps/worker/src/keeper/keeper-pool-lifecycle.ts:53-95`:

1. Disabled (`mecatlEnabled === false`) → state `{ kind: "disabled" }`, no
   process, no log noise beyond one `log.info("worker", "mecatl_disabled", {})`
   at boot.
2. Resolve the binary: `cfg.mecatlBin ?? Bun.which("mecated")`. Null →
   `{ kind: "unavailable", reason: "binary_missing" }`.
3. Mint a per-daemon bearer: `randomBytes(32).toString("hex")`, passed ONLY in
   the child env as `MECATL_AUTH_TOKEN` (never argv — argv is world-readable in
   `ps`, and Roost's own agent scanner runs `ps -A`).
4. Allocate one loopback port by binding `Bun.listen({hostname:"127.0.0.1",
   port:0})`, reading the assigned port, and closing it immediately. Mecatl's
   ready file documents `grpc_address`, not the HTTP address, so Roost must own
   the number rather than discover it.
5. Spawn, holding stdin open as Mecatl's parent-liveness channel:
   `Bun.spawn({ cmd: [bin, "serve", "--http-addr", "127.0.0.1:<port>",
   "--grpc-unix-socket", <runtimeDir>/mecatl.sock, "--metrics-addr", "",
   "--workspace", cfg.mecatlRoot, "--store-dir", join(workerDataDir(),
   "mecatl", "sessions"), "--ready-file", <runtimeDir>/mecatl-ready.json,
   "--lifetime-stdin"], stdio: ["pipe", "ignore", <fd of
   workerLogDir()/mecatl.err.log>], env: { ...process.env, MECATL_AUTH_TOKEN }})`.
   Keep the returned `stdin` writable open for the process lifetime; closing it
   is the graceful-stop signal Mecatl documents.
6. Readiness is proven, never assumed (same rule the keeper comment states):
   poll for the ready file every 100 ms up to 10 s, require its `pid` to equal
   the spawned pid, then `GET http://127.0.0.1:<port>/v1/compatibility` with
   `authorization: Bearer <token>` and require HTTP 200 with `api_major === 1`.
   Failure → kill the child, state `{ kind: "unavailable", reason:
   "readiness_timeout" | "incompatible_api" }`.
7. On child exit while enabled: log `mecatl_exit` with the exit code, then
   restart with backoff 1 s, 2 s, 5 s, 5 s, 5 s — at most five starts per
   rolling five minutes; after that state `{ kind: "unavailable", reason:
   "restart_exhausted" }` and stop trying until the worker process restarts.
8. Worker SIGTERM: close the child's stdin and await exit with a 10 s bound
   (Mecatl drains and persists on that signal). This deliberately differs from
   `apps/worker/src/main.ts`, which leaves the keeper alive: keeper owns live
   PTYs, `mecated` owns nothing that a restart cannot recover from its
   `--store-dir`.

Exports: `mecatlDaemonState(): MecatlDaemonState` and
`mecatlLocalRequest(init): Promise<Response>` (the only place the bearer is
added). Wire the start call into `runWorker()` in `apps/worker/src/main.ts`
after `startCoordLink`, and the stop into its existing SIGTERM shutdown.

Provider credentials stay Mecatl's: the daemon reads
`~/.config/mecatl/{settings,auth}.yaml` or inherited provider env. Roost never
reads, writes, stores, or logs them. A machine with no provider makes `mecated`
exit at startup, which surfaces as `unavailable` with reason `daemon_exit`.

### 2. One relay frame pair on the existing worker link

Workers dial out; the coordinator cannot open a connection to a worker. The
agent surface therefore rides the existing worker WebSocket
(`apps/worker/src/transport/coord-link.ts`, `/ws/coord-worker/<fp>`) as a
generic HTTP relay, so Roost transports Mecatl's API without modeling it.

Add to `apps/shared/proto/roost/v1/worker_transport.proto` (verbatim):

```proto
message DMecatlRelayRequest {
  string request_id = 1;
  string method = 2;        // GET | POST | DELETE | PATCH
  string path = 3;          // path + query, always begins with "/"
  string headers_json = 4;  // JSON object of forwarded request headers
  bytes body = 5;
}
message DMecatlRelayCancel { string request_id = 1; }
message WMecatlRelayChunk {
  string request_id = 1;
  bool head = 2;            // first frame only: status + headers_json set
  uint32 status = 3;
  string headers_json = 4;
  bytes body = 5;
  bool end = 6;
  string error = 7;         // set with end=true when the relay failed
}
```

Oneof arms (use the next free tags; the implementer confirms the current
maxima in the file before assigning — `CoordWorkerDown` currently uses 1-21
plus 30, `CoordWorkerUp` 1-22 plus 30):
`DMecatlRelayRequest mecatl_relay_request`, `DMecatlRelayCancel
mecatl_relay_cancel` on `CoordWorkerDown`; `WMecatlRelayChunk
mecatl_relay_chunk` on `CoordWorkerUp`.

Regenerate with `bun run --filter='@roost/shared' proto:gen`, then complete the
established five-file sequence: `UpstreamFrame` union in
`apps/worker/src/transport/coord-link-types.ts`, encode in
`apps/worker/src/transport/coord-link-codec.ts` (follow the `"rpc-ok"` case at
lines 33-36), decode/dispatch in
`apps/worker/src/transport/coord-link-downstream.ts` (follow the
`"browserCommand"` case at line 106), coord-side dispatch in
`apps/coord/src/connect/worker-frame-dispatch.ts` (follow the `"rpcOk"` case at
line 361), and a send helper in `apps/coord/src/connect/worker-send.ts` beside
`sendAttachmentChunk` (line 376), which is the existing chunked-relay
precedent.

### 3. Worker-side relay executor

`apps/worker/src/mecatl/relay.ts`, called from the new downstream case.

- On `DMecatlRelayRequest`: reject with one `end`+`error` chunk when the daemon
  state is not ready (`error` = the state reason verbatim, e.g.
  `binary_missing`), when `path` does not start with `/v1/`, when the method is
  outside `GET|POST|DELETE|PATCH`, when `body` exceeds 1 MiB, or when eight
  relays are already in flight (`error = "relay_busy"`).
- Otherwise `fetch(baseUrl + path)` with the forwarded headers plus
  `authorization: Bearer <daemon token>`, `signal` from a per-request
  `AbortController` kept in a `Map<string, AbortController>`.
- Emit one `head` chunk (status + response headers JSON), then body chunks of
  at most 32 KiB read from `response.body.getReader()`, then one `end` chunk.
- Backpressure: await the link's existing send result before reading the next
  chunk (`TransportSendResult` in `coord-link-types.ts`); if the link reports a
  closed transport, abort the fetch and drop the entry.
- Bounds: 64 MiB total per relay and 60 s without any chunk from the daemon
  both end the relay with `end`+`error` (`response_too_large`,
  `upstream_idle`). SSE keepalives make 60 s safe.
- `DMecatlRelayCancel` aborts the controller and removes the entry; an unknown
  request id is a no-op.
- Log one line per relay start and end (`log.info("worker","mecatl_relay",…)`)
  carrying request id, method, path, status, byte count — never bodies,
  headers, or the token.

### 4. Coordinator BFF route

`apps/coord/src/mecatl-relay.ts` plus one branch in
`apps/coord/src/coord-factory.ts`, inserted BEFORE the generic
`url.pathname.startsWith("/api/")` 404 at line 126:

```ts
} else if (url.pathname.startsWith("/api/mecatl/")) {
  nonConnectSurface = "api";
  resp = await handleMecatlRelay(req, url, deps);
}
```

`handleMecatlRelay`:

1. Parse `/api/mecatl/<workerFp>/<rest>`; `rest` plus `url.search` becomes the
   relayed `path`, which must begin `/v1/`.
2. Authenticate exactly like `apps/coord/src/connect/sync-ws-upgrade.ts:139`:
   `verifyJwt(token, { db, cache: deps.jwtCache, jwtMaxAgeSecs:
   deps.cfg.jwtMaxAgeSecs })` on the `Authorization: Bearer` header, then
   `resolveCallerPrincipal(deps.db, verified)`; admit only
   `account-device` and `legacy-self-hosted` (the same authority
   `requireAccountDevice` grants Connect handlers). A `worker` principal or a
   null principal is 401. This is the whole authorization model — Roost is a
   single-install tenancy where an authenticated device reaches the install.
3. Require a live `workers` row for `<workerFp>` (`deleted_at_ms IS NULL`);
   otherwise 404.
4. Forward only `content-type` and `accept` from the browser request. The
   Roost JWT and every other header stop at the coordinator, and the daemon
   bearer is added on the worker — so browser JavaScript never holds a Mecatl
   credential.
5. Register a pending relay keyed `<workerFp>\0<requestId>` (`crypto.randomUUID`),
   send `DMecatlRelayRequest`, and return a `Response` whose body is a
   `ReadableStream` fed by the arriving chunks; the `head` chunk supplies status
   and `content-type`. Worker not connected → 503 with body
   `{"error":"worker_offline"}`.
6. `ReadableStream.cancel` (browser aborted / SSE closed) and a 512 KiB
   unread-queue ceiling both send `DMecatlRelayCancel` and drop the entry. A
   relay with no chunk for 90 s is cancelled the same way.
7. Cap 16 concurrent relays per worker fingerprint; over it, 429 with
   `{"error":"relay_busy"}`.

### 5. The `/agent` surface in the SPA

Add the dependency `@stacklok-oss/mecatl-sdk` to `apps/web/package.json`.

- `apps/web/src/routes.ts`: add `AGENT: "/agent/:workerFp?"` to `ROUTES` and an
  `agentHref(workerFp: string): string` builder beside `browseHref`.
- `apps/web/src/App.tsx`: one `<Route path={ROUTES.AGENT} …>` inside the
  existing `AppShell` route, code-split with `lazy` exactly like
  `GlobalSearchPage` is split in `MainPane.tsx:38-40`.
- `apps/web/src/components/MainPane.tsx`: add `const isAgent = createMemo(() =>
  location.pathname.startsWith("/agent"))`, include it in `overlayActive()`
  (line 193), and render `<Show when={isAgent()}><AgentPane /></Show>` next to
  the existing `<Show when={isSearch()}>` at line 223. This is the sanctioned
  additive pane pattern: the terminal deck host is visibility-flipped, never
  unmounted, so opening an agent pane cannot disturb a live terminal.
- `apps/web/src/components/layout/WorkbenchActivityBar.tsx`: one `<A>`
  destination plus its pathname predicate, matching the five existing entries.
- `apps/web/src/components/Agent/mecatlClient.ts`: build the client with the
  documented browser transport and Roost's existing per-call signer —
  `connect({ baseUrl: \`/api/mecatl/${workerFp}\`, fetch: roostFetch })` where
  `roostFetch` clones the request and sets `Authorization: Bearer <jwt>` from
  the same signer `apps/web/src/connect.ts:100-101` uses. `HttpTransportOptions`
  declares `fetch?: typeof globalThis.fetch`, so no SDK fork is needed.
- `apps/web/src/components/Agent/AgentPane.tsx` (+ small sibling components,
  400-line cap): machine picker over `rootStore` workers; session list from
  `client.sessions.list(...)`; transcript from `session.transcript()`;
  live events by iterating `session.run(text)`; reconnect/refresh via
  `session.activity({ from: cursor })` holding the cursor in component state;
  permission asks rendered from the `permission.ask` event and answered with
  `run.resolveAsk(askId, "allow_once" | "allow_always" | "deny")`; cancel via
  `run.cancel()`.
  Compose only from `apps/web/src/components/Settings/md/primitives.tsx`
  (`Surface`, `Card`, `List`/`ListRow`, `Chip`, `StatusDot`, `Button`,
  `Dialog`, `EmptyState`, `TextField`) and `--md-*` tokens; copy the pane
  structure of `apps/web/src/components/GlobalSearchPage.tsx`. No raw hex, px
  font sizes, or hand-rolled status dots — the raw-value ratchet fails the
  build otherwise.
- Mid-run steering is supported over HTTP/SSE via
  `POST /v1/sessions/{id}/steer` + `POST /v1/sessions/{id}/cancel-steer`, but
  only on servers that advertise it: probe the session capability snapshot for
  `steer` and `/v1/compatibility` for the `http_steer` feature, and never probe
  by 404. When either is absent, keep the composer enabled and send the text as
  the next run once the current one ends. Do not emulate steering.
- Unavailable machine (relay 503, or a daemon-state error body): render
  `EmptyState` with the literal text `Mecatl is not running on this machine.`
  and detail `Install mecated and configure a model provider, then set
  ROOST_MECATL=1 for this worker.`
- Nothing from this pane is written to `rootStore`, Sync, or localStorage
  beyond the last-used `workerFp`.

### 6. Operator visibility

- `apps/roost-cli/src/status-types.ts` + `status-report.ts` +
  `status-output.ts`: one additional check line per worker,
  `mecatl runtime` → `ready` / `disabled` / `unavailable: <reason>`, sourced
  from the worker's existing presence payload (extend the presence/diag field
  the worker already reports rather than adding an RPC).
- `apps/roost-cli/src/doctor.ts`: one new `SOURCES` row for
  `<workerLogDir>/mecatl.err.log` so `roost doctor --since 24h` digests daemon
  failures alongside coord/worker/keeper.

## Critical files & anchors

- `apps/worker/src/keeper/keeper-pool-lifecycle.ts:53-95` — the spawn +
  proven-readiness pattern step 1 copies (readiness is a successful protocol
  call, never endpoint existence).
- `apps/shared/proto/roost/v1/worker_transport.proto:190-208,385-407` — the two
  frame oneofs that receive the relay arms; tag maxima live here.
- `apps/coord/src/coord-factory.ts:100-142` — the non-Connect fetch router; the
  relay branch goes before the `/api/` 404.
- `apps/coord/src/connect/sync-ws-upgrade.ts:130-150` — the verbatim
  non-Connect `verifyJwt` + principal sequence step 4 reuses.
- `apps/web/src/components/MainPane.tsx:189-225` — `overlayActive()` and the
  overlay `<Show>` block; the only place a new pane kind is selected.

## Verification

Prerequisite for the end-to-end proof: `mecated` on `PATH`
(`brew install stacklok/tap/mecatl`, or a release archive from
github.com/stacklok/mecatl), verified with `mecated --version`. No model
credentials are needed — Mecatl ships offline mock providers.

1. Unit, hermetic, no daemon: new tests beside their sources
   (`apps/worker/tests/mecatl-*.test.ts`, `apps/coord/tests/mecatl-relay.test.ts`).
   Point the relay executor at a throwaway `Bun.serve` that returns a fixed SSE
   body and assert the observable contract: chunk order `head → body* → end`,
   `relay_busy` past the ninth concurrent relay, `response_too_large` past
   64 MiB, cancel aborting the upstream fetch, a non-`/v1/` path refused, and a
   coordinator 401 for a worker-principal JWT and 404 for an unknown worker fp.
2. `bun run lint`, `bun run test:unit`, `bun run test:worker`,
   `bun x tsgo -p tsconfig.base.json --noEmit`, `bun run test:terminal`,
   `bun run test:upgrade` — the standing gates. `test:terminal` is the proof
   that the additive pane did not disturb the terminal plane.
3. End-to-end, the deliverable proof (run from the repo root):
   - `bun run --cwd apps/web build`
   - `ROOST_MECATL=1 ROOST_MECATL_ROOT="$PWD" bun smoke/terminal/live-stack.ts`
     — it prints `READY <url> worker=<fp>`.
   - Open `<url>/agent/<fp>` in a browser paired to that coordinator. Expected
     observable result: the pane lists zero sessions, "New session" creates one
     through the relay, the prompt `Say hello and then stop.` streams assistant
     text into the transcript, and a tool-requiring prompt renders a permission
     card whose `Allow once` resumes the run. Reloading the page restores the
     transcript from `session.transcript()` with the conversation intact.
   - Confirm the credential boundary in the browser devtools network tab: every
     request goes to `/api/mecatl/<fp>/v1/...` on the Roost origin, and no
     response or request carries `MECATL_AUTH_TOKEN`.
   - Kill the daemon (`pkill -f 'mecated serve'`) and observe the pane reporting
     the machine unavailable, then recovering on the worker's restart backoff
     without a page reload beyond the pane's own retry.
4. Negative control: with `ROOST_MECATL` unset, the same stack must show the
   `Mecatl is not running on this machine.` empty state and log
   `mecatl_disabled` once — and every terminal gate must still pass.

## Assumptions & contingencies

- **One daemon per machine, rooted at `ROOST_MECATL_ROOT` (default `$HOME`).**
  Mecatl placement is server-owned and per-daemon, so per-folder agent sessions
  would mean a daemon pool. If folder-scoped agents turn out to be required,
  add a pool keyed by absolute root with idle reaping — the daemon module is
  written to be instantiated more than once, but v1 starts exactly one.
- If `mecated` rejects an empty `--metrics-addr`, pass a second
  Roost-allocated loopback port instead; if it rejects `--grpc-unix-socket`
  while HTTP is enabled, drop that flag and pass a third allocated port as
  `--grpc-addr`. Neither changes the relay.
- If the installed SDK version's browser transport does not accept `fetch`,
  build a Connect-ES transport with an auth interceptor and pass it as
  `InjectedTransportOptions.transport`; if the package cannot be added to the
  workspace at all, call the documented HTTP/SSE routes
  (`/docs/reference/http-sse-api`) directly with `fetch` and `EventSource`-style
  parsing. The pane's behavior and this plan's other steps are unchanged.
- Installing or pinning `mecated` through Roost's own installer is out of scope:
  the operator installs it, and the worker auto-detects it. The repo already
  pins external binaries by SHA-256 (Shawl) if that changes later.


---

## Parked state — resume here (2026-09-16)

Branch `mecatl-agent-panes`. Verified on that branch unless marked otherwise.

### Done and proven

- **Step 1 — worker supervises `mecated`.** `apps/worker/src/mecatl/daemon.ts`,
  the three `ROOST_MECATL*` config keys, service-definition + deploy
  registration, and boot/shutdown wiring in `main.ts`.
  `apps/worker/tests/mecatl-daemon.test.ts` (6 pass) pins: an opted-out machine
  starts nothing and refuses relayed requests with reason `disabled`; readiness
  requires the ready file's OWN pid plus a 200 from `/v1/compatibility`; a
  readiness document from another process never counts; `api_major != 1` is
  refused as `incompatible_api`; a killed daemon restarts with a new pid and
  serves again; `stop()` ends the process and stops reporting ready.
- **Step 2 — wire contract.** `DMecatlRelayRequest` / `DMecatlRelayCancel`
  (CoordWorkerDown 22/23) and `WMecatlRelayChunk` (CoordWorkerUp 23), generated.
- **Step 4 — coordinator BFF.** `apps/coord/src/mecatl-relay.ts`,
  `connect/worker-send-mecatl.ts`, the `/api/mecatl/` branch in
  `coord-factory.ts`, the `mecatlRelayChunk` dispatch arm.
  `apps/coord/tests/mecatl-relay.test.ts` (8 pass).
- **Step 6 — operator visibility.** `MecatlRuntimeReport` (shared schema +
  proto), the heartbeat field AND its `main.ts` caller,
  `workers.mecatl_runtime_json` (migration 0032), the `roost status` line, and
  the `mecatl.err.log` doctor source.

### Done but NOT proven

- **Step 3 — worker relay executor** (`apps/worker/src/mecatl/relay.ts`, the
  `coord-link-*` arms, and its `refs.mecatlRelay` binding). Typechecks and
  lints; its slice was cancelled before writing
  `apps/worker/tests/mecatl-relay.test.ts`. **Write that test first**: chunk
  order head → body* → end, `relay_busy` past 8 concurrent, `bad_path`, cancel
  aborting the upstream fetch, and a not-ready daemon's reason.
- **Step 5 — `/agent` pane** (`apps/web/src/components/Agent/**`, route,
  activity-bar entry, MainPane overlay). Builds and typechecks against
  `@stacklok-oss/mecatl-sdk@0.2.0`; never rendered against a real daemon.

### Next actions, in order

1. `apps/worker/tests/mecatl-relay.test.ts`.
2. The end-to-end proof in Verification above. It needs `mecated` installed
   (`brew install stacklok/tap/mecatl`); the box this was built on had neither
   `mecated` nor `mecatui`, so no code path here has ever met a real daemon.
3. Gates not yet run on this branch: `bun run test:unit`,
   `bun run test:terminal`, `bun run test:upgrade`. Green already:
   `bun run lint`, `bun run test:worker`, the web build, and
   `tsgo -p tsconfig.base.json --noEmit` for every file in this change.

### Decisions that differ from the body above

- `MecatlUnavailableReason` carries `disabled`, `not_ready` and `stopped` in
  addition to the failure reasons, so the pane never reports a phantom crash
  for a machine that is merely opted out, still starting, or shut down.
- The daemon's gRPC unix socket is named per launch attempt
  (`<pid>-<random>.sock`) and unlinked when that daemon dies. Mecatl refuses to
  replace a LIVE listener, and a restart fires on backoff without waiting for
  the previous daemon to finish draining, so a fixed name would make every
  retry after a readiness failure refuse until the budget ran out.
- `sessions.list` passes a literal typed `satisfies ListSessionsRequest`
  instead of `create()`: the SDK bundles `@bufbuild/protobuf` 2.14.1 while this
  workspace pins 2.12.0, so the two `Message` brands cannot unify. Aligning the
  workspace pin to 2.14.1 would remove the cast.

### Incident to be aware of

While cleaning up after the cancelled subagents, this session ran
`git checkout --` on `apps/web/src/components/cell-terminal-presentation.ts`
and `apps/web/tests/cellTerminalPresentation.test.ts`, which were part of a
concurrent terminal-plane change in the working tree, not part of this feature.
That work is unrecoverable from git. Nothing else in that change set was
touched, and none of it is in this branch's commits.

### Follow-up worth doing after the pane is proven: fleet-wide Mecatl config

The operator value the pane is really chasing is "configure agents once for
the whole fleet" instead of per-harness, per-machine. Mecatl gets you one
schema, but its files are still per machine: `$XDG_CONFIG_HOME/mecatl/
settings.yaml` (providers, models, permissions, posture, MCP, command_runner)
and `auth.yaml` (provider credentials), both read by the daemon at startup —
it has no hot reload, so applying a change means restarting the daemon.

Roost already owns a mechanism for exactly this: `install-integrations.ts`
plans and writes owned asset files onto each worker through a staged,
rollback-capable transaction with per-asset ownership markers, refusing paths
it does not own. Distributing a coordinator-held `settings.yaml` through that
path (and restarting the supervised daemon after a write) turns provider,
model, permission posture and MCP selection into one setting in Roost that
converges on every machine.

Scope is `settings.yaml` ONLY. `auth.yaml` is out, and not as a "decide later":
step 1 of this plan states that provider credentials stay Mecatl's and that
Roost never reads, writes, stores, or logs them. Distributing them would make
the coordinator a secret store and put plaintext provider keys on every
worker's disk. An operator installs credentials per machine, or points the
fleet at a gateway.

Prerequisite: the daemon supervisor must gain a restart-on-config-change path,
because Mecatl reads both files only at startup.

One plane, not one value: a fleet default that a machine may override is the
target shape, and both halves already have a seam. Roost's deploy composer
treats `DEPLOY_HOST_LOCAL_ENV_KEYS` as settings that belong to the target box
— the installed value wins and a deploy only seeds a first install — which is
exactly the override rule (`ROOST_MECATL_ROOT` already rides it). Mecatl has
its own tier for the same purpose: a trusted project contributes
`.mecatl/settings.yaml` or `.mecatl/settings.local.yaml`, while operator-only
settings (providers, credentials, guardrail configuration) stay user-global
and a project cannot weaken them. So the fleet default lives in Roost, a
machine keeps whatever it was deliberately given, and a repo can narrow its
own agent behavior without reaching operator authority.
