# Coord lead handoff — Stage 2C (sockets live, then the rows)

**A moment, not a state.** Read `git status --porcelain` and `git log --oneline -10`
in `/home/almalinux/repos/roost-v3-coord` before trusting any line below. The plan
(`docs/v3-handoff/roost-v3-finish-and-cutover-plan.md` "### Stage 2C") wins.

## Build environment

```
export PATH="$HOME/.cargo/bin:$PATH" CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=3 \
       CARGO_TARGET_DIR=/home/almalinux/repos/roost-v3-coord/target-track
flock target-track/.roost-build.lock cargo …
```

JS side of the C-B check: `bun install` + `bun run --cwd apps/web build` done once in
this worktree (`apps/web/dist` exists, untracked).

## Base

`v3-coord` = `b0d026bc` (= `v3`, S3.0 green). Ratchet at start:
`grep -c 'PortStatus::AwaitingDomainPort' crates/roost-coord/src/rpc/method_route_rows.rs` = **19**;
`#[ignore]` in `crates/roost-coord` = 0.

## Done (SHAs)

- none yet in Stage 2C.

## In flight

- Wave C-B: slices `WlWire` (worker_link wire + `worker_upgrade`) and `Sy2Driver`
  (`sync_ws/{socket,driver}.rs` + `sync_upgrade`) running in parallel; a read-only
  prep map of the C-C/C-D slices is being written.

## Next step

Integrate C-B, compile, gate, commit, push; run the C-B check
(`cargo build --release -p roost-cli -p roost-keeper`, then
`ROOST_SMOKE_COORD_EXECUTABLE=target-track/release/roost bun smoke/terminal/live-stack.ts`
→ `READY <url> worker=<fp>`), report to Main, then C-C/C-D in the plan's order
(S4, C-INPUT, C-SEND, SY3, C-SCREEN first).
