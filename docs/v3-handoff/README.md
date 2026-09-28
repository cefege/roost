# v3 handoff — PAUSED 2026-09-28; resume on a new host from here

The active plan is `roost-v3-finish-and-cutover-plan.md` in this directory;
every other file here is a brief or lead note it cites (`local://` paths from
the original session were rewritten to `docs/v3-handoff/`). The plan wins on
any conflict. Nothing else from the original host is needed: `git clone`.

## Disk budget on this host

Disk, not memory, stopped the run. The root is 111 GiB (no unallocated space
on `sda`), ~50 GiB is fixed content, and each worktree's cargo target dir grew
to 13–17 GiB during clippy + tests; three tracks plus the gate dir drove free
space to 4 GiB. RAM was fine (31 GiB, 24 available, no OOM). The workspace
`Cargo.toml` already had dev/test `debug = "line-tables-only"`; on top of it,
host-local `~/.cargo/config.toml` sets `[profile.dev.package."*"] debug = false`
and host-target linker flag `--compress-debug-sections=zlib`. Measured on the
`roost-coord` test build: 12 GiB → 7.3 GiB → 4.3 GiB from clean; the full
workspace gate dir is ~11 GiB. A track dir mid-wave still grows to 10–14 GiB
(`debug/` only; compression verified present on newest test binaries): cargo
never deletes a test binary superseded by a new hash, and those were ~40% of
each dir. Every cargo/dx build goes through
`flock <worktree>/target-track/.roost-build.lock /home/almalinux/repos/roost-build-slot <cmd>`,
which admits at most two builds host-wide. After every gate, under the same
build lock: `/home/almalinux/repos/roost-target-sweep <worktree>/target-track`
(keeps the newest hash per executable in `debug/deps`, never touches
libraries). Delete release dirs after live-stack checks. `cargo clean` a
track's `target-track` between waves when it passes ~12 GiB. The integrator's
`target-gate` exists only during a merge gate. Never set `RUSTFLAGS` in a
build env (it replaces the config flag).

## PAUSE STATE — read this first

Development was paused by the user to move to a faster host. Every track's
work is on origin. Exactly ONE branch and at most ONE snapshot per track is
current — the table below. A snapshot is a `git stash create` commit: restore
it with `git checkout <branch> && git stash apply <snapshot>` on the base
named in the table. Each track's own handoff doc has the per-slice state —
read it before touching that track.

