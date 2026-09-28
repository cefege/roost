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

## Done (SHAs on `recover/workerroot` = `origin/v3-worker`)
| SHA | What |
|---|---|
| `29efc28e` | Terminal view registry moved (git mv) from roost-coord to `roost-protocol/src/terminal_view/` — shared by coord hub and worker view owner, as v2's `packages/protocol/src/terminal-view`. |
| `b69afc30` | `TerminalStreamResult.failure_kind` is `Option` (None ⇄ v2 UNSPECIFIED on a committed result). Guard + mutation in the commit body. |
| `deda6301` | Shared registry gains v2 `broadcast`, `view`, `set_session_allowed`, sweep → `syncWatching` (coord terminal_view tests 11/7/10 pass). CoordLead2C told the SHA. |

## Wave 1 (W-DOWN + W-INPUT + W-VIEW) — UNCOMMITTED; snapshot `recover/workerroot-snap` (force-updated; latest SHA via `git ls-remote origin refs/heads/recover/workerroot-snap`)
Restore on a fresh checkout: `git checkout recover/workerroot && git stash apply $(git ls-remote origin refs/heads/recover/workerroot-snap | cut -f1)` after `git fetch origin recover/workerroot-snap`.
The worktree `/home/almalinux/repos/roost-v3-worker` holds the same plus whatever the two
still-running slices wrote after the snapshot. Seams: `worker-w1-contract.md`; v2→Rust map:
`worker-v2-map.md`; per-slice constructors, mutations, open items: `worker-w1-wiring.md`
(all three in this directory). Transcripts: `history://WorkerLead2W.<Name>`.

| slice | state at snapshot |
|---|---|
| WInput | DONE — route owner, work budget, link authority, `session/input_write.rs`, roost-keeper `input_queue.rs` lane, worker pending table; tests + 2 mutations |
| WQuery | DONE — query-reply lane, roost-term reply queue / write_raw / shadow-vte unhandled CSI, unhandled_seq, replay_align (v2 test 2/7 red before); roost-term full suite + clippy NOT yet run |
| WPipeline | DONE — terminal_core_capacity (+ roost-host `host_memory.rs`), pipeline owner; admission at spawn/respawn/adoption/boot; `adopt_survivors` returns `Result` |
| WCells | DONE — `runtime/cell_cadence.rs` driver (cells now flow), sync-output hold, raw + semantic metadata lanes, `LinkLifecyclePort` |
| WView | DONE — `terminal_view/` owner over the shared registry; 12 tests, 4 mutations |
| WDown | DONE (after the first snapshot; in snapshot 2 below) — uplink, link_ports, every downstream arm, `grep 'other =>'` empty, link_wire `InvalidBrowserCommand`; hello now sends `advertised()`; seams changed: `write_input`/`apply_stream_state` return `OwnerFuture<Option<..>>`, `apply_stream_state` takes a `LinkFence`. Still needed: `link.attach_owners(..)`. Parity gaps it reported (fix in the gate pass): control lane not cleared on detach (v2 clears it); pong waits for the barrier (v2 sends before live); `coord-link-repair-order.test.ts` unported — Rust outbox drains Control before Terminal, needs an outbox decision |
| WStream | DONE (in the latest snapshot) — `session/{terminal_state,terminal_control,terminal_txn,core_reprove}.rs`, resize.rs rewritten as the txn's keeper step, `StreamOwner` implements `TerminalStreamPort`; 16 tests incl. real-keeper resize. Mutation 2 caught; mutation 1 (skip the resize in a committed txn) only partly observed — re-run it against terminal_stream_state + terminal_stream_keeper in the gate pass. Constructor: `StreamOwner` in `session/terminal_control.rs` (see its result at history://WorkerLead2W.WStream) |

## Next steps, in order (exact)
1. Read `agent://WorkerLead2W.WDown` / `.WStream`; if unfinished, complete their `# Done`
   lists from their transcripts.
2. Composition (lead files): `pub emitter` on `SessionStack` (`cells.emitter()` in
   `session_stack.rs`); new `runtime/owners.rs` building `DownstreamOwners { input, stream,
   pipeline, view, lifecycle }` from `worker-w1-wiring.md`; in `boot_sequence.rs` (~388
   lines — keep construction in `owners.rs`): one `CoordinatorCellSink` shared by
   `link.attach_cell_sink` and `CellCadence::spawn`, query-reply lane attach + writer spawn,
   `link.attach_owners(..)` before `link.run`, `?` on `adopt_survivors(..).await`, dispose
   view/routes/work_budget and abort the cadence at shutdown.
3. Session-closed hook (none exists): `SessionManager::close_channel` (`session/lifecycle.rs`
   ~:198-270) notifies observers after `sessions.forget`, no lock held →
   `view.close_session(&sid)`, `routes.retire_session(sid)` (v2 main.ts:228-236).
4. Track gate (commands from `.github/workflows/ci.yml`): two agreeing runs of `cargo test
   -p roost-worker -p roost-keeper -p roost-term -p roost-protocol -p roost-host
   --no-fail-fast`; `cargo clippy --workspace --all-targets -- -D warnings` (roost-coord
   depends on roost-keeper, whose client API WInput changed); `ROOST_REPO_ROOT=$PWD cargo
   xtask lint` (read inputs); `cargo xtask fmt` + `git status --short`; wasm32 build of
   roost-protocol. Commit per slice (v2 files, mutations from `worker-w1-wiring.md`, consumer
   of each new value), push both refs, wave report to Main.
5. Wave 2: W-DOOR (shares `routes`+`work_budget`; `TerminalViewOwner::register_local`;
   `CellCadence::register_sink`), W-HEART (reads `manager.terminal_core_capacity().snapshot()`),
   W-CAPS (hello sends `advertised()` = terminal_metadata_v1 + terminal-view-owner-v1 +
   terminal-input-route-v1, test asserts the constant; delete `NoSnapshot`), W-RESUME (keeper
   `channel_history` always refuses → adoption always respawns; respawn of a held session
   fails at table insert and leaves the PTY running; keeper-death reconcile; force-live
   retire shutdown frame), OSC 7 cwd change publishes no session event (v2 did).
6. Wave 3: W-AGENTS, W-ATTACH, W-PEER, W-KUPDATE, W-CAPTURE; then the audit over
   `worker-v2-map.md` (every module ported with a `//!` header naming it, or Windows-only in
   `crates/roost-worker/README.md`).

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
