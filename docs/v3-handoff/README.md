# v3 handoff — track state and next steps (development stays on this host)

The active plan is `roost-v3-finish-and-cutover-plan.md` in this directory;
every other file here is a brief or lead note it cites (`local://` paths from
the original session were rewritten to `docs/v3-handoff/`). The plan wins on
any conflict. Nothing else from the original host is needed: `git clone`.

## Disk budget on this host

Disk, not memory, stopped the run. The root is 111 GiB (no unallocated space
on `sda`), ~50 GiB is fixed content, and each worktree's cargo target dir grew
to 13–17 GiB during clippy + tests; three tracks plus the gate dir drove free
space to 4 GiB. RAM was fine (31 GiB, 24 available, no OOM). Countermeasures:
`~/.cargo/config.toml` sets `debug = "line-tables-only"` for the dev and test
profiles host-wide; at most two tracks build at once; `cargo clean` a track's
`target-track` once its branch is pushed and it has no build left in its wave.
sccache would save CPU, not disk.

## Stage 0 state (plan "### Stage 0") — COMPLETE

| Branch | SHA | State |
|---|---|---|
| `v3-worker` (= `recover/workerroot`) | `cceaf15a` | DONE. F6+F13 mutation confirmed; five over-cap files split; origin/v3 merged; 26 pre-existing red tests classified by v2 and fixed. Gate: `cargo test -p roost-worker -p roost-keeper` 603/0/0 twice, clippy 0, lint 0 (2492 inputs), fmt clean |
| `v3-coord` | `f3a717a6` | DONE: `files.rs` split + 8 arms (AwaitingDomainPort 27→19), send-queue ignore retired (0 `#[ignore]` in roost-coord), `event_snapshot_cap.rs`, 2 clippy fixes, origin/v3 merged. Gate: `cargo test -p roost-coord --no-fail-fast` 113 binaries 712/0/0 twice, clippy 0, fmt clean; lint 8 = roost-keeper only (cleared by the worker merge) |
| `v3-web` | `10822254` | U-0 items 1–3 DONE. Gate: 613/0/3 twice (the three named ignores), clippy 0, fmt clean, wasm32 build 0; lint 8 = roost-keeper only, cleared by the worker merge |
| `coord-guard-move-snap` | `73cf9322` | Snapshot of `roost-v3-coord-split`'s one untracked file (`recover-held-rollout.ts`) |
| `v3-web-snap` | `17464bb5` | U-1 (DECODE in progress) uncommitted work, snapshot per the plan's rule. Restore: `git checkout v3-web && git stash apply 17464bb5`. Not compiled-verified |
| `v3` | `78d5dc24` | All three tracks merged; S3.0 GREEN (see `docs/v3-gate-baselines.md` "Phase gates"); merged back into each track |
| `coord-guard-move` | `f7ff7e37` | Pushed; merged into no track. Inspect before deleting |

## Next steps, in order

1. Stage 2C wave C-B, Stage 2W first wave (W-DOWN + W-INPUT + W-VIEW), and U-1 (resume from `v3-web-snap`) in parallel, per the plan.
2. Each track lead keeps its own `docs/v3-handoff/<track>-lead-handoff.md` current and commits it with every push, so a crashed lead loses nothing.

## Host facts the plan relies on

- Production (`mike.roosttt.com`) still runs v2 on the original host; Stages 3.3–4 run there (install gate user, cloudflared, caddy, systemd bridge). The v2 keeper on that host is never killed.
- Port 4114 (v3 local door) was free on the original host at handoff; v2's door is 4104.
