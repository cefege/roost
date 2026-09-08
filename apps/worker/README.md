# @roost/worker

The Bun process on every released fleet machine (macOS or Linux; the retained
Windows implementation is paused). It owns every session's shell PTY, holds the
one authoritative terminal grid, and relays PTY bytes both ways over a single
**outbound** WebSocket. Agent CLIs are ordinary programs launched inside those
PTYs; the worker never interprets agent output and owns no agent process,
conversation, transcript, tool call, or approval model. It may persist an
official opaque conversation reference as private recovery metadata. With the
explicit POSIX-only restore gate enabled, involuntary loss may produce one
worker-owned OMP resume input only after ordinary shell respawn; this does not
turn the reference into public state or an agent-control channel. The worker
owns **no listener** — no inbound HTTP or WS surface exists.

Path references are relative to `apps/worker/` unless they start at the repo root (`apps/…`, `scripts/…`, `smoke/…`, `docs/…`).

## Entry point

`src/main.ts` exports the `completeWorkerBootAdmission()` test seam and
`runWorker()`. The latter enforces this boot order:

1. Compile the patched WTerm core, then load config and key material.
2. `runInstall()` (`src/install.ts`) is awaited only when a bootstrap token is
   present; otherwise it and agent-integration installation cannot gate the
   coordinator link or heartbeat.
3. Open `SessionEventStore`, pass it through `src/coord-link-deps.ts` into
   `startCoordLink()`, bind `coordLinkSink()`, then construct `SessionManager`,
   agent status, local health/report servers, and heartbeat.
4. Reconciliation serializes reference reports while the CoordLink exactly
   replays and ACKs all durable session events, then reads the coordinator's
   worker-only recovery rows. This replay barrier does not wait for snapshot
   activation and does not gate ordinary respawn work.
5. `completeWorkerBootAdmission()` invokes `handleKeeperSurvivor()` only after
   coordinator admission has reserved every durable session outcome. Success
   activates the `src/snapshot.ts` provider and marks health ready.
   Failed adoption follows the ordinary replacement-shell path; only after its
   durable `respawned` admission may the opt-in restore path submit one input.
   Successful adoption submits none.
   SIGTERM/SIGINT close the long-lived owners and event store but deliberately
   do **not** kill the keeper.

`src/coord-link-deps.ts` owns `buildCoordLinkDeps(ctx)`: the whole `startCoordLink()` dependency object, i.e. every
coord→worker callback the worker answers. It uses a **forward ref** (`CoordLinkRefs`), not closures — the
`CoordLink`, the `SessionManager` and the `AgentStatusRegistry` are all constructed *from* this object, so none of
them exists when it is built. Callbacks read `refs.sessionMgr` / `refs.link` through a getter that throws while
unbound, and `runWorker()` binds each ref the instant it exists; a null read there is a boot-wiring bug, never a
race. Add a callback here, not in `src/main.ts`.

## Transport — outbound only

`src/transport/coord-link.ts` is the composer: it dials a long-lived raw Bun `WebSocket` at
`<coordinatorUrl>/ws/coord-worker/<fp>` and authenticates with the exact `roost-worker-auth` marker plus JWT
subprotocol pair. It owns the FSM (`idle → connecting → open → reconnecting → …`, plus `closed` on `dispose()`).
Every browser command arrives *downstream* on this one socket. Frames are proto-typed `CoordWorkerUp` /
`CoordWorkerDown` oneofs (`@roost/shared/proto/worker_transport_pb`), serialized binary — no JSON on the hot path.
The JWT rotates **in band** via the `refreshJwt` frame 30 s before its 300 s TTL, so one stream stays open for hours.

- `src/transport/coord-link-outbox.ts` — protocol-barrier ordering for the
  pending transport lanes; `src/transport/coord-link-native-writer.ts` owns
  native WebSocket byte admission. `src/transport/coord-link-agent-status.ts`
  retains the last possibly-sent occupant retirement plus the latest active
  occupant per session, so backpressure cannot invert a replacement.
