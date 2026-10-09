<!-- AUDIENCE: human -->
# Architecture

A tour of how Roost fits together. For install and run, see
[`GETTING_STARTED.md`](GETTING_STARTED.md); for terms,
[`GLOSSARY.md`](GLOSSARY.md); for the crate map and the enforced dependency
DAG, [`crates/README.md`](crates/README.md); for the wire contract,
[`protocol/README.md`](protocol/README.md) and `protocol/spec/`. Operating
rules for LLM collaborators are in [`CLAUDE.md`](CLAUDE.md). Paths are
repo-root-relative.

Roost is Rust end to end: two shipped binaries (`roost`, `roost-keeper`), a
Dioxus web client compiled to wasm, and one shared protocol layer. The `roost`
binary runs the coordinator (`roost coord`) and the worker (`roost worker`)
as services; `roost-keeper` is spawned by the worker and outlives it.

```
  Browser (Dioxus/wasm client on any device that reaches the front door)
       │
       │  unary Connect RPC + protobuf Sync WebSocket (HTTPS via front door)
       ▼
  Front door (Caddy, nginx, a tunnel, tailscale serve — not Roost's code)
       │  plaintext HTTP to ROOST_COORDINATOR_BIND (default 127.0.0.1:4113)
       ▼
  Coordinator (`roost coord`, crate roost-coord) ── SQLite or Postgres
       │
       │  protobuf over a raw WebSocket (worker link, dialed by the worker)
       │
       ├──────────────────────┬──────────────────────┐
       ▼                      ▼                      ▼
  Worker                 Worker                 Worker
  (`roost worker`)       macOS/Linux            macOS/Linux
       │                      │                      │
       │ Unix socket          │ Unix socket          │ Unix socket
       ▼                      ▼                      ▼
  roost-keeper           roost-keeper           roost-keeper
       │                      │                      │
       └─ PTY per channel     └─ PTY per channel     └─ PTY per channel
```

This diagram shows the always-present control plane. A selected terminal may
instead carry cell frames and input directly between browser and worker:
same-worker loopback first, then an authenticated WebRTC UDP peer; Sync stays
connected for metadata, authorization/control, and fallback.

The coordinator owns one plaintext HTTP listener. TLS, DNS, and public
reachability belong to whatever the operator puts in front of it, and
`ROOST_WEB_PUBLIC_URL` is how the coordinator learns the resulting origin. The
same listener serves the web bundle (`ROOST_WEB_DIST_PATH`), Connect RPC, both
WebSockets, `/healthz`, `/readyz`, and `/api/db-export`
(`crates/roost-coord/src/http/listener.rs`). The coordinator also ships as the
container image built from `Dockerfile` and the Helm chart in
`deploy/helm/roost-coordinator/`; `ROOST_COORDINATOR_DATABASE_URL` set to a
`postgres://` URL replaces the SQLite file
(`crates/roost-host/src/database_location.rs`).

## Components

| Component | Crate(s) | Owns |
|---|---|---|
| Browser client | `roost-web`, `roost-web-terminal`, `roost-client-core` | Routes, components, pane layouts, terminal painting, input. Holds Sync for metadata/control/fallback and paints typed cell fulls/deltas from the elected carrier; raw PTY bytes never reach the browser. |
| Coordinator | `roost-coord` | Durable event log and `sessions` projection, auth and pairing, worker presence, Sync fan-out and per-session screen replicas, terminal view hub, direct-terminal grants and bounded WebRTC signaling, server-side web push (no browser subscribes yet), audit. Routes terminal commands; never owns a PTY or a terminal parser (its DAG has no `roost-term` edge). |
| Worker | `roost-worker` | One per host. Keeper client, per-session terminal core, durable lifecycle outbox, the outbound coordinator link, the loopback door (`127.0.0.1:4114`), the WebRTC peer (str0m), agent status detection. |
| Keeper | `roost-keeper` | One process per worker: owns every PTY, a 1 MiB raw-output ring per channel (`KEEPER_RING_CAP_BYTES`), and the framed Unix-socket protocol in `protocol/spec/keeper.md`. Survives worker restarts and coordinator deploys. The only crate permitted `unsafe`. |
| Terminal core | `roost-term` (+ `third_party/rio_vt`) | `TerminalCore` trait, its rio-vt implementation, and the grid→`CellGridFrame` emitter. Linked only by the worker. |
| CLI | `roost-cli` (binary `roost`) | Service entry points (`coord`, `worker`, `keeper`), `quickstart`, `join`, `add-machine`, `add-browser`, `status`, `doctor`, `logs`, `update`, `deploy`, `push` (journaled fleet rollout), `api` (28 headless verbs incl. `agent-*`, `ui`, `tasks`), `skill` (prints the embedded agent skill), `db-to-postgres`, `db-to-sqlite`, `import-v2`. |
| Protocol | `roost-proto`, `roost-protocol` | `roost-proto`: protobuf messages and Connect stubs generated from `protocol/proto/roost/v1/` (the only protobuf runtime). `roost-protocol`: I/O-free wire logic — the one event fold, cell model, chunk assembly, viewport geometry, view registry, peer packet framing, layout documents. Builds for wasm32 and native. |
| Host/platform | `roost-host`, `roost-platform`, `roost-observability` | `ROOST_*` config and service names; path/shell conventions; the JSON log line `roost status`/`roost doctor` parse. |

