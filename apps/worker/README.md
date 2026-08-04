# @roost/worker (v2)

Bun worker on every Mac in the fleet. It owns every session's shell PTY, ships
the authoritative terminal grid to coord, and relays PTY bytes both ways over
an outbound raw WebSocket (`transport/CoordLink.ts` →
`/ws/coord-worker/:fp`). Agent CLIs are ordinary programs launched inside
those PTYs; the worker has no agent RPC child or structured transcript path. It
exposes no inbound HTTP/WS surface.

The v1 Rust worker (`crates/idea-worker` + `crates/idea-protocol`) and the
Elixir `main_node` hub it spoke to were removed in the v1 sunset; see git
history on branch `v1-sunset`.

## Navigate
- Entry: `src/main.ts` — key + bootstrap redeem + register + heartbeat + CoordLink + attachment reaper.
- Transport: `src/transport/CoordLink.ts` (raw-WS dial, proto frames, in-band JWT rotation).
- Sessions: `src/session-manager.ts` (SessionId → SessionRecord; spawn/kill/input/resize/scrollback).
- Keeper: `src/keeper/{multiplexed-main,multiplexed-client,protocol-v2}.ts` (one Bun-PTY subprocess per worker over a UDS).
- Wire shape: `@roost/shared` (add a variant in `apps/shared/src/wire/event.ts` first).
- Agent status: `src/agent-status/` (see below).

## Agent status (volatile metadata, not an agent API)

Labels each shell PTY with `working` / `blocked` / `idle` for whatever coding
agent runs inside it. Nothing is persisted: frames go upstream as
`CoordWorkerUp.agent_status`, are resent after every CoordLink reopen, and are
dropped when the session closes.

- **Who is running** — `process-scan.ts` takes a throttled `ps -A` snapshot
  (250 ms) and finds a known agent in the session's process tree; identity is
  held through one missed scan so a momentary miss doesn't flap.
- **Authoritative reports** — `report-server.ts` listens on
  `$ROOST_AGENT_SOCKET_PATH` (default `~/.roost/agent-report.sock`, dir `0700`,
  socket `0600`). One JSON line per request:
  `{"version":1,"method":"agent.report","params":{session_id?,pid,agent,state,message?,seq,active}}`
  → `{"ok":true}` or `{"ok":false,"error":"…"}`. Caps: 4 KiB per line, 32
  requests per connection. The worker resolves `pid` to the session that owns it
  and rejects a mismatch (`pid_session_mismatch`), so a report cannot claim
  another terminal. `seq` must increase; `active:false` withdraws.
- **PTY env** — `environment.ts` injects `ROOST_AGENT_SOCKET_PATH` +
  `ROOST_SESSION_ID` into every spawned/respawned shell.
- **Owned integration files** — `install-integrations.ts` writes
  `roost-omp-agent-state.ts` and `roost-pi-agent-state.ts` (mode `0600`,
  temp-file + rename, idempotent) into
  `${PI_CONFIG_DIR:-~/.omp}/agent/extensions` and
  `${PI_CODING_AGENT_DIR:-~/.pi/agent}/extensions`. A file that exists without a
  `ROOST_INTEGRATION_ID=<id>` marker is a user file and is never overwritten.
  `ROOST_AGENT_STATUS_DISABLED=1` makes an installed integration inert.
- **Arbitration** — `registry.ts`: an integration report wins while its 30 s
  lease is fresh, otherwise the screen fallback does. State changes get a
  monotonic revision; `working|blocked → idle` also stamps a completion revision
  (what the browser's unseen "done" badge keys on).
- **Screen fallback** — `manifests.ts` + `manifest-engine.ts` +
  `stable-detection.ts` match the session's visible screen and OSC
  title/progress against per-agent manifests pinned from Herdr `eacea2da`
  (Apache-2.0). A plain `working → idle` needs repeated confirmation before it
  is published, so a redraw can't flicker a completion.

## Run / test / deploy
- Run: `bun apps/worker/src/main.ts`
- Test: `bun test apps/worker/tests/`
- Install LaunchAgent: `bash apps/worker/scripts/install.sh install`
- Deploy to a tailnet host: `bun apps/roost-cli/src/main.ts deploy <host>`
