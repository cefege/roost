# @roost/worker

The Bun process on every machine (macOS, Linux, or Windows) in the fleet. It owns every session's shell PTY, holds
the one authoritative terminal grid, and relays PTY bytes both ways over a single **outbound** WebSocket. Agent CLIs
are ordinary programs launched inside those PTYs; the worker never interprets agent output and exposes no
transcript, tool, or approval state. It owns **no listener** — no inbound HTTP or WS surface exists.

Path references are relative to `apps/worker/` unless they start at the repo root (`apps/…`, `scripts/…`, `smoke/…`, `docs/…`).

## Entry point

`src/main.ts` exports exactly one symbol, `runWorker()`, and the boot order it enforces:

1. `handleKeeperSurvivor()` (`src/boot-keeper.ts`) and `prepareWtermCoreModule()`
   (`@roost/shared/wterm-core-factory`) in parallel — adopt or retire a keeper that outlived the last worker while
   the patched WTerm module compiles. Then `loadWorkerConfig()` (`src/config.ts`), `loadWorkerKey()` (`src/jwt.ts`),
   and relocation recovery (`src/coord-relocation.ts`, `src/coord-target.ts`), which can rewrite
   `cfg.coordinatorUrl` before any dial.
2. `runInstall()` (`src/install.ts`) — awaited **only** when a bootstrap token is present (first boot), otherwise
   fire-and-forget. Nothing external may gate CoordLink or the heartbeat; a wedged `tailscale status` used to
   stall the whole worker.
3. `startCoordLink(buildCoordLinkDeps({...}))`, then `SessionManager`, `AgentStatusRegistry` +
   `AgentScreenDetector`, `serveServiceHealth`, `startAgentReportServer`, `startHeartbeat`,
   `reconcileOpenSessions("boot")`, `emitSnapshot`. SIGTERM/SIGINT run one idempotent `shutdown()` that
   deliberately does **not** kill the keeper — it self-terminates when its endpoint disappears.

`src/coord-link-deps.ts` owns `buildCoordLinkDeps(ctx)`: the whole `startCoordLink()` dependency object, i.e. every
coord→worker callback the worker answers. It uses a **forward ref** (`CoordLinkRefs`), not closures — the
`CoordLink`, the `SessionManager` and the `AgentStatusRegistry` are all constructed *from* this object, so none of
them exists when it is built. Callbacks read `refs.sessionMgr` / `refs.link` through a getter that throws while
unbound, and `runWorker()` binds each ref the instant it exists; a null read there is a boot-wiring bug, never a
race. Add a callback here, not in `src/main.ts`.

## Transport — outbound only

`src/transport/coord-link.ts` is the composer: it dials a long-lived raw Bun `WebSocket` at
`<coordinatorUrl>/ws/coord-worker/<fp>?token=<jwt>` and owns the FSM (`idle → connecting → open → reconnecting →
…`, plus `closed` on `dispose()`), the hello, the stale-link watchdog, in-band JWT refresh, relocate, and `state()`.
Every browser command arrives *downstream* on this one socket. Frames are proto-typed `CoordWorkerUp` /
`CoordWorkerDown` oneofs (`@roost/shared/proto/worker_transport_pb`), serialized binary — no JSON on the hot path.
The JWT rotates **in band** via the `refreshJwt` frame 30 s before its 300 s TTL, so one stream stays open for hours.

- `src/transport/coord-link-outbox.ts` — encoded outbox: two bounded pending lanes (control, raw metadata) plus
  native-backpressure admission against `ws.bufferedAmount`. `drainQueues()` ordering is load-bearing — durable and
  control chronology always fence cells and raw metadata.
- `src/transport/coord-link-unacked.ts` — at-least-once `SessionEvent` ledger: the unacked map that survives a
  socket swap, over the durable `client_seq` in `src/transport/client-seq.ts`.
- `src/transport/coord-link-reconnect.ts` — backoff ladder (500 ms → 30 s, escalating to 5 min only on a real
  non-open streak). A worker is a daemon; nothing here ever gives up.