`roost-client-core` is UI-free: no DOM type, no async runtime I/O, no timer.
`roost-web` calls `ClientCore::handle` from exactly one place
(`crates/roost-web/src/pump.rs`) and performs the effects it returns through
`crates/roost-web/src/platform/`. `roost-web-terminal` is the imperative
`web-sys` renderer with no framework code. A future native front end depends
only on `roost-client-core` and `roost-protocol`.

Windows is a browser client only; no Windows host binaries are published.

## Transport spine

| Surface | Path / label | Peers | Server owner | Spec |
|---|---|---|---|---|
| Connect RPC | `POST /roost.v1.CoordinatorService/<Method>` | browser, CLI → coordinator | `crates/roost-coord/src/rpc/` (one `CoordinatorService` impl in `service_impl.rs`; route/auth table in `method_route_rows.rs`) | `protocol/spec/coordinator-rpc.md` |
| Sync WS | `/ws/coord-sync?tab=…&since=…&flow=1&sync_v=2` | browser ↔ coordinator | `crates/roost-coord/src/sync_ws/` | `protocol/spec/sync.md` |
| Worker link WS | `/ws/coord-worker/<64-hex fingerprint>` | worker → coordinator | `crates/roost-coord/src/worker_link/`; dial in `crates/roost-worker/src/link_dial.rs` | `protocol/spec/worker-link.md` |
| Loopback door | `127.0.0.1:4114`: `/api/local-bootstrap`, `/ws/local-terminal`, `/ws/local-attachment-transfer` | same-machine browser ↔ worker | `crates/roost-worker/src/door/`, `crates/roost-worker/src/local_terminal/` | `protocol/spec/direct-terminal.md`, `protocol/spec/attachments.md` |
| Terminal WebRTC | data channels `roost-terminal-control-v1`, `-data-v1`, `-history-v1` | browser ↔ worker (DTLS/SCTP over UDP) | `crates/roost-worker/src/peer/`; signaling in `crates/roost-coord/src/terminal_direct/` | `protocol/spec/direct-terminal.md` |
| Keeper socket | Unix-domain socket, length-prefixed frames, channel id per frame | worker ↔ keeper | `crates/roost-keeper/src/server.rs`, `crates/roost-worker/src/keeper_pool/` | `protocol/spec/keeper.md` |

Sync multiplexes exactly seven generation domains — terminal, workers,
workspaces, tasks, MCP, pair, audit (`SyncDomain::ALL` in
`crates/roost-client-core/src/sync/link.rs`, checked against
`protocol/proto/roost/v1/sync.proto`). Delivery is ACK-windowed per socket
(`crates/roost-coord/src/sync_ws/ack_window.rs`).

Browser auth is a non-extractable WebCrypto Ed25519 device key in IndexedDB
signing short-lived EdDSA JWTs (`crates/roost-web/src/platform/device_key.rs`,
`crates/roost-client-core/src/client/auth.rs`); the coordinator resolves each
verified key to a principal — `AccountDevice`, `Worker`, or
`LegacySelfHosted` (`crates/roost-coord/src/auth/principal.rs`). Workers are
identified by the SHA-256 fingerprint of their Ed25519 public key.

**Direct carriers.** The browser elects per session: loopback, then WebRTC,
then the coordinator (Sync) fallback; the pane's transport indicator names the
baseline-qualified carrier as `Loopback`, `WebRTC`, or `Coordinator`
(`crates/roost-web/src/components/terminal/terminal_transport_indicator.rs`).
Election and staged-candidate promotion live in
`crates/roost-client-core/src/terminal/routes/registry/`; attempts, grants,
and signaling in `crates/roost-client-core/src/client/carriers/`; the browser
side of loopback and WebRTC in `crates/roost-web/src/platform/loopback.rs` and
`crates/roost-web/src/platform/peer/`. Grants and signaling bind the exact
device, tab, worker connection, and worker epoch (a fresh per-process
identity); the worker rechecks all of them
(`crates/roost-worker/src/local_terminal/grants.rs`). STUN is discovery only
(default `stun:stun.cloudflare.com:3478`, override or disable with
`ROOST_TERMINAL_PEER_STUN_URLS`); Roost supplies no TURN relay and promises no
direct path. Normative rules: `protocol/spec/direct-terminal.md`.