| Track | Current branch tip (pushed) | Current snapshot → restore onto | Track handoff |
|---|---|---|---|
| integrator | `v3` (this commit) | none | this file |
| coord (Stage 2C) | `v3-coord` = `b7ad6997` | `v3-coord-snap-pause` = `6c6245e4` → `b7ad6997` (the wave-3 slices' uncommitted work, NOT gated; taken by the integrator after stopping the coord lead and its slices, equal to the worktree) | `coord-lead-handoff.md` |
| worker (Stage 2W) | `recover/workerroot` = `v3-worker` = `0f79ffae` | `recover/workerroot-snap-pause` = `0b35b15d` → `0f79ffae` (waves 2–3; source verified equal to the worktree; also force-adds `target-track/pause/` drafts, mutation runners, `wresume_run.sh`, and `target-track/drafts/`) | `worker-lead-handoff.md` "PAUSE STATE" |
| web (Stage 5) | `v3-web` = `97ba537c` (worktree clean at pause) | none needed. `v3-web-snap-pause` = `419149c6` sits on the older `5a43d383` and holds only mutation scratch (4 mutated test files + `mut-artifacts/`: an unapplied `store_sidebar` strengthening patch, the MutShellSidebar/MutDeckSidebarCore catalogues). Mine it for those; never apply it to `97ba537c` | `web-lead-handoff.md` "Pause state" |

**Every other `*-snap*`, `*-wip*`, `*-drafts`, `*-preserve` ref on origin is
SUPERSEDED** by the table above (they are older points of the same tracks,
kept only as history): `v3-coord-snap`, `-snap2`, `-snap3`, `v3-coord-wip`,
`-wip2`, `v3-web-snap`, `-snap-u1pump`, `-snap-waveb`, `-waveb2`, `-waveb3`,
`-waveb3-commits`, `-waveb3-final`, `recover/workerroot-snap`,
`recover-workerroot-snap`, `v3-worker-snap*`, `v3-worker-wip`,
`v3-worker-wen-snap`, `v3-enroll-snap-b32731d`, `v3-cli-*snap`,
`v3-cli-preserve`. `recover/workerroot-drafts` (`08b5fa1a`) is the worker's
draft mirror, also inside `0b35b15d`. `coord-guard-move` /
`coord-guard-move-snap` are pre-v3-track history; inspect before deleting.

Where each track stands:

- **Coord.** Waves C-B and 2 gated (1247/0/0; clippy 0; lint 0). Rust coord passes `terminal-delivery` 4/4 with TS worker+web; `terminal-render` 3/5 (two Rust-only fails; check first whether they are the dropped view-stream controller — the plan's C-SCREEN ruling is conditional). `AwaitingDomainPort` = **11** at `c96a21a0` (C-DIRECT, AT, AG2, X2, D1, GS open; C-CAPTURE, C-PUSH, C-RETAIN open). Wave 3 slices were mid-flight at pause.
- **Worker.** Wave 1 gated (1223/0/0 ×2, clippy 0, lint 0; no catch-all arm; hello sends `advertised()`). Waves 2–3 (13 slices) wired and compiling in the snapshot, NOT gated: 3 known reds, most mutations and clippy pending, WResume's tests never run, capture protocol types not committed. Cross-track gaps: worker Connect client sends no worker credential; v3 coord does not fill `recovery_metadata`.
- **Web.** U-0, DECODE, PUMP (live check passed vs TS backend), wave A, renderer core, wave B (SHELL/SIDEBAR/DECK/TERM/SMOKE) committed. Tests 1323/0/4 ×2 and clippy 0 at `e09f39ca`; lint 0 measured by the integrator; fmt and wasm32 not re-run. Not done: both dx bundles, the TERM gate spec, all other U-2 rows. Known gaps: UI tab close never becomes `SessionsKill`; a 1013 close does not trigger an immediate redial.
- **v3 itself** is at the Stage 0 merge (S3.0 green at `78d5dc24`) plus docs. No track work after Stage 0 has been merged into `v3`.

Rulings made during the run (all in the tracks' commit bodies): parity = v2 wins, with ONE user-visible exception — resumed direct uploads append at `bytesWritten` (v2 wrote at offset 0, corrupting the file). Deliberate path deviations: worker agent-report endpoint and attachment base live in the v3 worker data dir. `bun_abi` restored on `KeeperContractV1` with the Rust keeper reporting `"rust"`. `terminal_metadata_v1` (underscores) is v2's spelling. Public auth routes = v2's 7 exactly. `SmokeApi` has 53 members.

Operating lessons (put them in every lead brief): a lead that ends a turn while waiting dies (forced tool choice → API 400) — block in the foreground instead; leads run out of request budget in 1.5–3 h, so scope each lead to what one budget finishes and commit after every gated item; one build lock per worktree serializes that track's helpers — keep ≤3 helpers per lead, one level deep; sweep stale test binaries after every gate.

## Stage 0 state (plan "### Stage 0") — COMPLETE (history; the PAUSE table above is current)

| Branch | SHA | State |
|---|---|---|
| `v3-worker` (= `recover/workerroot`) | `cceaf15a` | DONE. F6+F13 mutation confirmed; five over-cap files split; origin/v3 merged; 26 pre-existing red tests classified by v2 and fixed. Gate: `cargo test -p roost-worker -p roost-keeper` 603/0/0 twice, clippy 0, lint 0 (2492 inputs), fmt clean |
| `v3-coord` | `f3a717a6` | DONE: `files.rs` split + 8 arms (AwaitingDomainPort 27→19), send-queue ignore retired (0 `#[ignore]` in roost-coord), `event_snapshot_cap.rs`, 2 clippy fixes, origin/v3 merged. Gate: `cargo test -p roost-coord --no-fail-fast` 113 binaries 712/0/0 twice, clippy 0, fmt clean; lint 8 = roost-keeper only (cleared by the worker merge) |
| `v3-web` | `10822254` | U-0 items 1–3 DONE. Gate: 613/0/3 twice (the three named ignores), clippy 0, fmt clean, wasm32 build 0; lint 8 = roost-keeper only, cleared by the worker merge |
| `coord-guard-move-snap` | `73cf9322` | Snapshot of `roost-v3-coord-split`'s one untracked file (`recover-held-rollout.ts`) |
| `v3-web-snap` | `17464bb5` | SUPERSEDED (its DECODE work is committed in `dcfd84bb`) |
| `v3` | `78d5dc24` | All three tracks merged; S3.0 GREEN (see `docs/v3-gate-baselines.md` "Phase gates"); merged back into each track |
| `coord-guard-move` | `f7ff7e37` | Pushed; merged into no track. Inspect before deleting |

## Next steps on the new host, in order

1. Clone; create one worktree per track branch; restore ONLY the coord snapshot (`git checkout v3-coord && git stash apply 6c6245e4`, base `b7ad6997`) and the worker snapshot (`git checkout recover/workerroot && git stash apply 0b35b15d`, base `0f79ffae`); web needs none — never apply `v3-web-snap-pause`; reinstall `roost-build-slot` and `roost-target-sweep` from this directory (adjust paths) and `host-cargo-config.toml` as `~/.cargo/config.toml`.
2. Close out, per track, in parallel: coord wave 3 (the 11 rows + C-D remainder, the two terminal-render fails); worker waves 2–3 gate (fix the 3 reds, pending mutations/clippy, WResume, capture types as a protocol-only commit to coord); web gate remainder, both bundles, TERM spec, then U-2.
3. Integrator: merge `v3-worker`, `v3-coord`, `v3-web` into `v3` and re-run the workspace gate (S3.0-style) before Stage 3.

## Host facts the plan relies on

- Production (`mike.roosttt.com`) still runs v2 on the original host; Stages 3.3–4 run there (install gate user, cloudflared, caddy, systemd bridge). The v2 keeper on that host is never killed.
- Port 4114 (v3 local door) was free on the original host at handoff; v2's door is 4104.
