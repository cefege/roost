<!-- AUDIENCE: human -->
# Glossary

One place to nail down every overloaded term. When in doubt, the cited file
wins.

- **machine** — a macOS or Linux machine in the cluster. The
  user-facing word; the **Machines** settings pane lists them. Maps 1:1 to a row
  in the `workers` table.

- **worker** — the `roost worker` process running on a machine. Owns the PTYs (via the
  keeper) and the per-session state machines, and maintains its outbound
  coordinator link. Its loopback UI door is a same-machine direct terminal
  carrier; after coordinator admission, it may create a bounded authenticated
  WebRTC UDP peer. It exposes no unauthenticated terminal endpoint or public
  worker HTTP terminal endpoint. Identified by the SHA-256 fingerprint of its
  ed25519 public key
  (`fp`).
  Source: `crates/roost-worker/src/runtime/boot.rs`,
  `crates/roost-worker/src/session/`, `crates/roost-worker/src/local_terminal/`.

- **coordinator** (coord) — the control-plane `roost coord` process, one per cluster. Auth,
  the event log, the `sessions` projection, and fan-out to browsers. Holds no
  PTY state of its own. Also serves the Dioxus web bundle as static files.
  Source: `crates/roost-coord/src/serve.rs`, `crates/roost-coord/src/http/spa.rs`.

- **session** — a shell PTY running in a folder on a machine. It is built by
  folding `SessionEvent`s, not stored as a mutable snapshot. Identified by a
  `SessionId`.
  Source: `crates/roost-worker/src/session/`; projected into the coordinator's
  `sessions` table (`crates/roost-coord/src/events/projection.rs`).

- **channel** — a single PTY inside the keeper. The keeper multiplexes every
  channel for a worker over one Unix-domain socket.
  Source: `crates/roost-keeper/src/pty_channel.rs`, `crates/roost-keeper/src/frames.rs`.

- **keeper** — the `roost-keeper` subprocess, one per worker, that hosts every
  PTY (via `portable-pty`) behind a single UDS and keeps a 1 MiB raw byte ring
  per channel. It survives the parent worker
  dying; the restarted worker reattaches and re-adopts open sessions. This is
  what makes sessions outlive worker restarts.
  Source: `crates/roost-keeper/src/server.rs`, `crates/roost-keeper/src/channel_history.rs`;
  worker side `crates/roost-worker/src/keeper_pool/`.

- **workspace** — a named container of sessions in the sidebar, backed by a
  folder. Sessions in a workspace inherit its working directory. Opening a
  workspace shows a tab bar of its sessions above one live terminal.
  Source: `crates/roost-web/src/components/sidebar/`,
  `crates/roost-coord/src/sessions/workspaces.rs`.

- **SessionEvent** — the unit of event sourcing: `opened`, `closed`, `attached`,
  `detached`, `cwd`, `workspace_assigned`, `snapshot`, `respawned`, `renamed`,
  `git`, `pr`, and `ports`. Workers emit them; the coordinator appends +
  projects them; the browser folds them.
  Source: `protocol/proto/roost/v1/events.proto`, `crates/roost-protocol/src/wire/event.rs`.

- **fold_event** — the pure reducer that applies a `SessionEvent` to state. The
  coordinator's projection and the browser's store both call it, so the two
  agree by construction.
  Source: `crates/roost-protocol/src/wire/event.rs`.

- **event log / projection** — the append-only `events` table is the source of
  truth; the `sessions` table is a projection of it, rebuilt by replaying the
  log. Append + project happen in one database transaction (SQLite or
  Postgres).
  Source: `crates/roost-coord/src/events/event_log.rs`,
  `crates/roost-coord/src/events/append_transaction.rs`.

- **Sync stream** — one long-lived protobuf WebSocket that multiplexes exactly
  eight generation domains: terminal, workers, workspaces, tasks, MCP, pair,
  audit, and agent. Audit is the only lazy domain. A missing or extra generation
  is a protocol mismatch, so a tab from an incompatible deployment must reload.
  Sync remains the authenticated metadata/control plane and terminal fallback
  even while an elected direct carrier transports a session's cells and input.
  Source: `protocol/proto/roost/v1/sync.proto`, `crates/roost-coord/src/sync_ws/`,
  `crates/roost-client-core/src/sync/`.

