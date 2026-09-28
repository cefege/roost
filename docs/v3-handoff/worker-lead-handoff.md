# Track W lead handoff — `recover/workerroot` (pushed to `v3-worker`)

Lead: `WorkerLead2W` (Stage 2W of `roost-v3-finish-and-cutover-plan.md`). Earlier leads'
notes (W-C compile-to-green, Stage 0) are in git history of this file; everything they
listed as open is either done on `v3` (S3.0 green at `78d5dc24`) or is a Stage 2W row below.

## Build rule (host-wide, user-agreed)
Every cargo command: `export PATH="$HOME/.cargo/bin:$PATH" CARGO_INCREMENTAL=0
CARGO_BUILD_JOBS=4 CARGO_TARGET_DIR=/home/mike/repos/roost-v3-worker/target-track`,
run as `flock <worktree>/target-track/.roost-build.lock /home/mike/repos/roost-build-slot cargo …`
(at most two cargo builds host-wide). Never set RUSTFLAGS. `/home/mike/wl/wcargo.sh` wraps all of it
(recreate from this paragraph if it is gone). Stop and `cargo clean` target-track if
`df --output=avail -B1G /home | tail -1` < 10.

Never end a turn while waiting on a build or a slice (a turn with no tool call kills the
agent): block in the foreground (eval `time.sleep` loop, or `flock <lock> true`).

## Done (SHAs on `recover/workerroot` = `origin/v3-worker`)
| SHA | What |
|---|---|
| `29efc28e` | Terminal view registry moved (git mv) from roost-coord to `roost-protocol/src/terminal_view/` — shared by coord hub and worker view owner, as v2's `packages/protocol/src/terminal-view`. |
| `b69afc30` | `TerminalStreamResult.failure_kind` is `Option` (None ⇄ v2 UNSPECIFIED on a committed result). Guard + mutation in the commit body. |
| `deda6301` | Shared registry gains v2 `broadcast`, `view`, `set_session_allowed`, sweep → `syncWatching` (coord terminal_view tests 11/7/10 pass). CoordLead2C told the SHA. |

## RESUME STATE (lead `WorkerLead`, host /home/mike) — read this first
- Series on `recover/workerroot` = `v3-worker`: `7a46b99a` protocol terminal-capture types (coord
  cherry-picked it as `7b19778b`), then the wave 2+3 per-slice commits (SERIES_SHAS). Shared files
  land whole in the first slice commit that lists them; only the series tip is gated.
- Compile fixes at resume: keeper_update_support bun_abi; packet-port tests import the split's traits;
  clippy -D warnings fixes across agents/attachments/capture/peer/runtime + 5 test files; roost-protocol
  attachment_transfer range checks.
- Reds classified against v2 (details in the slice commit bodies): boot_adoption_gate ×2, session_adoption
  ×2, session_spawn ×1 (test defects + one product double-release in spawn.rs); heartbeat fixture
  bun_abi; roost-host older-generation token in http_security.rs; local_terminal_socket stalled-socket
  (fixture: unattached coord sink held the baseline); link_agent_status retirement replay (test race
  before the snapshot barrier went live).
- Mutations run at resume (`/home/mike/wl/mutbatch.py`, specs `/home/mike/wl/muts-*.json`, logs
  `mut-base.log`, `mut-2.log`; copies in `target-track/pause/resume/`): 57 mutants: 56 killed (after the guards below), 1 survivor.
  First-pass survivors and their resolution: WDoorTerm M8/M8b and M12, WPeer M4 -> guards added
  (a_live_session_the_grant_does_not_name_is_refused, authority_is_asked_only_before_a_slice_that_has_rows,
  an_untransferred_read_returns_its_ceiling_to_an_open_port), each seen failing under its mutant; WPeer
  M17 was killed by a --lib test the batch had not run. Remaining survivor: WKUpdate K-M1b.
- Still open, in plan order: (1) mutations never run: WAgentsDetect M1-M12 + P1 (`pause/wad/run.py`),
  WAttach 8 (same_carrier, seq check, unique_name, manifest retain, idle-sweep direct, reaper sort,
  capability_matches, lease deadline()), WCapture 9 (list in its commit body), WDoorHttp batch C + D1;
  (2) WKUpdate K-M1b survived (process_reap.rs sweep removal not caught by
  a_nohup_job_that_ignores_sighup_is_still_reaped) — harden the test or classify equivalent;
  (3) the v2-map audit (`//!` headers naming v2 modules; Windows-only list in
  `crates/roost-worker/README.md`); (4) the live door check (`curl` the door on a Playwright stack port,
  never 4114); (5) [INFERENCE, untested] CellCadence registers an active unattached "coord" sink at boot:
  local-door deltas may stall until a coordinator link attaches (v2 suspends it on socket open,
  coord-link-deps.ts:148) — verify on a live stack.
