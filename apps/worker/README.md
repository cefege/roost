# @roost/worker (v2)

Bun worker on every Mac in the fleet. Owns the PTYs that host `claude` (or any
shell) and ships the authoritative terminal grid to coord; agent lifecycle
status is learned from `--settings` hooks (`claude/hooks.ts`) + `detect/`
screen-scrape, NOT by consuming the agent's stream-json (spawned but never
parsed). Dials **outbound** to coord over a raw WebSocket
(`transport/CoordLink.ts` → `/ws/coord-worker/:fp`) and relays PTY bytes both
ways. Purely outbound — no inbound HTTP/WS surface.

The v1 Rust worker (`crates/idea-worker` + `crates/idea-protocol`) and the
Elixir `main_node` hub it spoke to were removed in the v1 sunset; see git
history on branch `v1-sunset`.

## Navigate
- Entry: `src/main.ts` — key + bootstrap redeem + register + heartbeat + CoordLink + claude-hook UDS + attachment-reaper.
- Transport: `src/transport/CoordLink.ts` (raw-WS dial, proto frames, in-band JWT rotation).
- Sessions: `src/session-manager.ts` (SessionId → SessionRecord; spawn/kill/input/resize/scrollback).
- Keeper: `src/keeper/{multiplexed-main,multiplexed-client,protocol-v2}.ts` (one Bun-PTY subprocess per worker over a UDS).
- Agent status: `src/claude/hooks.ts` (claude `--settings` hook UDS → `applyAgentPatch`) + `src/detect/` (screen-scrape adapters for hookless agents, e.g. pi).
- Wire shape: `@roost/shared` (add a variant in `apps/shared/src/wire/event.ts` first).

Full architecture + run/deploy commands live in the repo root `CLAUDE.md`.

## Run / test / deploy
- Run: `bun apps/worker/src/main.ts`
- Test: `bun test apps/worker/tests/`
- Install LaunchAgent: `bash apps/worker/scripts/install.sh install`
- Deploy to a tailnet host: `bun apps/roost-cli/src/main.ts deploy <host>`
