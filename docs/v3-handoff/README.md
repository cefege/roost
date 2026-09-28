# v3 handoff — development moved off the original host (2026-09-28)

The active plan is `roost-v3-finish-and-cutover-plan.md` in this directory;
every other file here is a brief or lead note it cites (`local://` paths from
the original session were rewritten to `docs/v3-handoff/`). The plan wins on
any conflict. Nothing else from the original host is needed: `git clone`.

## Why the move

Disk, not memory. The original host has a 111 GiB root (no unallocated space
on `sda`), ~50 GiB of fixed content, and each worktree's cargo target dir
grows to 13–17 GiB during clippy + tests; three tracks plus the gate dir
drove free space to 4 GiB. RAM was fine (31 GiB, 24 available, no OOM).
On the new host: ≥250 GB disk, one shared `CARGO_TARGET_DIR` or sccache,
and `CARGO_PROFILE_DEV_DEBUG=line-tables-only CARGO_PROFILE_TEST_DEBUG=line-tables-only`
in every build env; `cargo clean` a track's target once its branch is pushed.

## Stage 0 state (plan "### Stage 0")

| Branch | SHA | State |
|---|---|---|
| `v3-worker` (= `recover/workerroot`) | `cceaf15a` | DONE. F6+F13 mutation confirmed; five over-cap files split; origin/v3 merged; 26 pre-existing red tests classified by v2 and fixed. Gate: `cargo test -p roost-worker -p roost-keeper` 603/0/0 twice, clippy 0, lint 0 (2492 inputs), fmt clean |
| `v3-coord` | `f3a717a6` | Commits DONE: `files.rs` split + 8 arms (AwaitingDomainPort 27→19), send-queue ignore retired (0 `#[ignore]` in roost-coord), `event_snapshot_cap.rs`, 2 clippy fixes, origin/v3 merged. Gate: test run 1 `cargo test -p roost-coord --no-fail-fast` 113 binaries 712/0/0; **run 2 incomplete** (lead died after 5 binaries, 36/0); lint not recorded. Re-run run 2 + lint first |
| `v3-web` | `10822254` | U-0 items 1–3 DONE. Gate: 613/0/3 twice (the three named ignores), clippy 0, fmt clean, wasm32 build 0; lint 8 = roost-keeper only, cleared by the worker merge |
| `v3-web-snap` | `17464bb5` | U-1 (DECODE in progress) uncommitted work, snapshot per the plan's rule. Restore: `git checkout v3-web && git stash apply 17464bb5`. Not compiled-verified |
| `v3` | `b28005c0` | Nothing merged yet |
| `coord-guard-move` | `f7ff7e37` | Pushed; merged into no track. Inspect before deleting |

## Next steps on the new host, in order

1. Four worktrees as in the plan's Execution model (paths may differ; keep one per branch).
2. `v3-coord`: second `cargo test -p roost-coord --no-fail-fast` run + `ROOST_REPO_ROOT=$PWD cargo xtask lint`.
3. Plan Stage 0 step 5: trial merge `v3-worker`, `v3-coord`, `v3-web` into `v3` (in that order), workspace gate (lint 0, clippy 0, fmt clean, two `cargo test --workspace --no-fail-fast` runs 0 failed), record S3.0 in `docs/v3-gate-baselines.md` "Phase gates", merge `v3` back into each track.
4. Stage 2C wave C-B, Stage 2W first wave (W-DOWN + W-INPUT + W-VIEW), and U-1 (from `v3-web-snap`) in parallel, per the plan.

## Host facts the plan relies on

- Production (`mike.roosttt.com`) still runs v2 on the original host; Stages 3.3–4 run there (install gate user, cloudflared, caddy, systemd bridge). The v2 keeper on that host is never killed.
- Port 4114 (v3 local door) was free on the original host at handoff; v2's door is 4104.
