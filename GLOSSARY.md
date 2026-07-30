<!-- AUDIENCE: human -->
# Glossary

One place to nail down every overloaded term. When in doubt, the cited file
wins.

- **machine** — a Mac in the cluster. The user-facing word; the **Machines**
  settings pane lists them. Maps 1:1 to a row in the `workers` table.

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
  Source: `apps/worker/src/keeper/protocol-v2.ts`.

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
  Source: coordinator `connect/handlers-streaming.ts`,
  `apps/web/src/store/sync.ts`.

- **scrollback** — a session's history. Served on first open and after a gap;
  **resumable** because every byte carries a sequence number, so a client can
  ask for "everything since seq N".
  Source: `apps/worker/src/session-manager.ts` (`getScrollbackSince`).

- **agent CLI** — an arbitrary terminal program, such as `omp`, Claude Code, or
  Codex, launched inside a normal shell PTY. Roost transports its terminal
  input and output but does not interpret its lifecycle, transcript, tools, or
  approval prompts. There is no structured agent session type or agent API.

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