- `src/transport/coord-link-unacked.ts` — one-at-a-time
  hello → durable replay → snapshot → live protocol driver. It coalesces
  metadata in memory, hands exact durable-event ACKs to the store, and exposes
  a replay-drained barrier before snapshot-provider activation.
- `src/transport/session-event-store.ts` and its focused database/schema/
  sequence modules — bounded SQLite `SessionEventStore`: crash-safe
  `opened`/`closed`/`respawned`/`agent_reference` rows, pre-mutation capacity
  reservations, exact-ACK deletion, and block-reserved `client_seq`. Startup
  transactionally migrates schema-v1 `lifecycle_events` into schema-v2
  `session_events` without changing pending rows or sequence state. It imports
  the legacy text watermark once; standalone client-sequence ownership remains
  retired.
- `src/transport/coord-link-reconnect.ts` — backoff ladder (500 ms → 30 s, escalating to 5 min only on a real
  non-open streak). A worker is a daemon; nothing here ever gives up.
- `src/transport/coord-link-downstream.ts` — dispatch for every
  `CoordWorkerDown` variant, including dedicated `DAgentPrompt`, the per-kind
  terminal-control admission slots, and the monotonic budget from coord's
  *relative* `budget_ms`. Raw input and viewport retain independent budgets, so
  a viewport flood cannot starve typing.
  Codecs, tuning knobs, and the type surface are in
  `src/transport/coord-link-codec.ts`,
  `src/transport/coord-link-constants.ts`, and
  `src/transport/coord-link-types.ts`.

All filenames are kebab-case; do not add a parallel PascalCase entry.

## Keeper

**One** multiplexed Bun subprocess per worker hosts **all** PTYs over one local endpoint (UDS on POSIX, named pipe
on Windows — `@roost/shared/local-endpoint`). It is spawned `detached`, so PTYs survive a worker restart or deploy:
boot re-probes the endpoint, adopts a protocol-compatible survivor after a generated
`KeeperContractV1` probe (`src/boot-keeper.ts`, `src/keeper/keeper-probe.ts`,
`src/keeper/keeper-stamp.ts`) and resumes its channels (`src/session-resume.ts`). A
POSIX keeper shuts itself down when its endpoint file is removed. Bun 1.3's native `Bun.spawn({terminal})` is the
PTY; node-pty and `ROOST_KEEPER_MODE` are retired.

- `src/keeper/protocol.ts` is the entry point: it holds the frame diagram (`[4B BE total][1B type][2B BE
  channel_id][payload]`), the wire-version bump log, and `KEEPER_PROTOCOL_VERSION`, and re-exports three families —
  `src/keeper/protocol-envelope.ts` (envelope, spawn, scalar codecs), `src/keeper/protocol-io.ts` (hello handshake, typed `PtyIn`),
  `src/keeper/protocol-terminal.ts` (resize control, authoritative terminal
  state, ordered history). Protocol versions are fields rather than separate
  modules; a mismatch is reported rather than dispatched across.
- **Decode hazard, load-bearing.** `decodeMuxFrames()` returns each frame's `payload` as a `subarray` **view** onto
  the streaming receive buffer, valid only until the next read. Synchronous readers may use the view; anything
  outliving the read MUST copy. That is why the frame handler wraps input in `Buffer.from(...)` before queueing it
  for `proc.terminal.write` (`src/keeper/keeper-frame-handler.ts`), and why retained history chunks are copied out
  (`src/keeper/keeper-pool-lifecycle.ts`). Skipping the copy yields garbage PTY bytes under load.
- Keeper side: `src/keeper/multiplexed-main.ts` (entry, listener, endpoint
  watchdog), `src/keeper/keeper-frame-handler.ts` (frame dispatch, the real
  PTY spawn site), `src/keeper/keeper-history.ts`,
  `src/keeper/keeper-input-queue.ts`, `src/keeper/keeper-resize-result.ts`,
  `src/keeper/keeper-types.ts`, `src/keeper/keeper-log.ts`,
  `src/keeper/keeper-process-reap.ts`, and `src/keeper/histfile.ts`. Worker
  side: `src/keeper/multiplexed-client.ts` (the pool),
  `src/keeper/keeper-pool-lifecycle.ts`,
  `src/keeper/keeper-pool-channels.ts`, `src/keeper/keeper-pool-io.ts`, and
  `src/keeper/keeper-pool-config.ts`.