## Session events and the single fold

Durable session state is an ordered, ACK-paced event log reconciled by a
sequenced worker snapshot before live traffic. Normative spec:
`protocol/spec/session-events.md`.

1. **Worker outbox.** Worker-authored `opened`, `closed`, and `respawned`
   (plus private conversation-reference updates) enter a bounded SQLite store
   opened `synchronous=FULL` (`crates/roost-worker/src/event_store.rs`,
   `crates/roost-worker/src/event_store/`). The link outbox
   (`crates/roost-worker/src/outbox.rs`) guarantees a session's `opened`
   reaches the coordinator before its first terminal frame.
2. **Link barrier.** `idle → open → hello → replay → snapshot → live`
   (`crates/roost-worker/src/link_barrier.rs`): durable events replay one at a
   time, then a full session snapshot, then live traffic. An event leaves the
   store only on the exact post-commit coordinator ACK.
3. **Coordinator append.** Insert with `ON CONFLICT (worker_fp, client_seq) DO
   NOTHING`, update the `sessions` projection in the same transaction, publish
   strictly after commit (`crates/roost-coord/src/events/`). The
   announced-channel barrier holds a channel's first terminal frames behind the
   durable `opened`/`respawned` that makes it routable
   (`crates/roost-coord/src/worker_link/announced_barrier.rs`).
4. **One fold.** `fold_event` / `fold_all` in
   `crates/roost-protocol/src/wire/event.rs` is the only session reducer. The
   coordinator projection (`crates/roost-coord/src/events/projection_writes.rs`)
   and the client store (`crates/roost-client-core/src/sessions.rs`) both call
   it, so they agree by construction; `protocol/conformance/` vectors pin it.
5. **Browser.** Sync backfills from a fixed event cutoff, then a bounded live
   tail (`crates/roost-coord/src/sync_ws/live_feed.rs`); cold start or an
   unprovable cursor uses guarded current-state hydration
   (`crates/roost-client-core/src/sync/hydration.rs`).

Worker boot order is fixed: identity → keeper admission → coordinator link →
session reconcile → ready (`crates/roost-worker/src/runtime/boot_order.rs`).
Keeper admission is three-valued — adopt, replace, or refuse when a survivor's
channel set cannot be proven (`crates/roost-worker/src/boot_keeper.rs`).

## The terminal data plane

Normative detail lives in `protocol/spec/terminal-stream.md` and
`protocol/spec/direct-terminal.md`; this section is the map.

- **Three replicas.** The worker's terminal core is authoritative. The
  coordinator keeps one screen replica per watched session
  (`crates/roost-coord/src/terminal_screen/replica.rs`), bounded by a residency
  budget; each browser keeps one per session in `roost-client-core`, which
  survives renderer detach, direct promotion, and Sync reconnect. Direct
  candidates fold separately until promoted.
- **One membership and geometry authority per session.** A worker advertising
  view ownership owns membership and geometry for its own sessions
  (`crates/roost-worker/src/terminal_view/`); the coordinator then only relays
  and holds a read model (`crates/roost-coord/src/terminal_view/relay.rs`,
  `owner.rs`). Otherwise the coordinator's view hub owns it. Both run the one
  registry in `crates/roost-protocol/src/terminal_view/` and the one per-axis
  minimum, `minimum_terminal_geometry` in
  `crates/roost-protocol/src/viewport.rs`, with bounded leases and park grace.
- **Full before delta.** Each stream generation needs one complete full; only
  an exact stream/epoch/geometry/sequence successor extends a replica
  (`crates/roost-client-core/src/terminal/frame_fold.rs`). Any refusal latches
  one repair per gap (`crates/roost-client-core/src/terminal/repair.rs`).
  Oversized fulls travel as contiguous whole-row chunks under one snapshot
  identity (`crates/roost-protocol/src/cell/frame_chunks.rs`).
- **Liveness.** Visible panes challenge a quiet stream and escalate missed
  proof through resync (`crates/roost-client-core/src/terminal/liveness.rs`,
  `idle_probe.rs`). Exact direct-route loss falls back through a fresh Sync
  baseline without input replay.