- Cross-track gaps to REPORT (not worker-fixable): the worker Connect client sends no worker credential;
  the v3 coord does not fill `recovery_metadata` (v2 returns it only to a worker principal).
- Shared-file edits needing the integrator: root `Cargo.toml` (unicode-normalization, WAgentsInstall) + `Cargo.lock`
  edges (roost-worker -> unicode-normalization, async-compression); roost-protocol `local_ui_door.rs` (ProtoDoor) and
  `attachment_transfer/*` (ProtoAttach) are self-contained commits in the series.

## PAUSE STATE (superseded by RESUME STATE above; user stop order, lead `WorkerLead2W2`) — read this first
- Branch `recover/workerroot` = `v3-worker`: wave 1 (`5f67e12c`..`e5ecfe4a`, handoff `28e3cd1d`), then
  `b8cb104c` protocol bun_abi (cherry-pick of coord `c9772f99`, gated by CoordLead2C; roost-protocol +
  roost-keeper tests pass here) and `5208e0f9` protocol agent-status retirement decode (integrator-ruled;
  roost-protocol + roost-keeper tests pass; its clippy target NOT yet confirmed — the clippy run stopped on
  the capture test below), then this handoff commit.
- Uncommitted waves 2+3 (all 13 slices wired into the tree, ~400 status lines): snapshot
  `refs/heads/recover/workerroot-snap-pause` (parent = the pushed branch head). Restore:
  `git fetch origin recover/workerroot-snap-pause && git stash apply FETCH_HEAD`. The snapshot also
  force-adds `target-track/pause/` (commit drafts `commits2/<Slice>.{paths,msg}`, mutation runners and
  logs: wresume_run.sh + wresume_*.bak, wdt-muts.json/wdt-session.sh, wad-mutate.py, wheart-mut.py,
  wdurable/, wpeer/, wad/) and `target-track/drafts/` — copy `target-track/pause/*` back after restoring.
- Terminal-capture protocol types (`crates/roost-protocol/src/terminal_capture/*`, tests
  `terminal_capture_{view,validate,envelope}.rs`) are NOT committed: tests pass but
  `clippy --all-targets -D warnings` fails in `tests/terminal_capture_view.rs` (1 error). Fix it, then land
  them as a self-contained roost-protocol commit (recipe: `target-track/pause/../protocommits.sh` builds it
  in a scratch worktree) and send the SHA to CoordLead2C (C-CAPTURE cherry-picks it).
- Per-slice state at pause (reports: `agent://WorkerLead2W2.<Slice>P2`, plus `.WDurableP3`):
  | slice | state | still unverified / open |
  |---|---|---|
  | WDoorTerm | wired, 36/37 tests pass | red `local_terminal_socket::a_socket_that_stops_draining_is_dropped_alone` (unclassified); 15 mutations (wdt-muts.json) and clippy never ran |
  | WDoorHttp | paused mid-verification | see its report |
  | WHeart | wired, 74 tests pass, 18/19 mutations seen | new miss-reset phase in tests/heartbeat.rs unbuilt; mutation M4 unverified; clippy |
  | WResume | wired, compiles, roost-keeper suite green | worker tests + 9 mutations + clippy never ran: `pause/wresume_run.sh` (UNOWNED — assign WFinishA, integrator-approved) |
  | WKUpdate | keeper 35 + worker slice tests pass | re-runs after split, mutations M1–M4 + WA/WB batches, clippy |
  | WCapture | paused | see its report; protocol half above |
  | WAgentsReport | 26 tests pass, 5/12 mutations seen | 7 mutations, clippy |
  | WAgentsPrompt + WDurable + WBootOrder | run2 green, 21/24 guards hold, B2 guard added | run3 (B2 verdict), clippy, 3 pass-level A10 tests; cross-track: worker Connect client sends no worker credential and the v3 coord does not fill `recovery_metadata` (v2 returns it only to a worker principal) |
  | WAgentsInstall | 16 tests pass, M1–M8 seen | M9, clippy, in-tree test rerun (agent died on the forced-tool-choice 400) |
  | WAgentsDetect | paused | see its report; red `link_agent_status::successful_retirements_replay_in_order_without_an_active_snapshot` (times out) |
  | WAttach | paused | see its report |
  | WPeer | tests pass pre-split | post-split rerun, mutation batch B (M3,M4,M6,M7,M17), clippy |
  | WAttachDirect | all tests pass | 12 mutations + clippy (runner stopped by the pause; SIGTERM restored the file) |