## Module map

- **Boot** — `src/main.ts`, `src/coord-link-deps.ts`, `src/boot-keeper.ts`,
  `src/boot-reconcile.ts`, `src/install.ts`, `src/service-definition-env.ts`,
  `src/config.ts`, `src/jwt.ts`.
  **`src/transport/`** — the outbound link, durable session-event store,
  schema migration, and replay barrier (above). **`src/keeper/`** — the PTY
  host (above).
- **Session family**, one owner split across `this`-bound modules:
  `src/session-manager.ts` (facade/delegating wrappers),
  `src/session-manager-state.ts` (channel-keyed maps + event sink),
  `src/session-record.ts`, `src/session-constants.ts`, `src/session-spawn.ts`,
  `src/session-resume.ts`, `src/session-respawn.ts`, `src/session-lifecycle.ts`,
  `src/session-emit.ts`, `src/session-resume-events.ts`,
  `src/session-sync-output.ts`, `src/session-snapshot-cursor.ts`,
  `src/session-terminal-control.ts`, `src/session-terminal-state.ts`,
  `src/session-terminal-txn.ts`, `src/session-resize-capture.ts`,
  `src/session-diag-snapshot.ts`, `src/session-raw-metadata.ts`,
  `src/session-control-lanes.ts`, `src/session-scrollback.ts`,
  `src/session-scrollback-ring.ts`, `src/session-unhandled-seq.ts`,
  `src/session-git-ports.ts`, `src/terminal-replay-align.ts`, plus
  `src/fsm.ts`.
- **Browser RPCs** — `src/browser-command-handler.ts` owns the exhaustive
  downstream switch. Implemented request families delegate to
  `src/browser-command-spawn.ts`, `src/browser-command-terminal.ts` (cell
  retrieval), `src/terminal-search.ts` (bounded single-session content-search
  paging), `src/terminal-search-batch.ts` (fair, deadline-shared worker fan-out),
  `src/terminal-search-matcher.ts`, `src/terminal-search-scheduling.ts`,
  `src/terminal-search-result.ts`, `src/terminal-search-cancellation.ts`,
  `src/browser-command-files.ts`, `src/browser-command-attachments.ts`, and
  `src/browser-command-diag.ts`, answering upstream as `rpc-ok` / `rpc-error`.
  Cross-worker transfer has no worker command or result frame in v0.5.0; the
  beta web item is informational. Attachment upload/download remains supported.
- **Agent observation, private reference capture, guarded input, and restore** —
  `src/agent-status/` owns volatile per-session state, the PID-attested local
  report protocol, typed integration assets, and reference admission gate;
  `src/agent-prompt-control.ts` owns the prompt-only status/process/foreground
  fence and `src/agent-prompt-submit.ts` the two acknowledged keeper writes it
  admits. `src/agent-conversation-restore.ts` owns the fixed,
  versioned OMP resume descriptor and post-respawn one-input policy. **`src/util/`** —
  `src/util/mono.ts` is the monotonic clock behind every terminal-control
  deadline; `src/util/path.ts` owns worker-native path handling.
- **Host + coord plumbing** — `src/heartbeat.ts` with
  `src/host-sample-darwin.ts`, `src/host-sample-linux.ts`,
  `src/host-sample-win32.ts`, and `src/host-sample-types.ts`;
  `src/coord-client.ts` (Connect client, boot calls only — events ride CoordLink); `src/event-sink.ts`;
  `src/snapshot.ts`.
