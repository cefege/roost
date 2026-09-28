# Track W lead handoff — `recover/workerroot` (pushed to `v3-worker`)

Lead: `WorkerLead2W` (Stage 2W of `roost-v3-finish-and-cutover-plan.md`). Earlier leads'
notes (W-C compile-to-green, Stage 0) are in git history of this file; everything they
listed as open is either done on `v3` (S3.0 green at `78d5dc24`) or is a Stage 2W row below.

## Build rule (host-wide, user-agreed)
Every cargo command: `export PATH="$HOME/.cargo/bin:$PATH" CARGO_INCREMENTAL=0
CARGO_BUILD_JOBS=3 CARGO_TARGET_DIR=/home/almalinux/repos/roost-v3-worker/target-track`,
run as `flock <worktree>/target-track/.roost-build.lock /home/almalinux/repos/roost-build-slot cargo …`
(at most two cargo builds host-wide). Never set RUSTFLAGS. `/tmp/wcargo.sh` wraps all of it
(recreate from this paragraph if /tmp was cleaned). Stop and `cargo clean` target-track if
`df --output=avail -B1G /home | tail -1` < 10.

Never end a turn while waiting on a build or a slice (a turn with no tool call kills the
agent): block in the foreground (eval `time.sleep` loop, or `flock <lock> true`).

## Done (SHAs on `recover/workerroot`)
| SHA | What |
|---|---|
| `29efc28e` | Terminal view registry moved (git mv) from roost-coord to `roost-protocol/src/terminal_view/` — shared by coord hub and worker view owner, as v2's `packages/protocol/src/terminal-view`. Integrator merged it into v3 / told coord to merge. |
| `b69afc30` | `TerminalStreamResult.failure_kind` is `Option` (None ⇄ v2 UNSPECIFIED on a committed result). |

## In flight — wave 1 (W-DOWN + W-INPUT + W-VIEW)
Contract: `local://worker-w1-contract.md` (copy of the slice table and seams). Slices
(agents `WorkerLead2W.<name>`): WDown (uplink + link_ports + every downstream arm, no
catch-all), WInput (route owner, work budget, input authority, input write, keeper input
queue), WStream (stream state txn, core reprove, resize caller), WView (view owner over the
shared registry; adds `view`/`broadcast`/`set_session_allowed`/sweep sync_watching to the
registry — its own commit, then message CoordLead2C the SHA), WPipeline (pipeline snapshot,
terminal core capacity), WCells (cell emission driver — `emit_cell_frame` had NO production
caller —, sync-output hold, terminal metadata lane, link lifecycle), WQuery (query-reply lane,
roost-term reply queue + unhandled CSI shadow parser, replay align).

## Next steps, in order
1. Collect wave-1 results; lead writes `runtime/owners.rs` (builds `DownstreamOwners`) and
   wires boot (`boot_sequence.rs`): cell driver spawn, view owner, capacity (before the
   session stack, v2 main.ts:104), `adopt_survivors` now returns `Result` (add `?`).
2. Track gate (two runs of `cargo test -p roost-worker -p roost-keeper -p roost-term
   -p roost-protocol --no-fail-fast`, workspace clippy, `ROOST_REPO_ROOT=$PWD cargo xtask
   lint`, `cargo xtask fmt` + `git status --short`), commit per slice, push both refs,
   wave report to Main.
3. Wave 2: W-DOOR, W-HEART, W-CAPS (hello sends `advertised()` incl. view-owner +
   input-route; delete `NoSnapshot`), plus W-RESUME (keeper `channel_history` always refuses
   → adoption always respawns; keeper-death reconcile; force-live retire shutdown frame).
4. Wave 3: W-AGENTS, W-ATTACH, W-PEER, W-KUPDATE, W-CAPTURE; then an audit pass over
   `local://worker-v2-map.md` (every v2 module ported with a `//!` header naming it, or
   Windows-only and listed in `crates/roost-worker/README.md`).

## Decisions
- Downstream kinds whose owner lands in a later wave answer with v2's own absent-owner
  reply (e.g. "local terminal grants unsupported by this worker") in an explicit arm; the
  owning slice replaces the arm in its commit. Rust `match` exhaustiveness makes a
  catch-all-free W-DOWN impossible otherwise.
- `update-broker` answers per the plan (POSIX): non START/STATUS → `unsupported updater
  action: <action>`, else "Windows update broker unsupported by this worker". v2's POSIX
  worker actually reaches its dep and throws "Windows update broker command received on a
  POSIX worker"; the plan's text was followed.
- Unhandled-CSI telemetry: vte 0.15 drops unrecognised CSI inside vte (not alacritty), so
  roost-term runs a shadow `vte::Parser` classify-only pass instead of vendoring vte.