- **Resize at the keeper's ordered boundary.** The acknowledged keeper resize
  is the parse boundary; the core resizes there and forces a new full.
- **Proven input outcomes.** Keeper writes are accepted, rejected, or
  ambiguous (`PtyInRequest`/`Ack`/`Reject`/`Ambiguous` in
  `protocol/spec/keeper.md`); only a proven pre-write rejection is retry-safe.
  A carrier handoff claims a worker-acknowledged input route before releasing
  unsent input, and the worker rejects late bytes from the old route
  (`crates/roost-worker/src/terminal_input/route_owner.rs`).
- **Live vs reading.** `CellGridRenderer` carries an explicit `ReaderIntent`
  (`crates/roost-web-terminal/src/reader_intent.rs`) plus holds for selection
  and an armed link. Passive output never cancels a reader; one admitted local
  keystroke calls `prepare_live_interaction` and returns to the live tail.
- **The application decides mouse and focus forwarding.** Frames carry the
  core's `mouse_tracking`, `mouse_sgr`, `focus_events`, `cursor_keys_app`, and
  `bracketed_paste`. The browser forwards only what the application requested,
  as SGR-1006 or legacy X10 (`crates/roost-web-terminal/src/mouse_forward/`).
  Alternate-screen occupancy alone never captures the mouse.

Every session is a shell PTY. Agent CLIs (`omp`, Claude Code, Codex) run inside
it. Roost never spawns, supervises, or owns an agent process, conversation,
transcript, tool call, or approval model.

## Terminal fidelity

Streaming raw bytes to a browser terminal corrupts in practice: the browser
re-parses the stream at its own width, and **re-parse at a new width is the
corruption**. A terminal core's row resize is asymmetric and lossy — shrinking
pushes rows into scrollback, growing fills with blanks, neither reverses — and
reconnects duplicate or drop output. Roost therefore ships cells, the model
server-side multiplexers use:

- **The worker holds the one authoritative grid** per session in a `roost-term`
  core (`crates/roost-term/src/core.rs`, `rio/`), fed by keeper bytes,
  rebuilt at a single agreed width on resize.
- **The browser paints that grid as-is.** It parses no VT and never reflows
  (`crates/roost-web-terminal/src/cell_renderer.rs`). Rows are pinned to the
  grid's `cols` in `ch` units, so a wider pane letterboxes instead of
  stretching (`.cell-grid` in `crates/roost-web/assets/styles/sidebar.css`).
  The accepted tradeoff: plain shell history does not rewrap to a narrower
  device; it scrolls sideways.
- **The agreed width is the SCD** (smallest common denominator) across views
  actually looking, computed by `minimum_terminal_geometry`, so no present
  viewer is clipped.
- **Alt-screen owns the viewport and carries no scrollback.** The frame states
  it (`alt_screen`); the renderer hides the history sheet and locks scrolling
  while it is set.
- **One cell model**, `crates/roost-protocol/src/cell/types.rs`: `CellSpan` (a
  style run with explicit `columns`, so a double-width glyph is one two-column
  span and no phantom continuation cell exists; OSC 8 `link_uri` is the only
  hyperlink source), `CellRow` (absolute index plus spans), and
  `CellGridFrame` (`stream_id`, `grid_epoch`, geometry, cursor, modes, `full`,
  `viewport_rows`, `scrollback_append`, `scrollback_total`, `sb_base`,
  `base_seq`, `seq`). `protocol/proto/roost/v1/cell.proto` mirrors it on every
  carrier.
- **Full or delta is the emitter's one decision**
  (`crates/roost-term/src/emitter.rs`): first frame, explicit force, or a
  semantic reframe (shape change, alt-screen toggle, total going backwards,
  ring eviction past what the client holds) forces a full. Authoritative fulls
  are viewport-only (`base_seq = 0`).
- **The scrollback origin is authoritative, never inferred.**
  `TerminalCore::scrollback_origin` uses the core's `discarded_line_count()`
  and errors if a core cannot supply it; rio-vt's `lines_evicted` supplies it
  (`third_party/rio_vt/ROOST-PATCHES.md`), so absolute history indices never
  re-alias.
- **Retained history is demand-paged** only on explicit scroll or find
  (`crates/roost-web-terminal/src/backfill.rs`; `SessionsGetScrollbackCells`
  on Sync, the history lane on WebRTC). A cold attach lands at the bottom
  having fetched none of it; each page is fenced to the grid epoch it was asked
  under, and a spacer stands in for unpainted history so absolute rows keep a
  fixed pixel offset.