- **Session metadata pushed to the SPA** — `src/git-branch.ts`, `src/pr-status.ts`, `src/listening-ports.ts`.
  **Files + attachments** — `src/file-rpcs.ts`, `src/attachment-upload.ts`, `src/attachment-reaper.ts` (1 h sweep,
  24 h TTL, 1 GB LRU). **Terminal byte analysis** — `src/terminal-stream-scan.ts` (alt-screen transitions),
  `src/terminal-query-reply.ts`, `src/shell-spec.ts`, `src/wterm-serialize.ts` (test utility), and
  `src/diag/byte-capture.ts` (last 256 KB of PTY output per session, for `diag-dump-bytecap`).

## Invariants

- **Durable session events precede local acknowledgement or mutation.**
  `src/event-sink.ts` persists `opened`/`closed`/`respawned` and private
  `agent_reference` events through `src/transport/session-event-store.ts`;
  `cwd`/`git`/`pr`/`ports` coalesce in memory, and snapshots belong to the
  CoordLink barrier. Lifecycle producers reserve outbox capacity before
  changing PTY/session state; reference reports return success only after the
  local SQLite append. A disconnect drops volatile metadata, but durable rows
  replay after process restart until the exact coordinator ACK deletes them.

- **`SessionManager`'s live maps are keyed by `channelId`, not `SessionId`** —
  `sessions`, `terminalStreams`, cell emission state and raw metadata queues all
  share that key. Reach a session by sid via `getBySessionId()`; an ad-hoc
  sid-keyed owner will diverge.
- **The worker holds the one authoritative grid.** History is served as
  immutable cell rows by `handleGetScrollbackCells`; `src/terminal-search.ts`
  traverses the same absolute row space newest-first through exclusive,
  row-bounded cursors. Regex queries use the linear-time RE2 syntax rather
  than JavaScript's backtracking engine. Searches are latest-wins per
  browser-tab/channel, and a newer global batch tombstones every session in
  its predecessor before scanning. Channel close aborts every active owner;
  bounded cancellation tombstones reject cancel-before-start request reordering.
  Both readers settle terminal control and epoch-fence cooperative work; the
  browser never reflows rows. `getScrollbackSince` remains retired.
- **The coordinator owns viewer membership and SCD.** The worker receives one
  `DTerminalStreamState` per channel with an already-aggregated geometry and
  never keeps per-viewer claims, freshness timers, or withdraw grace. A new
  stream ID gates deltas until its complete full cursor is installed and sent.
  Disable gates cell emission without changing the keeper/core geometry.
- **Live resize mutates the existing core at the keeper boundary.**
  `src/session-resize-capture.ts` holds ordered PTY output while the keeper
  answers ResizeAck/Reject, applies the synchronous core boundary, and recovers
  a lost ACK from keeper history. Ordinary viewer geometry reaches it through
  `src/session-terminal-txn.ts`; it is not adoption-only.
- **`Bun.spawn({terminal})` does not inject `TERM` into the child env** (node-pty did). A locally bootstrapped
  worker inherits `TERM` from its terminal and hides the bug; an SSH-deployed one sees `TERM=""`/`unknown` →
  backspace echoes wrong and ncurses dies with `cannot initialize terminal type`. The real spawn site in
  `src/keeper/keeper-frame-handler.ts` sets `TERM: "xterm-256color"` explicitly. Guard: lint rule `L11: keeper
  Bun.spawn env must set TERM explicitly (deployed-only ncurses $TERM=unknown)` in `scripts/lint-roost.ts`. A test
  that passes `TERM` in itself falsely covers this.
- **`agent-status` is volatile, process-observed metadata on a shell PTY.** No
  SQLite row, event-log variant, or `session.kind`. One registry construction
  owns one `status_epoch`; an uninterrupted agent-kind/PID incarnation owns one
  `occupant_id` across integration/screen source changes. PID never leaves
  worker-private detection and registry state. Identified frames are resent
  with their exact identity and revision after every CoordLink reopen; the
  in-memory status lane preserves required inactive→active ordering while
  coalescing unseen intermediate occupants. Session close drops the entry, and
  worker restart changes the epoch and re-derives every status.