- Arm table status (uncommitted tree): every interim arm now routes to a real owner — localTerminalGrant
  + Revoke (WDoorTerm); localTerminalPeerOffer, terminalTransportProbe, localTerminalPeerCancel,
  terminalDirectRetire (WPeer); localAttachmentPeerOffer/Cancel (WAttachDirect); agentPrompt
  (WAgentsPrompt); keeperUpdatePrepare (WKUpdate); localAttachmentGrant, attachmentDirectStatusRequest,
  localAttachmentGrantRevoke, attachmentChunk (WAttach). All 7 no-frame kinds are among them. Unverified
  until the gate.
- Next steps: restore; ≤3–4 finisher helpers at once (one worktree build lock; helpers die on the
  forced-tool-choice 400 when a long bash call is auto-backgrounded — tell them to block in one bash call
  with timeout ≤3600 and never end a turn); finish the unverified column above; the capture protocol
  commit; then the track gate on the whole tree, per-slice commits from `commits2/`, the v2-map audit
  (`//!` headers / Windows-only README list), the live door check (never port 4114), and
  `roost-target-sweep` after each gate.

## Wave 1 (W-DOWN + W-INPUT + W-VIEW + W-STREAM + W-PIPELINE + W-CELLS + W-QUERY) — composed, commit plan ready
All seven slices DONE and composed by W1Wire; W1Lower brought roost-term/keeper/host/protocol green
(no roost-coord/roost-cli caller of the old keeper-client API existed). Commit plan:
`target-track/commits/NN-<slice>.{paths,msg}` (01-WQuery … 07-WDown, 09-W1Wire; W1Lower's crate work
is folded into 01/02/03 so every commit builds — keeper's client API lands with WInput's pool.rs).
Regenerate/verify with `python3 target-track/commits/plan.py` (exit 0 = every `git status` path outside
target-track/ in exactly one .paths file). Per-slice constructors, mutations: `worker-w1-wiring.md`.

| slice | state |
|---|---|
| WQuery | query-reply lane + writer, roost-term reply queue / write_raw / shadow-vte unhandled CSI, unhandled_seq, replay_align |
| WPipeline | terminal_core_capacity (+ roost-host host_memory), PipelineOwner; admission at spawn/respawn/adoption/boot; `adopt_survivors(..).await?` refuses an over-cap survivor set |
| WInput | route owner, work budget, link authority, input_write, keeper input lane, worker pending table |
| WStream | stream txn with in-place keeper resize, core re-proof, trapped-core lane; mutation 1 re-run: fails terminal_stream_state ×3 + terminal_stream_keeper ×1 |
| WCells | CellCadence driver (cells flow), sync-output hold, raw + semantic metadata lanes, LinkLifecyclePort |
| WView | TerminalViewOwner over the shared registry |
| WDown | uplink, link_ports, one explicit arm per downstream variant (table below) |
| W1Wire | `runtime/owners.rs` (`WorkerOwners`: stack, downstream, routes, work_budget, view, cadence, uplink, coord_sink; `shutdown()` disposes view/routes/work_budget, aborts the cadence); one `CoordinatorCellSink` Arc for cadence + link; query-reply and cwd-event writers spawned; `link.attach_owners` before `link.run`; session-closed hooks (`SessionManager::on_session_closed`) → routes.retire_session + view.close_session; updateBroker = v2 running POSIX answer; pong on a liveness lane before live; control lane cleared on detach (agent status kept); owed writable repair leads queued controls (v2 repair-order test ported); hello advertises terminal-metadata-v1 + terminal-view-owner-v1 + terminal-input-route-v1; `NoSnapshot` deleted; OSC 7 cwd change publishes `SessionEvent::Cwd` |