- **Per-byte sequence lives one layer lower**, in the keeper's per-channel
  ring, so a restarted worker can re-adopt a live PTY and re-prove its core
  from the retained history (`crates/roost-keeper/src/channel_history.rs`).

The recurring failure modes of this plane and their fixes are indexed in
[`docs/FAILURE-INDEX.md`](docs/FAILURE-INDEX.md).

## Agent metadata

- **Agent status** — worker-observed `idle|working|blocked`, volatile,
  revision-fenced, never a stored session field. Integrations (OMP, Pi) report
  over the worker's local socket (`crates/roost-worker/src/agents/report_server.rs`);
  screen manifests cover the rest; `crates/roost-worker/src/agents/registry.rs`
  arbitrates and publishes. Shape: `crates/roost-protocol/src/wire/agent_status.rs`.
- **Fenced prompts** — `SessionsPrompt` writes one bounded text input to the
  same ordinary PTY only while the exact occupant/revision/state still match.

Normative: `protocol/spec/agent-metadata.md`.

## Tenant isolation

Coordinator boot creates and validates the single local
account/organization/dashboard inside one transaction before any listener
opens, and refuses to start otherwise
(`crates/roost-coord/src/auth/self_hosted_tenant.rs`). Authorization is per
credential: a browser device reaches the whole install, a worker JWT only its
own resources, and revocation closes every socket for that fingerprint.

## Pane layouts

Pane trees persist per browser profile under `roost.paneLayout.v1`
(`crates/roost-client-core/src/store/layout/record.rs`). Export, import, tab
state reports, and the acknowledged `UiApplyLayout` RPC share one bounded
`LayoutDocumentV1` parser (`crates/roost-protocol/src/layout/`); runtime IDs
never cross it. Apply targets one exact tab socket and correlation
(`crates/roost-coord/src/ui_state/layout_apply/`); `target_gone` means the
acknowledgement is unavailable, not that nothing ran.

## Resilience model

- **Browser drops:** Sync resumes from the last folded event id, then visible
  panes' liveness challenges repair any stalled stream. A hidden document
  starts and renews no peer pre-warm
  (`crates/roost-client-core/src/handle_terminal/prewarm.rs`).
- **Direct carrier drops:** the browser retires only that exact route, fences
  its input generation, and returns to Sync through a fresh full baseline. It
  never replays an input whose prior write was accepted or ambiguous.
- **Worker drops:** keepers preserve PTYs. Direct routes to that worker close;
  reconnect runs the link barrier — durable replay, snapshot reconciliation,
  then live.
- **Worker deletion:** the coordinator retires the worker's direct-terminal
  and attachment grants, fences its link generation, and retires its volatile
  terminal routes (`crates/roost-coord/src/workers/rpc.rs`).
- **Coordinator drops:** workers keep PTYs and their durable outboxes; browsers
  redial while visible. An established direct route may continue only while its
  grant and route liveness hold; no new grant, renewal, or negotiation
  completes. On restart the coordinator reopens its transactional log and
  projection, and reconnecting worker snapshots repair drift.
- **Worker process crashes:** keepers survive. The restarted worker adopts them
  and emits `respawned` only when a terminal was actually replaced; its new
  worker epoch invalidates old direct grants.

The goal is not "nothing ever disconnects". It is "a disconnect cannot silently
lose a PTY lifecycle edge, and every visible terminal either proves progress or
triggers bounded recovery."

## Key entry points

Paths cited above are not repeated here.

- **Coordinator:** `crates/roost-coord/src/serve.rs` (`serve`, run by
  `roost coord`); `sync_ws/socket.rs`; `worker_link/connection.rs`;
  `db/backend.rs`. Contract with sources: `docs/phase3-coord-contract.md`.
- **Worker:** `crates/roost-worker/src/runtime/mod.rs` (`serve`, run by
  `roost worker`); `session/`; `keeper_pool/`.
- **Keeper:** `crates/roost-keeper/src/server.rs` (socket), `keeper.rs`
  (protocol), `pty_channel.rs`.
- **Client core:** `crates/roost-client-core/src/core.rs`. Contract:
  `docs/phase4-client-contract.md`.
- **Web:** `crates/roost-web/src/main.rs` (scrubs URL-carried credentials and
  probes the serving origin before mounting); `app.rs`; `routes.rs`;
  `components/terminal/pane_mount.rs`; `components/md/` (design primitives).
- **CLI:** `crates/roost-cli/src/lib.rs` (command tree); `quickstart/`;
  `status/`; `doctor/`; `push/`; `update/`. Contract:
  `docs/phase6-cli-contract.md`.