- **A prompt is admitted against the live process, not a stale badge.**
  `src/agent-prompt-control.ts` forces a private process-proof refresh before
  terminal-input admission. Immediately before the write, while holding the
  keeper admission, it rechecks the live session/channel and deadline, current
  coordinator connection, integration source, exact epoch/occupant/revision,
  unchanged refreshed process proof, `idle|working` state, and that the pane's
  tty foreground job still belongs to the proved agent's own process subtree —
  an interactive child that took the foreground (pager, `$EDITOR`, `sudo`,
  nested shell) would otherwise receive the prompt. Every mismatch is a
  rejected pre-write result with zero keeper writes. Accepted text is encoded
  once through `@roost/shared/terminal-input` and written, then the submitting
  CR follows as its own write 300 ms later, so an agent that debounces
  bracketed-paste assembly cannot read the pair as an unsubmitted draft;
  `accepted` requires both acknowledgements and an ambiguous boundary is never
  retried. Prompt text and status messages never enter worker logs or durable
  storage.
- **Conversation restore is post-respawn and at-most-once.** Keeper adoption
  always precedes restore and writes no resume input when it succeeds. After
  failed adoption, the normal replacement shell and durable `respawned`
  admission precede the fixed `omp --resume=<reference>` descriptor.
  Integration data supplies only the opaque value, which the canonical POSIX
  quoting utility renders as exactly one argv element; the worker types that
  command plus one CR as a single acknowledged batch. A reference already
  claimed earlier in the same pass is skipped, and a claim is released again
  when its write is rejected before any keeper byte, so the next session
  holding that reference still resumes it. Accepted, rejected, and ambiguous
  outcomes never retry, re-enter respawn/tombstone handling, or clear the
  stored reference; a non-accepted outcome reporting written bytes is followed
  by exactly one worker-owned `0x03`, which discards a truncated `--resume=`
  line from the prompt without re-sending it or ending the session.
- **Keeper input correlation is worker-owned.** Browser-local `input_seq` and
  worker request IDs correlate their respective hops only. The keeper receives
  a monotonically increasing per-channel/connection key allocated by the
  worker, so simultaneous devices using the same local sequence cannot replace
  each other's pending result.

## Agent status

Labels each shell PTY `working` / `blocked` / `idle` for whichever coding
agent runs inside it. This is observed metadata: the worker owns no agent
process, conversation, transcript, tool call, or approval state. A guarded
prompt uses that observation only as a fence for input to the same ordinary
PTY. Status code lives under `src/agent-status/`; prompt admission lives in
`src/agent-prompt-control.ts`.

- `src/agent-status/process-scan.ts` — throttled (250 ms) `ps -A` snapshot
  finds a known agent plus its PID in the session process tree; identity
  survives one missed scan so a momentary miss cannot flap.
  `src/agent-status/process-tree.ts` owns the snapshot's process-tree facts:
  the subtree of a session's child PID and which process group holds the pane
  tty's foreground job, which is what the prompt fence reads.
  `src/agent-status/detector.ts` carries that verified private PID through
  screen stabilization into the registry, and when an agent leaves a still-live
  session it emits that session's one durable `agent_reference: null` clear
  through the same admission gate the integration reports use.
- `src/agent-status/report-server.ts` — authoritative reports on
  `$ROOST_AGENT_SOCKET_PATH` (default `~/.roost/agent-report.sock`, dir `0700`,
  socket `0600`): exactly one bounded JSON request per connection. Volatile
  `agent.report` supplies only session-authorized state. The separate
  `agent.reference` method accepts OMP `id|path` set/replace/clear values and
  returns success only after the durable local append. Both methods use the
  per-session capability, kernel-attest the accepted socket's peer PID, and
  require a fresh process-tree scan proving that exact process is the current
  agent under the claimed session. Reference values are never logged, placed
  in public session/snapshot state, or retried after an ambiguous response.
  `src/agent-status/environment.ts` injects the endpoint, capability, and
  `ROOST_SESSION_ID` into every spawned shell.
