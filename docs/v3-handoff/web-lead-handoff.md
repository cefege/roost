# Track U — web lead handoff

Worktree `/home/almalinux/repos/roost-v3-web`, branch `v3-web`. The worktree
(`git log --oneline -5 && git status --short`) is the state; this note is the
moment it was written. Plan: `roost-v3-finish-and-cutover-plan.md` "### Stage 5".

## Build rule

`source /tmp/webenv.sh && c <cargo args>` — wraps every cargo/dx call as
`flock target-track/.roost-build.lock /home/almalinux/repos/roost-build-slot cargo …`
with `CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=3 CARGO_TARGET_DIR=target-track`
(recreate the script from this line if `/tmp` was wiped). Never end a turn
while a build runs: block in the foreground on the lock.

## Done

| Item | Commit | Evidence |
|---|---|---|
| U-0 items 1–3 | merged into `v3` at `d1258917` | — |
| U-1 DECODE (+ RPC/Sync client codecs, WebDeviceKey) | `dcfd84bb` | 694/0/3 twice (3 named ignores), clippy workspace 0, lint 0 (2955 inputs), fmt clean, wasm32 build 0; mutations in the commit body |

## In flight

U-1 PUMP (`crates/roost-web/src/pump*`, core boot path: hydration tickets,
access state, probe). Then SMOKE (56 `SmokeApi` methods), TERM mount, then the
U-2 table in parallel slices (shared slice brief: `/tmp/web-slice-context.md`,
copied into each task's context).

## Exact next step

Land PUMP: `bun smoke/terminal/live-stack.ts` with `ROOST_SMOKE_WEB_DIST`
pointing at a `dx build` → the browser leaves `Checking`.