### Downstream arms (31 Rust variants = v2's 17 `handleDownstream` cases in `coord-link-downstream.ts` + 10 `coord-link-direct-terminal.ts` cases + 4 retired coord-move tags v2 has no case for, `coord-link-downstream.ts:4-5`)
| kind (v2 file:line) | production owner / interim answer | owning slice |
|---|---|---|
| helloAck (downstream:99-106) | link barrier → `TerminalViewOwner::drop_coordinator_sockets` → `CellCadence::on_hello_ack` | done |
| ping (:107-111) | `Pong` on `outbox::Lane::Liveness`, written before live (link_drain) | done |
| browserCommand (:112-137) | `BrowserLink` → `browser_commands::Deps`; unparseable → rpc-error "invalid browser command" | done |
| binary (:138-142) | `terminal_input::InputOwner::write_binary` (DIR_TO_PTY) | done |
| inputRequest (:143-171) | `InputOwner::write_input` | done |
| agentPrompt (:172-199) | interim input-result REJECTED "worker agent prompt handler is unavailable" (:174-181) | W-AGENTS |
| terminalStreamState (:200-251) | `session::terminal_control::StreamOwner` (cap 64: "worker terminal-stream admission is full") | done |
| terminalPipelineSnapshot (:252-255) | `terminal_pipeline::PipelineOwner` | done |
| terminalSnapshotRequest (:256-259) | `StreamOwner::request_snapshot` | done |
| terminalViewRelay (:260-264) | `TerminalViewOwner::relay` | done |
| terminalViewSocketClosed (:265-268) | `routes.retire_connection` then `view.close_socket` | done |
| localTerminalGrant (:269-296) | interim rpc-error "local terminal grants unsupported by this worker" (:271-277) | W-DOOR |
| localTerminalGrantRevoke (:297-300) | v2 absent-owner = no reply (`?.()`); warn log | W-DOOR |
| keeperUpdatePrepare (:301-323) | interim rpc-error "keeper update preparation unsupported by this worker" (:303-309) | W-KUPDATE |
| attachmentChunk (:324-331) | v2 absent-owner = no reply; warn log | W-ATTACH |
| updateBroker (:332-366) | final: non START/STATUS → "unsupported updater action: <action>" (:342-344); else "Windows update broker command received on a POSIX worker" (coord-link-deps.ts:379-381 via :362-364) | done (Windows not ported) |
| eventAck (:367-375) | link barrier; `on_snapshot_ready` when it takes the link live | done |
| localTerminalPeerOffer (direct:116-133) | interim local-terminal-peer-error "disabled" (:119-121) | W-PEER |
| localTerminalPeerCancel (direct:134-136) | v2 absent-owner = no reply; warn log | W-PEER |
| localAttachmentPeerOffer (direct:137-154) | interim local-attachment-peer-error "disabled" (:140-142) | W-PEER |
| localAttachmentPeerCancel (direct:155-157) | v2 absent-owner = no reply; warn log | W-PEER |
| attachmentDirectStatusRequest (direct:158-165) | interim attachment-direct-status error "upload_not_found" (:79-95) | W-ATTACH |
| localAttachmentGrant (direct:166-192) | interim rpc-error "local attachment grants unsupported by this worker" (:169-175) | W-ATTACH |
| localAttachmentGrantRevoke (direct:193-195) | v2 absent-owner = no reply; warn log | W-ATTACH |
| terminalInputRouteClaim (direct:196-217) | `InputOwner::claim_route`; none/failed → refused "route_claim_busy" | done |
| terminalTransportProbe (direct:218-223) | v2 absent-owner = no reply (answers only an owner result); warn log | W-PEER |
| terminalDirectRetire (direct:224-226) | v2 absent-owner = no reply; warn log | W-PEER |
| coordMovePrepare / coordMoveSnapshotStart / coordMoveSnapshotChunk / coordRelocate | inert (retired tags; v2 ignores a variant with no case) | — |
The 6 interim texts and every reply-less arm are pinned in `tests/link_downstream_absent.rs`.

## Decisions
- Downstream kinds whose owner lands in a later wave answer with v2's own absent-owner
  reply (e.g. "local terminal grants unsupported by this worker") in an explicit arm; the
  owning slice replaces the arm in its commit. Rust `match` exhaustiveness makes a
  catch-all-free W-DOWN impossible otherwise.
- `update-broker` answers v2's running POSIX behaviour: non START/STATUS -> `unsupported updater
  action: <action>`, else "Windows update broker command received on a POSIX worker"
  (coord-link-deps.ts:380-381 via coord-link-downstream.ts:362-364).
- Seven downstream kinds have no v2 absent-owner reply (v2 `deps.onX?.()` sends nothing): the interim
  arm sends nothing, warns, and is pinned by a no-frame test (integrator-approved).
- Deliberate deviations (integrator-approved; also in crates/roost-worker/README.md): attachment base = `<v3 worker data dir>/attachments`; resumed direct uploads append at `bytesWritten` (v2
  attachment-operation-owner.ts:266,374-381 writes at offset 0); the agent-report endpoint lives in the v3
  worker data dir (v2 `~/.roost/agent-report.{cap,sock}` would collide during the Stage-4 side-by-side run).
- Unhandled-CSI telemetry: vte 0.15 drops unrecognised CSI inside vte (not alacritty), so
  roost-term runs a shadow `vte::Parser` classify-only pass instead of vendoring vte.