- **direct terminal transport** — the browser-to-worker carrier selected per
  session without changing the cell model: same-worker **loopback** first, then
  a qualified authenticated **WebRTC terminal peer**, then Sync. The browser
  keeps one document-scoped direct registry, stages a candidate full baseline,
  and elects it atomically; direct transport never carries raw PTY bytes.
  Source: `crates/roost-client-core/src/terminal/routes/`,
  `crates/roost-client-core/src/handle_sync/promotion/`.

- **loopback carrier** — an authenticated `LocalTerminal` WebSocket from a
  browser to the loopback UI door of the worker on the same machine. A
  coordinator-issued, worker-scoped grant and `LocalTerminalHello` authorize
  it. It has priority over WebRTC, needs no UDP/ICE, and can continue through a
  coordinator outage only while its existing grant and route remain valid.
  Source: `crates/roost-worker/src/local_door.rs`,
  `crates/roost-worker/src/local_terminal/hello.rs`.

- **WebRTC terminal peer** — one browser-to-worker encrypted DTLS/SCTP
  `RTCPeerConnection` carrying ordered control, terminal-cell, and history data
  channels. The browser offers; the worker creates its UDP endpoint only after
  coordinator admission of the authenticated device/tab/grant/worker-epoch
  tuple and verifies the expected direct hello. It is opportunistic: a failed
  or unavailable peer falls back to Sync. It is not a worker HTTP endpoint,
  TURN relay, or a promise that NAT traversal will succeed.
  Source: `crates/roost-web/src/platform/peer/`, `crates/roost-worker/src/peer/`,
  `crates/roost-coord/src/terminal_direct/`.

- **worker epoch** — a fresh worker-process identity, distinct from terminal
  grid epoch, terminal `domain_generation`, and coordinator connection
  generation. Direct grants, SDP answers, `LocalTerminalReady`, probes, and
  input routes carry it so a restarted worker cannot accept stale peer work.
  Source: `crates/roost-worker/src/runtime/downstream/local_grants.rs`,
  `crates/roost-coord/src/terminal_direct/grant_owner.rs`.

- **input route** — worker-acknowledged authority for one
  device/tab/connection/session writer, identified by a monotonically revised
  claim and an `input_route_epoch`. On a route-capable carrier handoff, the
  browser claims the new route before releasing unsent input; the worker checks
  it immediately before the keeper write. Late bytes from the old route are
  rejected, never replayed. Older loopback retains its established no-replay
  behavior without an unsupported claim. This is not a global PTY lock:
  worker-owned CLI and agent-prompt writers remain separate.
  Source: `crates/roost-worker/src/terminal_input/route_owner.rs`,
  `crates/roost-client-core/src/terminal/input/router/`.

- **STUN** — operator-configured UDP address discovery for WebRTC. The default
  is `stun:stun.cloudflare.com:3478`; an explicitly empty
  `ROOST_TERMINAL_PEER_STUN_URLS` disables external discovery. Roost accepts
  only bounded `stun:` UDP URLs, not TURN, relay credentials, or browser-supplied
  ICE-server configuration. STUN sees discovery traffic and address mapping,
  not terminal cells, input, or grants.
  Source: `crates/roost-protocol/src/terminal_peer/peer.rs`.

- **ICE candidate** — a bounded UDP endpoint candidate in the authenticated
  WebRTC offer/answer: host, server-reflexive (`srflx`), or peer-reflexive
  (`prflx`), never relay or ICE-TCP. An authenticated direct peer can learn
  the candidate's address/port metadata during connectivity checks; browser
  policy, NAT, or firewalls can still prevent a direct path.
  Source: `crates/roost-protocol/src/terminal_peer/sdp/candidate.rs`.

- **scrollback** — a session's history. Fresh and grid-incompatible full frames
  carry only the visible grid; a compatible same-grid renewal may carry a
  bounded recent tail. Older retained history is fetched separately, on
  explicit demand, by absolute row range.
  Source: `crates/roost-worker/src/scrollback_read.rs`,
  `crates/roost-worker/src/browser_commands/scrollback_page.rs`
  (`GetScrollbackCells`).

- **agent CLI** — an arbitrary terminal program, such as `omp`, Claude Code, or
  Codex, launched inside a normal shell PTY. Roost transports its terminal
  input and output but does not interpret its transcript, tools, or approval
  prompts. The built-in agent is a separate Roost-owned conversation type.

- **built-in agent** — the Rust coding-agent harness in the coordinator.
  `roost-agent` drives conversations and `roost-llm` handles provider access;
  tools execute on the selected worker through `agent_tools_v1` frames using
  `roost-agent-tools`.

