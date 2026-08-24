<!-- AUDIENCE: human -->
# Glossary

One place to nail down every overloaded term. When in doubt, the cited file
wins.

- **machine** — a machine (macOS, Linux, or Windows) in the cluster. The
  user-facing word; the **Machines** settings pane lists them. Maps 1:1 to a row
  in the `workers` table.

- **worker** — the Bun process running on a machine. Owns the PTYs (via the
  keeper) and the per-session state machines. Purely outbound: it dials the
  coordinator and exposes no inbound port. Identified by the SHA-256
  fingerprint of its ed25519 public key (`fp`).
  Source: `apps/worker/src/main.ts`, `apps/worker/src/session-manager.ts`.

- **coordinator** (coord) — the control-plane Bun process, one per cluster. Auth,
  the event log, the `sessions` projection, and fan-out to browsers. Holds no
  PTY state of its own. Also serves the web SPA as static files.
  Source: `apps/coord/src/main.ts`, `apps/coord/src/coord-factory.ts`.

- **session** — a shell PTY running in a folder on a machine. It is built by
  folding `SessionEvent`s, not stored as a mutable snapshot. Identified by a
  `SessionId`.
  Source: `apps/worker/src/session-manager.ts` (`SessionRecord`); projected into
  the coordinator's `sessions` table.

- **channel** — a single PTY inside the keeper. The keeper multiplexes every
  channel for a worker over one Unix-domain socket.
  Source: `apps/worker/src/keeper/protocol.ts`.

- **keeper** — one subprocess per worker that hosts every PTY (via Bun's native
  `terminal:` spawn option) over a single UDS. It survives the parent worker
  dying; the restarted worker reattaches and re-adopts open sessions. This is
  what makes sessions outlive worker restarts.
  Source: `apps/worker/src/keeper/multiplexed-main.ts`.

- **workspace** — a named container of sessions in the sidebar, backed by a
  folder. Sessions in a workspace inherit its working directory. Opening a
  workspace shows a tab bar of its sessions above one live terminal.
  Source: `apps/web/src/components/sidebar/`, coordinator `workspaces` table.

- **SessionEvent** — the unit of event sourcing: `opened`, `closed`, `attached`,
  `detached`, `cwd`, `workspace_assigned`, `snapshot`, `respawned`, `renamed`,
  `git`, `pr`, and `ports`. Workers emit them; the coordinator appends +
  projects them; the browser folds them.
  Source: `apps/shared/src/wire/event.ts`.

- **foldEvent** — the pure reducer that applies a `SessionEvent` to state. The
  coordinator's projection and the browser's store both call it, so the two
  agree by construction.
  Source: `apps/shared/src/wire/event.ts`.

- **event log / projection** — the append-only `events` table is the source of
  truth; the `sessions` table is a projection of it, rebuilt by replaying the
  log. Append + project happen in one SQLite transaction.
  Source: `apps/coord/src/event-log.ts`.

- **Sync stream** — one long-lived server-streaming RPC that multiplexes live
  deltas for every domain (sessions, presence, workspaces, tasks, permissions,
  MCP, webhook tokens, audit) plus PTY bytes. On reconnect, the browser sends
  the last event id it saw and the coordinator backfills the gap.
  Source: `apps/coord/src/connect/handlers-streaming.ts`,
  `apps/web/src/store/sync.ts`.

- **scrollback** — a session's history. The live cell frame carries only the
  visible grid; retained history is fetched separately, on explicit demand, by
  absolute row range.
  Source: `apps/worker/src/browser-command-terminal.ts`
  (`handleGetScrollbackCells`).

- **agent CLI** — an arbitrary terminal program, such as `omp`, Claude Code, or
  Codex, launched inside a normal shell PTY. Roost transports its terminal
  input and output but does not interpret its lifecycle, transcript, tools, or
  approval prompts. There is no structured agent session type or agent API.

- **agent runtime state** — what a coding agent inside a shell PTY is doing:
  `working`, `blocked` (waiting on the user), or `idle`. Volatile metadata on a
  terminal, never a stored session field. Absent = Roost sees no agent, which is
  what a plain shell shows.
  Source: `apps/shared/src/wire/agent-status.ts`.

- **lifecycle integration** — a small file Roost owns inside an agent's own
  extension directory (OMP, Pi) that reports that agent's state to the worker.
  Authoritative: it beats screen detection while its 30 s lease is fresh.
  Source: `apps/worker/src/agent-status/integrations/`.

- **agent report socket** — the worker's per-machine Unix socket
  (`ROOST_AGENT_SOCKET_PATH`, mode `0600`) that integrations write one JSON line
  to. The worker maps the reporting pid to the session that owns it, so a report
  cannot claim another terminal.
  Source: `apps/worker/src/agent-status/report-server.ts`.

- **screen fallback** — detection for terminals with no integration: the worker
  matches the session's own screen text and OSC title/progress against pinned
  per-agent manifests. Used for other agents and for terminals that predate an
  integration install.
  Source: `apps/worker/src/agent-status/{manifests,stable-detection}.ts`.

- **effective state / revision** — the one state the worker publishes per
  session after arbitrating integration over screen, stamped with a monotonic
  revision. Coordinator and browser drop anything at or below the revision they
  already hold, so a late frame can't resurrect stale state.
  Source: `apps/worker/src/agent-status/registry.ts`.

- **done (derived)** — an `idle` agent whose completion revision the user has
  not acknowledged yet. Purely a browser-side presentation level: viewing the
  session acknowledges it and `done` decays to plain `idle`.
  Source: `apps/web/src/lib/{agentStatus,agentSeen}.ts`.

- **notification suppression** — the three rules that stop duplicate or unwanted
  alerts: viewing a session cancels its pending notification and acknowledges
  the revision; one browser profile delivers one notification even with many
  tabs open (a storage/Web-Locks claim); and the coordinator skips Web Push to a
  device that is already viewing the transitioning session.
  Source: `apps/web/src/components/AgentNotificationBridge.tsx`,
  `apps/coord/src/push-dispatch.ts`.

- **cell-shipping / authoritative grid** — the terminal-fidelity model: the
  worker holds the one canonical grid for a session and the browser renders it
  without ever re-reflowing to its own width. The fix for the corruption that
  byte-streamed terminals accumulate across resizes and reconnects.

- **auth (EdDSA-JWT)** — the browser mints an ed25519 JWT in WebCrypto (private
  key in IndexedDB) and stamps every RPC with it; the coordinator verifies it in
  an interceptor. There are no shared passwords or copied tokens for normal use.
  Source: `apps/web/src/auth/web-key.ts`, `apps/coord/src/jwt.ts`.

- **tailnet** — your [Tailscale](https://tailscale.com) network. Every
  connection (browser↔coordinator, worker↔coordinator) runs over it.