- `src/transport/coord-link-downstream.ts` — dispatch for every `CoordWorkerDown` variant, the per-kind
  terminal-control admission slots (input and viewport hold independent budgets, so a viewport flood cannot starve
  typing), and the monotonic budget from coord's *relative* `budget_ms`. Codecs, tuning knobs and the type surface
  are in `src/transport/coord-link-{codec,constants,types}.ts`.

All kebab-case. There is no `CoordLink.ts`.

## Keeper

**One** multiplexed Bun subprocess per worker hosts **all** PTYs over one local endpoint (UDS on POSIX, named pipe
on Windows — `@roost/shared/local-endpoint`). It is spawned `detached`, so PTYs survive a worker restart or deploy:
boot re-probes the endpoint, adopts a build-compatible survivor (`src/boot-keeper.ts`,
`src/keeper/keeper-probe.ts`, `src/keeper/keeper-stamp.ts`) and resumes its channels (`src/session-resume.ts`). A
POSIX keeper shuts itself down when its endpoint file is removed. Bun 1.3's native `Bun.spawn({terminal})` is the
PTY; node-pty and `ROOST_KEEPER_MODE` are retired.

- `src/keeper/protocol.ts` is the entry point: it holds the frame diagram (`[4B BE total][1B type][2B BE
  channel_id][payload]`), the wire-version bump log, and `KEEPER_PROTOCOL_VERSION`, and re-exports three families —
  `src/keeper/protocol-envelope.ts` (envelope, spawn, scalar codecs), `src/keeper/protocol-io.ts` (hello handshake, typed `PtyIn`),
  `src/keeper/protocol-terminal.ts` (resize control, authoritative terminal state, ordered history). There is no
  `protocol-v1.ts` and no `protocol-v2.ts`: the version is a **field**, not a filename, and a mismatch is reported
  rather than dispatched across.
- **Decode hazard, load-bearing.** `decodeMuxFrames()` returns each frame's `payload` as a `subarray` **view** onto
  the streaming receive buffer, valid only until the next read. Synchronous readers may use the view; anything
  outliving the read MUST copy. That is why the frame handler wraps input in `Buffer.from(...)` before queueing it
  for `proc.terminal.write` (`src/keeper/keeper-frame-handler.ts`), and why retained history chunks are copied out
  (`src/keeper/keeper-pool-lifecycle.ts`). Skipping the copy yields garbage PTY bytes under load.
- Keeper side: `src/keeper/multiplexed-main.ts` (entry, listener, endpoint watchdog), `src/keeper/keeper-frame-handler.ts`
  (frame dispatch, the real PTY spawn site), `src/keeper/keeper-types.ts`, `src/keeper/keeper-log.ts`, `src/keeper/keeper-process-reap.ts`,
  `src/keeper/histfile.ts`. Worker side: `src/keeper/multiplexed-client.ts` (the pool) + `src/keeper/keeper-pool-{lifecycle,channels,io,config}.ts`.

## Module map

- **Boot** — `src/main.ts`, `src/coord-link-deps.ts`, `src/boot-keeper.ts`, `src/boot-reconcile.ts`,
  `src/install.ts`, `src/config.ts`, `src/jwt.ts`. **`src/transport/`** — the outbound coord link (above).
  **`src/keeper/`** — the PTY host (above).
- **Session family**, one owner split across `this`-bound modules: `src/session-manager.ts` (maps + delegating
  wrappers), `src/session-record.ts`, `src/session-constants.ts`, `src/session-spawn.ts`, `src/session-resume.ts`,
  `src/session-lifecycle.ts`, `src/session-emit.ts`, `src/session-terminal-control.ts`,
  `src/session-terminal-txn.ts`, `src/session-resize-capture.ts` (adoption only),
  `src/session-control-lanes.ts`, `src/session-scrollback.ts`, `src/session-scrollback-ring.ts`,
  `src/session-unhandled-seq.ts`, `src/session-git-ports.ts`, plus the channel FSM in `src/fsm.ts`.