- **agent runtime state** — what a coding agent inside a shell PTY is doing:
  `working`, `blocked` (waiting on the user), or `idle`. Volatile metadata on a
  terminal, never a stored session field. Absent = Roost sees no agent, which is
  what a plain shell shows.
  Source: `crates/roost-protocol/src/wire/agent_status.rs`.

- **lifecycle integration** — a small file Roost owns inside an agent's own
  extension directory (OMP, Pi) that reports that agent's state to the worker.
  Authoritative: it beats screen detection while its 30 s lease is fresh.
  Source: `crates/roost-worker/src/agents/install_integrations.rs`,
  `crates/roost-worker/src/agents/integration_assets.rs`.

- **agent report socket** — the worker's per-machine Unix socket
  (`ROOST_AGENT_SOCKET_PATH`, mode `0600`) that integrations write one JSON line
  to. The worker maps the reporting pid to the session that owns it, so a report
  cannot claim another terminal.
  Source: `crates/roost-worker/src/agents/report_server.rs`.

- **screen fallback** — detection for terminals with no integration: the worker
  matches the session's own screen text and OSC title/progress against pinned
  per-agent manifests. Used for other agents and for terminals that predate an
  integration install.
  Source: `crates/roost-worker/src/agents/manifests.rs`,
  `crates/roost-worker/src/agents/stable_detection.rs`.

- **effective state / revision** — the one state the worker publishes per
  session after arbitrating integration over screen, stamped with a monotonic
  revision. Coordinator and browser drop anything at or below the revision they
  already hold, so a late frame can't resurrect stale state.
  Source: `crates/roost-worker/src/agents/registry.rs`.

- **done (derived)** — an `idle` agent whose completion revision the user has
  not acknowledged yet. Purely a browser-side presentation level: viewing the
  session acknowledges it and `done` decays to plain `idle`.
  Source: `crates/roost-client-core/src/client/agents/status_policy.rs`,
  `crates/roost-client-core/src/client/agents/seen.rs`.

- **notification suppression** — the rules that stop duplicate or unwanted
  in-app alerts: viewing a session cancels its pending notification and
  acknowledges the revision; one browser profile delivers one notification
  even with many tabs open (a storage/Web-Locks claim). Web Push skips a
  device that is already viewing the session.
  Source: `crates/roost-web/src/components/notifications/agent_notifications/`,
  `crates/roost-coord/src/push/dispatch.rs`, `crates/roost-coord/src/push/viewers.rs`.

- **cell-shipping / authoritative grid** — the terminal-fidelity model: the
  worker holds the one canonical grid for a session and the browser renders it
  without ever re-reflowing to its own width. The fix for the corruption that
  byte-streamed terminals accumulate across resizes and reconnects.
  Source: `crates/roost-term/` (grid → `CellGridFrame`),
  `crates/roost-protocol/src/cell/`, `crates/roost-web-terminal/`.

- **auth (EdDSA-JWT)** — the browser mints an ed25519 JWT with a device key
  held in IndexedDB and stamps every RPC with it; the coordinator verifies it in
  an interceptor. There are no shared passwords or copied tokens for normal use.
  Source: `crates/roost-web/src/platform/device_key.rs`,
  `crates/roost-coord/src/auth/jwt_verify.rs`.

- **front door** — whatever the operator puts in front of the coordinator's
  plaintext loopback listener: Caddy, nginx, a Cloudflare tunnel,
  `tailscale serve`. It owns TLS, DNS, and public reachability; Roost owns none
  of them and is simply told the resulting origin through
  `ROOST_WEB_PUBLIC_URL`. That origin seeds the SPA's CSP `connect-src` and the
  Sync WebSocket origin allowlist.
  Source: `crates/roost-coord/src/middleware/security.rs`,
  `crates/roost-coord/src/sync_ws/upgrade_admission.rs`.

- **tailnet** — your [Tailscale](https://tailscale.com) network, when you run
  one. Roost may resolve its own MagicDNS name to publish a worker's
  `reachable_addr`, but that address is transport metadata, never authority.
  An existing tailnet interface may yield a usable ICE host candidate only if
  normal browser/worker ICE policy exposes it and the route works. Roost does
  not install, configure, manage, or require Tailscale, and does not promise
  tailnet direct-terminal reachability.
  Source: `crates/roost-worker/src/host/tailnet.rs`.
