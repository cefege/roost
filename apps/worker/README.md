# @roost/worker (v2)

Bun worker on every Mac in the fleet. It owns the PTYs of `shell` sessions and
ships their authoritative terminal grid to coord. An `agent` session has no PTY:
the worker forks one `omp --mode rpc-ui` child per session (`src/agent/`) and
projects its JSON-over-stdio event stream into transcript entries. The worker
dials **outbound** to coord over a raw
WebSocket (`transport/CoordLink.ts` → `/ws/coord-worker/:fp`) and relays PTY
bytes both ways. It exposes no inbound HTTP/WS surface.

The v1 Rust worker (`crates/idea-worker` + `crates/idea-protocol`) and the
Elixir `main_node` hub it spoke to were removed in the v1 sunset; see git
history on branch `v1-sunset`.

## Navigate
- Entry: `src/main.ts` — key + bootstrap redeem + register + heartbeat + CoordLink + attachment reaper.
- Transport: `src/transport/CoordLink.ts` (raw-WS dial, proto frames, in-band JWT rotation).
- Sessions: `src/session-manager.ts` (SessionId → SessionRecord; spawn/kill/input/resize/scrollback).
- Keeper: `src/keeper/{multiplexed-main,multiplexed-client,protocol-v2}.ts` (one Bun-PTY subprocess per worker over a UDS).
- Wire shape: `@roost/shared` (add a variant in `apps/shared/src/wire/event.ts` first).

## Run / test / deploy
- Run: `bun apps/worker/src/main.ts`
- Test: `bun test apps/worker/tests/`
- Install LaunchAgent: `bash apps/worker/scripts/install.sh install`
- Deploy to a tailnet host: `bun apps/roost-cli/src/main.ts deploy <host>`
