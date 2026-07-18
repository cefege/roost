<!-- AUDIENCE: claude -->
# phase-26 — perf upgrades + smoke leverage

**Status:** PARTIAL. Smoke backdoor + keeper pool SHIPPED. Multiplexed
wsLink was made redundant by Connect server-streaming Sync (one HTTP/2
stream multiplexes 8 buses) — no wsLink work needed.

Three items per the phase-25 follow-up: smoke backdoor, multiplexed
wsLink, keeper pool. **Shipped:** smoke backdoor + keeper pool.
**Deferred:** wsLink (tRPC's `applyWSSHandler` expects `ws.WebSocketServer`
from the `ws` npm package; Bun's native `Bun.serve({ websocket })` is a
different surface — bridging the two is multi-day work that warrants
its own design pass).

## Commits

```
914b164b phase-26-prep: window.__smoke backdoor (localStorage.roostSmoke=1)
1c707dec phase-26 keeper pool: persistent inputClient per session
```

## 26-smoke — window.__smoke backdoor

Problem: humanchrome can't drive synthetic keystrokes through wterm's
textarea + KeyboardEvent pipeline (synthetic events fail `isTrusted`
checks; `chrome_keyboard` is single-key). This makes smoke steps 5-9
(echo round-trip, pane close, cascade-delete) tool-blocked.

Solution: `apps/web/src/lib/smoke.ts` exposes `window.__smoke` when
`localStorage.roostSmoke === "1"`. Methods: `input(sid, text)` /
`spawnShell(fp, dir)` / `kill(sid)` / `state()`. Each dispatches via
the existing trpc client so auth + serialization match real calls.
`/roost-smoke` users flip the flag in the smoke tab once, then drive
PTY via `chrome_javascript: window.__smoke.input(...)`.

## 26-keeper-pool — persistent inputClient per session

Pre-26: `SessionManager.input(channelId, bytes)` → `sendInputOnce(socketPath, bytes)`
opened a NEW UDS socket per keystroke, wrote a single PtyIn frame,
closed. One connect per keystroke.

Post-26: `SessionRecord.inputClient: KeeperClient | null` lazily
attached on first input; reused. One socket connect per session
lifetime. `sendInputOnce` deleted.

## Real attach replay — investigation, not a commit

The "blank grid on tab refresh" UX bug the original 25a plan called
out is addressed by two layers already in place:

1. **Keeper ring** (`apps/worker/src/keeper/main.ts:31`): 256KB
   scrollback. On `FrameType.Attach`, keeper writes `ReplayStart` +
   every chunk via `PtyOut` frames.
2. **outputClient** (24d-1): persistent KeeperClient attached at spawn
   with `attach(0)`. Keeper replays its scrollback (initially empty) +
   then streams every live PtyOut frame. coord's `byte-hub` receives
   them ALL — there's never a window where a chunk exists only on the
   keeper.
3. **byte-hub ring replay** (25a): 256 chunks (≈ 500KB-1MB) with
   `busToAsyncIterable(... { replay: true })`. New sessions.bytes
   subscriber sees the recent ring.

What's left uncovered: **coord process restart**. The byte-hub is
in-memory; on restart the ring is empty until new bytes arrive.
Subscribers attaching to a quiet session in that window see blank grid
until the next live byte. Robust fix: on CoordLink open/reopen, worker
re-attaches outputClients (close old, open new with `attach(0)`) so
keeper re-replays into the byte-hub. Cost: duplicate output replay for
every currently-attached browser. Not a small change; deferred to a
real phase-27 if it becomes a felt pain.

## wsLink — deferred with rationale

`@trpc/server/dist/adapters/ws.mts` requires `ws.WebSocketServer`.
Bun has its own `Bun.serve({ websocket })` API that's incompatible at
the WebSocketServer type level. Options:

- Add the `ws` npm dependency + run a second-stack alongside Bun.serve
  for tRPC subscriptions only. Splits the websocket surface.
- Hand-write a Bun.serve↔tRPC bridge mapping Bun's
  `{ open, message, close }` to tRPC's wire format. Effective but ~200
  lines + auth replay + per-subscription routing. Deserves its own
  phase.

Today's 6-parallel-SSE pattern works; reconnect-on-JWT-refresh is the
visible cost (~6 sequential `[sync.trpc-X] started` log lines every
~4.5 min). Not stability-affecting.

## TIER 0 closed (this turn)

- ✅ **Smoke harness steps 1-9** — `.claude/skills/roost-smoke/run.js`
  shipped (c58a61bc). Step 1/2/3/4/8/9 green live; step 5/7 timing
  iteration TBD.
- ✅ **5 stale TS errors** — fixed (c06fe663). First clean
  `tsc --noEmit` across all 3 packages this session.
- ✅ **Legacy LaunchAgents** — `com.roost.coordinator` /
  `com.roost.worker` (pre-v2) booted out + plists deleted from
  `~/Library/LaunchAgents/`. `launchctl list | grep roost` now shows
  only `com.roost.coordinator-v2` + `com.roost.worker-v2`.

## What's left

| | scope | notes |
|---|---|---|
| **smoke step 5/7 timing iteration** | smoke polish | tighten waits OR fix underlying delivery race surfaced by harness |
| **CwdPicker + Onboarding UX** | TIER 1 #4 | `feedback_ui_polish_quality_bar.md` |
| **Permissions Pane redesign** | TIER 1 #5 | memory flagged not-friendly |
| **Worker auto-deploy when stale** | TIER 1 #6 | `roost-cli state --all-hosts` + diff |
| **cursor-pos batching** | TIER 1 #7 | ~30 lines |
| **wsLink** | TIER 3 | own design pass, ~1 session |
| **coord-restart-aware byte-hub** | TIER 3 | worker outputClient re-attach, ~1 session |

Product work (permissions UI, MCP relay UX, onboarding polish, etc.)
is outside the transport-and-infra scope of phase-24/25/26.