- **Browser RPCs** — `src/browser-command-handler.ts` dispatches every `ClientControlFrame` variant to
  `src/browser-command-{spawn,terminal,files,attachments,transfer,diag}.ts`, answering upstream as `rpc-ok` /
  `rpc-error`.
- **`src/agent-status/`** — volatile per-session agent state (below). **`src/util/`** — `src/util/mono.ts` (monotonic ms
  behind every terminal-control deadline), `src/util/path.ts`.
- **Host + coord plumbing** — `src/heartbeat.ts` with `src/host-sample-{darwin,linux,win32,types}.ts`;
  `src/coord-client.ts` (Connect client, boot calls only — events ride CoordLink); `src/event-sink.ts`;
  `src/snapshot.ts`. **Coordinator move** — `src/coord-target.ts`, `src/coord-relocation.ts`,
  `src/coord-relocation-recovery.ts`, `src/coord-relocation-windows{,-runtime}.ts`.
- **Session metadata pushed to the SPA** — `src/git-branch.ts`, `src/pr-status.ts`, `src/listening-ports.ts`.
  **Files + attachments** — `src/file-rpcs.ts`, `src/attachment-upload.ts`, `src/attachment-reaper.ts` (1 h sweep,
  24 h TTL, 1 GB LRU). **Terminal byte analysis** — `src/terminal-stream-scan.ts` (alt-screen transitions),
  `src/terminal-query-reply.ts`, `src/shell-spec.ts`, `src/wterm-serialize.ts` (test utility), and
  `src/diag/byte-capture.ts` (last 256 KB of PTY output per session, for `diag-dump-bytecap`).

## Invariants

- **`SessionManager`'s live maps are keyed by `channelId`, not `SessionId`** —
  `sessions`, `terminalStreams`, cell emission state and raw metadata queues all
  share that key. Reach a session by sid via `getBySessionId()`; an ad-hoc
  sid-keyed owner will diverge.
- **The worker holds the one authoritative grid.** History is served as
  immutable cell rows by `handleGetScrollbackCells`; the browser paints rows
  as-is and never reflows them. Reads are epoch-fenced against the terminal
  control lane. `getScrollbackSince` remains retired.
- **The coordinator owns viewer membership and SCD.** The worker receives one
  `DTerminalStreamState` per channel with an already-aggregated geometry and
  never keeps per-viewer claims, freshness timers, or withdraw grace. A new
  stream ID gates deltas until its complete full cursor is installed and sent.
  Disable gates cell emission without changing the keeper/core geometry.
- **Live resize mutates the existing core at the keeper boundary.** The
  `ResizeAck` callback synchronously calls `wtermCore.resize` before any later
  `PtyOut` from that ordered stream can dispatch. No ordinary viewer change
  allocates a core or replays the raw ring. `src/session-resize-capture.ts` is
  reachable only for degraded adoption when no in-memory core exists.
- **`Bun.spawn({terminal})` does not inject `TERM` into the child env** (node-pty did). A locally bootstrapped
  worker inherits `TERM` from its terminal and hides the bug; an SSH-deployed one sees `TERM=""`/`unknown` →
  backspace echoes wrong and ncurses dies with `cannot initialize terminal type`. The real spawn site in
  `src/keeper/keeper-frame-handler.ts` sets `TERM: "xterm-256color"` explicitly. Guard: lint rule `L11: keeper
  Bun.spawn env must set TERM explicitly (deployed-only ncurses $TERM=unknown)` in `scripts/lint-roost.ts`. A test
  that passes `TERM` in itself falsely covers this.
- **`agent-status` is volatile metadata on a shell PTY.** No SQLite row, no event-log variant, no `session.kind`.
  It ships as `CoordWorkerUp.agent_status`, is resent after every CoordLink reopen, and is dropped when the session
  closes; a restart must re-derive it. Do not persist it and do not promote it to a session kind.
- **Keeper input correlation is worker-owned.** Browser-local `input_seq` and
  worker request IDs correlate their respective hops only. The keeper receives
  a monotonically increasing per-channel/connection key allocated by the
  worker, so simultaneous devices using the same local sequence cannot replace
  each other's pending result.

## Agent status