- `src/agent-status/integration-assets.ts` is the canonical typed asset list;
  `src/agent-status/install-integrations.ts` materializes the OMP status,
  OMP reference, and Pi status assets (mode `0600`, temp-file + rename,
  idempotent). It canonicalizes and preflights every destination before any
  write or owned retirement. OMP/Pi directory collisions and symlink aliases
  fail the whole installation with zero mutation; a symlink or unowned file at
  ONE target fails only that asset — the pass logs `integration_install_failed`
  for it and its report still lists what installed. Ownership is a `//` comment
  line carrying the asset's marker token at any depth in the file, because an
  installed asset splices the shared report transport above its own header.
  `ROOST_AGENT_STATUS_DISABLED=1` makes status reporting inert
  without disabling the separate OMP reference asset.
- `src/agent-status/registry.ts` — an integration report wins while its 30 s
  lease is fresh, else the screen fallback
  (`src/agent-status/manifests.ts` + `src/agent-status/manifest-engine.ts` +
  `src/agent-status/stable-detection.ts`, pinned from Herdr `eacea2da`,
  Apache-2.0). A plain `working → idle` needs repeated confirmation so a redraw
  cannot flicker a completion. Same-process changes preserve the occupant;
  replacement publishes the old occupant inactive before a fresh occupant
  whose completion revision starts at zero, and retired reporters cannot
  reclaim the session. An occupant whose last candidate exits leaves an
  `occupant_exited` row behind whenever it has a completion to carry, so a
  completion nobody has seen outlives the process that earned it; an explicit
  `active: false` withdrawal or a session close retires the row outright.

## Run, test, deploy

- **Run from source** — `bun apps/worker/src/main.ts`, or `bun --filter @roost/worker run dev` to watch.
- **Test: `bun run test:worker`.** That is `scripts/test-worker.ts`: it globs
  `apps/worker/tests/**/*.test.ts` and runs **each one in its own `bun test` child** with an isolated temp
  root (`TMPDIR` plus a fresh `ROOST_WORKER_DATA_DIR` inside it, every inherited `ROOST_*` var stripped), so each
  file gets its own keeper subprocess, keeper socket dir and sqlite. The default
  pool is 4 children; `ROOST_WORKER_TEST_JOBS` is clamped to
  `1..min(availableParallelism, 8)`. Per-test timeout is 30 s and the per-file
  hang backstop is 90 s.
  Every file runs even after one fails; the run exits with the first failure's code.
  `apps/worker/tests/session-event-store.test.ts` pins reopen/ACK durability,
  sequence-block reservation, capacity release, and fail-closed corruption.
- **Do not run `bun test apps/worker/tests/`.** That executes every file in **one** process sharing one temp root,
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
- **Conversation restore configuration** —
  `ROOST_AGENT_CONVERSATION_RESTORE` accepts exactly `0` or `1`. Absent and
  `0` are disabled everywhere; `1` is POSIX-only and fails on Windows. The
  service installer persists explicit values, and source deploys preserve an
  installed override. Default-on remains blocked pending actual official-OMP
  POSIX real-stack qualification.
- **Keeper retire authorization** — `ROOST_KEEPER_FORCE_LIVE_RETIRE` accepts
  exactly `0` or `1` and defaults off. `1` lets `handleKeeperSurvivor()` retire
  a survivor that authenticates but cannot prove its channel bindings — a keeper
  predating binding proof, which the worker can neither adopt nor prove empty —
  and that ends every PTY it hosts, named field by field before the shutdown
  request (a `null` channel list is the survivor's own failure to enumerate).
  A survivor that proves its bindings, and a process that proved no keeper
  identity, are both still refused under it. `roost deploy <host> --force-live`
  supplies it for one activation, which means the installer writes it into the
  service definition — otherwise it would reach only the installer process, not
  the worker launchd/systemd starts. It is one-shot on both sides: boot spends
  it out of that definition (`src/service-definition-env.ts`) before any keeper
  work, and a source deploy strips an installed value, so neither a restart nor
  a later deploy can re-arm it.
