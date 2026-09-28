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

## Next steps (lead `WorkerLead2W2`), in order
1. Wave 1 is committed (`5f67e12c`..`e5ecfe4a`, one commit per slice + composition) and pushed; gate
   evidence is in each commit body (2 runs x 170 binaries, 1223 passed, 0 failed; clippy workspace 0;
   lint 3077 inputs 0 violations; fmt clean). `W1Split` split six files the formatter pushed over 400.
2. Waves 2+3 phase 1 is DONE: 13 slices drafted under `target-track/drafts/<Slice>/` (gitignored), each
   with a self-sufficient `WIRING.md` (files, exact edits, v2 test map, planned mutations, agreements).
   Mirrored to `refs/heads/recover/workerroot-drafts`; restore with
   `git fetch origin recover/workerroot-drafts && git archive FETCH_HEAD target-track/drafts | tar -x`.
   Slices: WDoorHttp, WDoorTerm, WHeart, WResume, WAgentsDetect, WAgentsReport, WAgentsPrompt (also owns
   journal->link durable delivery and the v2 snapshot frame with a journal seq), WAgentsInstall, WAttach,
   WAttachDirect, WPeer, WKUpdate, WCapture. Phase-1 reports: `agent://WorkerLead2W2.<Slice>`.
3. Phase 2: one fresh agent per slice wires its drafts from its WIRING.md. Compile-order dependencies:
   WDoorTerm (`crate::local_terminal`) before WPeer and WDoorHttp's terminal route; WAgentsReport +
   WAgentsPrompt modules before WAgentsDetect; WAttach + WPeer (str0m driver) before WAttachDirect;
   WKUpdate (`probe_runtime`, `shutdown_*_on`) before WResume's keeper_boot retire and WHeart's keeper_runtime.
4. Integrator condition: Track 2W is not done while any of the 7 reply-less interim arms remains; each
   reaches its real owner in this push and the arm table above shows it.
5. Merge CoordLead2C's `bun_abi` restore on `KeeperContractV1` (roost-keeper `contract()`) before the next gate.
6. Then: track gate, per-slice commits, the audit over `worker-v2-map.md` (every v2 module has a `//!`
   header naming it or a Windows-only README entry), live door check on a live stack (never port 4114).
7. Disk: after every gate run `roost-target-sweep` on target-track; `cargo clean` it when it passes 12 GiB.

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
- Deliberate deviations (integrator-approved): resumed direct uploads append at `bytesWritten` (v2
  attachment-operation-owner.ts:266,374-381 writes at offset 0); the agent-report endpoint lives in the v3
  worker data dir (v2 `~/.roost/agent-report.{cap,sock}` would collide during the Stage-4 side-by-side run).
- Unhandled-CSI telemetry: vte 0.15 drops unrecognised CSI inside vte (not alacritty), so
  roost-term runs a shadow `vte::Parser` classify-only pass instead of vendoring vte.