Labels each shell PTY `working` / `blocked` / `idle` for whatever coding agent runs inside it. Metadata only, never
an agent API. All under `src/agent-status/`.

- `src/agent-status/process-scan.ts` — throttled (250 ms) `ps -A` snapshot finds a known agent in the session's process tree;
  identity survives one missed scan so a momentary miss cannot flap. `src/agent-status/detector.ts` wires the pieces together.
- `src/agent-status/report-server.ts` — authoritative reports on `$ROOST_AGENT_SOCKET_PATH` (default `~/.roost/agent-report.sock`,
  dir `0700`, socket `0600`): one `agent.report` JSON line per request, ≤4 KiB, 32 per connection, `seq` monotonic,
  `active:false` withdraws, `pid` resolved to its owning session with a mismatch rejected
  (`pid_session_mismatch`) so a report cannot claim another terminal. `src/agent-status/environment.ts` injects
  `ROOST_AGENT_SOCKET_PATH` + `ROOST_SESSION_ID` into every spawned shell.
- `src/agent-status/install-integrations.ts` — the two owned `roost-agent-state.ts` extensions (mode `0600`, temp-file + rename,
  idempotent) under `${PI_CONFIG_DIR:-~/.omp}/agent/extensions` and `${PI_CODING_AGENT_DIR:-~/.pi/agent}/extensions`;
  one lacking a `ROOST_INTEGRATION_ID=<id>` marker is a user file and is never overwritten, and
  `ROOST_AGENT_STATUS_DISABLED=1` makes an installed one inert.
- `src/agent-status/registry.ts` — an integration report wins while its 30 s lease is fresh, else the screen fallback
  (`src/agent-status/manifests.ts` + `src/agent-status/manifest-engine.ts` + `src/agent-status/stable-detection.ts`, pinned from Herdr `eacea2da`, Apache-2.0), where
  a plain `working → idle` needs repeated confirmation so a redraw cannot flicker a completion. Changes carry a
  monotonic revision; `working|blocked → idle` also stamps the completion revision the SPA's badge keys on.

## Run, test, deploy

- **Run from source** — `bun apps/worker/src/main.ts`, or `bun --filter @roost/worker run dev` to watch.
- **Test: `bun run test:worker`.** That is `scripts/test-worker.ts`: it globs the 53
  `apps/worker/tests/**/*.test.ts` files and runs **each one in its own `bun test` child** with an isolated temp
  root (`TMPDIR` plus a fresh `ROOST_WORKER_DATA_DIR` inside it, every inherited `ROOST_*` var stripped), so each
  file gets its own keeper subprocess, keeper socket dir and sqlite. Pool caps at 4 concurrent children
  (`ROOST_WORKER_TEST_JOBS` overrides, clamped to 1..cores); per-test timeout 30 s, per-file hang backstop 90 s.
  Every file runs even after one fails; the run exits with the first failure's code.
- **Do not run `bun test apps/worker/tests/`.** That executes all 53 files in **one** process sharing one temp root,
  one keeper socket dir and one data dir; the files then contend over the same keeper, PTYs and sqlite, producing
  load-dependent failures unrelated to your change. That is a property of the command, not of the code — never
  "fix" a test because of it. To iterate on one file, reproduce the isolation by hand, other `ROOST_*` unset:
  `TMPDIR=$(mktemp -d) ROOST_WORKER_DATA_DIR="$TMPDIR/worker-data" bun test --timeout 30000 apps/worker/tests/fsm.test.ts`
- **Wire changes** — add the variant in `apps/shared/src/wire/event.ts` (or the proto under
  `apps/shared/proto/roost/v1/`, then `bun --filter @roost/shared run proto:gen`) *first*, then implement here.
  Import shared code by subpath (`@roost/shared/wire`, `@roost/shared/log`, `@roost/shared/diag`,
  `@roost/shared/viewport`) — there is no barrel.
- **Install as a service** — `bash apps/worker/scripts/install.sh install` (macOS launchd LaunchAgent, Linux
  systemd `--user` unit). **Deploy to a fleet host** — `bun apps/roost-cli/src/main.ts deploy <host>`.
