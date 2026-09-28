# WKUpdate — phase-2 hand-off (self-sufficient)

Slice W-KUPDATE of Stage 2W (`docs/v3-handoff/roost-v3-finish-and-cutover-plan.md` row "W-KUPDATE").
Ports v2 `apps/worker/src/keeper/update-admission.ts`, `apps/worker/src/keeper/keeper-process-reap.ts`
(POSIX half), `apps/worker/src/transport/coord-link-keeper-update.ts`, plus the administrative half of
`apps/worker/src/keeper/keeper-probe.ts` (shutdowns, waitForKeeperExit) and the `processEpoch` of
`keeper/multiplexed-main.ts`. Acceptance: ported v2 tests pass; keeper preservation across an update
admission (pid + channels unchanged when the keeper digest is unchanged); a killed channel's whole process
group/tree is reaped (real-PTY test with a forking child); mutations recorded; `wave-gates/done-WKUpdate`
written; report per the lead's context (files+line counts, v2 files, tests, mutations, consumers,
cross-owner edits, shared-file edits, parity gaps, commit draft + path list).

All drafts are under `target-track/drafts/WKUpdate/` mirroring repo paths. Move each to the same path in
`/home/almalinux/repos/roost-v3-worker`. Every draft was written against the tree as of phase 1; re-read each
anchor before editing (siblings edit concurrently). Build/test only through `/tmp/wcheck.sh` and
`/tmp/wcargo.sh` (rules in the lead's brief).

## 0. Key facts discovered (read first)

1. **The Rust keeper serves ONE connection at a time** (`crates/roost-keeper/src/server.rs` `serve`/`serve_one`).
   A fresh socket opened while the worker's `KeeperPool` holds its connection sits in the listen backlog and is
   never answered. So every proof/shutdown the worker does mid-life goes over the POOL's own connection
   (`KeeperPool.keeper: KeeperHandle`, `pub(super)`); only a caller holding no connection opens one.
2. **The Rust keeper `Hello` carried no pid / process epoch** (v2's did: `pid`, `process_epoch`). Admission,
   the coordinator's `verify_worker_result` (`crates/roost-coord/src/deploy/keeper_update/identity.rs`) and the
   heartbeat's `keeper_runtime` all need them → roost-keeper `KeeperObservation` gains them (edit K2–K4).
3. Bindings `(channel_id, pid)` come from `ListChannels` (`KeeperClient::list_channels`); the keeper answers a
   spawn before the next frame, so keeper-side `spawning_channels` is always empty (same claim as
   `runtime/keeper_probe.rs` header).
4. `portable_pty::Child::kill` = SIGHUP leader, poll 250 ms, SIGKILL leader only — leaks job-control groups and
   nohup'd jobs. The PTY master cannot be "closed" to force a kernel hangup because the reader thread and the
   input lane hold dups of it, so the reap sends the hangup signals by hand (SIGHUP+SIGCONT leader, SIGHUP
   foreground pgrp via `MasterPty::process_group_leader()`), then SIGTERM the leader's group, then SIGKILL every
   member of a pre-signal `ps -A -o pid=,ppid=` tree snapshot after 2 s (birth-time checked on Linux).
5. v2 keeper shutdown (`multiplexed-main.ts` `shutdown()`, including SIGTERM) calls `reapAllChannels` → Rust
   keeper binary calls `Keeper::reap_all_channels()` after its serve loop for every stop reason.
6. A successful JOURNALED preparation leaves channel admission closed and the reconcile boundary held (v2
   returns without rollback: the deploy replaces this worker). A completed MAINTENANCE shutdown releases both.
   Every failure releases both (admission first, then boundary — v2 order).

## 1. Draft files → repo paths (move verbatim, then fix compile)

| draft (relative to drafts/WKUpdate) | lines | purpose |
|---|---|---|
| crates/roost-keeper/src/process_reap.rs | 269 | v2 keeper-process-reap.ts POSIX: `ReapTarget{leader, foreground_group}`, `reap_channel_tree`, `reap_all_channels`, `terminate_process(pid:u32)->bool` (WResume uses it), `REAP_GRACE`=2 s |
| crates/roost-keeper/src/process_epoch.rs | ~37 | `pub(crate) fn mint_process_epoch() -> Option<String>` (v4 uuid from /dev/urandom) |
| crates/roost-keeper/tests/keeper_child_reap.rs | ~180 | port of v2 tests/keeper-child-reap.test.ts (3 tests, real daemon, bash -i, pgrep) |
| crates/roost-keeper/tests/keeper_update_proof.rs | ~110 | hello pid/epoch identical across reconnects + channels survive a client drop; forced `Shutdown` exits the daemon AND kills a SIGHUP-immune PTY child (keeper half of v2 keeper-force-live-refresh.test.ts) |
| crates/roost-worker/src/keeper_pool/runtime_proof.rs | 254 | `KeeperRuntimeProbe` (+`unreachable()`, `unauthenticated()`, `proof()`, `observation(ms)`), `KeeperRuntimeProof` (`binding_digest()`, `open_channel_ids()`, `is_empty()`), `binding_digest()`, `read_runtime_probe(&KeeperClient)`, `KeeperPool::probe_runtime()`, `probe_endpoint(socket)`, `KEEPER_IDENTITY_UNPROVEN`, `KEEPER_PROBE_TIMEOUT` |
| crates/roost-worker/src/keeper_pool/keeper_shutdown.rs | 204 | `EmptyKeeperShutdownExpectation`, `ExitWatch` trait, `HostFuture<'a,T>`, `wait_for_exit`, `wait_for_keeper_exit(socket)`, `endpoint_reachable`, `shutdown_empty_on/_forced_on(&KeeperClient)`, socket forms `shutdown_empty_keeper_authenticated`, `shutdown_keeper_authenticated`, `SocketExitWatch`, consts 30 s / 100 ms / 200 ms |
| crates/roost-worker/src/keeper_pool/update_admission.rs | ~250 | v2 update-admission.ts: `KeeperUpdateHost: ExitWatch` trait, `UpdateDirection`, `JournaledKeeperUpdateActionV1` (strict serde, `parse`, `validate`), `KeeperUpdateActionResult`, `apply_journaled_keeper_update_action`, `shutdown_keeper_for_maintenance` |
| crates/roost-worker/src/keeper_pool/update_host.rs | ~115 | `PoolKeeperHost::new(Arc<KeeperPool>, &Path)`: production `KeeperUpdateHost` (pool connection first, fresh socket fallback; after an accepted shutdown calls `pool.keeper_lost(..)`) |
| crates/roost-worker/src/keeper_pool/update_prepare.rs | ~225 | v2 coord-link-keeper-update.ts: `KeeperUpdateBoundary` trait + `BoundaryRelease`, `KeeperUpdateActions` trait + `HostKeeperUpdateActions::new(Arc<dyn KeeperUpdateHost>)`, `KeeperUpdatePreparer::new(manager, table, boundary, actions)` implementing `link_ports::KeeperUpdatePort` |
| crates/roost-worker/src/runtime/downstream/keeper_update.rs | ~52 | `Dispatcher::keeper_update_prepare` (owner → rpc-ok{data}/rpc-error{message}, fenced; no owners → v2 unsupported reply) |
| crates/roost-worker/tests/keeper_update_support/mod.rs | ~200 | `ScriptedHost` fake `KeeperUpdateHost` with virtual clock + `Exit::{Immediately,After,Never}`, contract/update/digest builders |
| crates/roost-worker/tests/keeper_update_action.rs | ~150 | port of v2 tests/keeper-update-action.test.ts (8 tests) |
| crates/roost-worker/tests/keeper_maintenance_admission.rs | ~95 | port of v2 tests/keeper-maintenance-admission.test.ts (7 tests; its log-line assertion not ported — no log capture in the crate) |
| crates/roost-worker/tests/keeper_update_preservation.rs | ~95 | real in-process keeper (`keeper_pool_support::KeeperFixture`): preserve admission built with `roost_protocol::keeper_update::keeper_update_admission` from the keeper's own observation → `preserved` with the keeper's pid/epoch/digest; drop pool, reconnect → same pid/epoch/digest and both channel pids (worker half of v2 `test:upgrade`) |
| crates/roost-worker/tests/keeper_update_prepare.rs | ~260 | handler tests: success stays closed + boundary held + input refused; session-set mismatch (v2 session-channel-creation-gate.test.ts test 3); failed action reopens; 6 malformed requests; maintenance success reopens; serialization |
| crates/roost-worker/tests/keeper_update_terminal_freeze.rs | ~150 | port of v2 tests/keeper-update-terminal-freeze.test.ts (2 tests: input via `session_support::Harness`, stream resize via `terminal_stream_support::Harness`) |
| crates/roost-worker/tests/link_downstream_keeper_update.rs | ~80 | arm tests: owner entered synchronously, rpc-ok with data; failure → rpc-error; panic → rpc-error with panic text |

Style notes for the mover: edition 2024 (`Future` is in the prelude; no `use std::future::Future`). The crate
convention is std `Mutex` (no parking_lot dependency). Every file ≤400 lines.

## 2. Exact edits to existing files

### roost-keeper (cross-owner)

K1. `crates/roost-keeper/src/lib.rs` — after `pub mod payloads;` / before `pub mod pty_channel;` add
`mod process_epoch;` and `pub mod process_reap;` (keep alphabetical order of the `pub mod` list).

K2. `crates/roost-keeper/src/payloads.rs` — struct `KeeperObservation` (currently `contract`, `live_channel_count`):
```rust
    /// The keeper's own pid, so a worker can prove which process holds its PTYs.
    #[serde(default)]
    pub keeper_pid: Option<u32>,
    /// This keeper process's incarnation (a v4 uuid minted at start); a recycled
    /// pid carries a different epoch.
    #[serde(default)]
    pub process_epoch: Option<String>,
```

K3. `crates/roost-keeper/src/keeper.rs`:
- struct `Keeper`: after `pub(crate) input_route: Arc<InputRoute>,` add
  `/// This process's epoch, reported in every Hello. \n pub(crate) process_epoch: Option<String>,`
- `Keeper::new()`: add `process_epoch: crate::process_epoch::mint_process_epoch(),`
- in the second `impl Keeper` block (near `is_empty`) add:
```rust
    /// Reap every live channel's whole process tree before the daemon exits
    /// (v2 `reapAllChannels`): a keeper that stops must not leave a PTY's
    /// children running with nothing to reach them.
    pub fn reap_all_channels(&mut self) {
        let targets: Vec<crate::process_reap::ReapTarget> = self
            .channels
            .values_mut()
            .filter_map(|channel| channel.pty.reap_target())
            .collect();
        tracing::info!(channels = targets.len(), "keeper: reaping every channel before exit");
        crate::process_reap::reap_all_channels(&targets);
    }
```
(keeper.rs is ~366 lines; stays <400.)

K4. `crates/roost-keeper/src/keeper_ops.rs` `hello()` — the `KeeperObservation { .. }` literal gains
`keeper_pid: Some(std::process::id()), process_epoch: self.process_epoch.clone(),`.

K5. `crates/roost-keeper/src/pty_channel.rs` — replace
```rust
    /// Terminate the child. Used by `KillChild` and by shutdown.
    pub fn kill(&mut self) {
        let _ = self.child.kill();
    }
```
with
```rust
    /// Terminate the child and every process it spawned (v2 `reapChannelTree`).
    /// Used by `KillChild` and by a respawn over a live channel.
    pub fn kill(&mut self) {
        if let Some(target) = self.reap_target() {
            crate::process_reap::reap_channel_tree(target);
        }
    }

    /// Where a reap starts, or `None` once the child has exited: a reaped
    /// leader's pid may already belong to someone else (v2's `ch.exited` guard,
    /// and v2 dropped an exited channel from its map before any shutdown reap).
    pub fn reap_target(&mut self) -> Option<crate::process_reap::ReapTarget> {
        if self.exited().is_some() {
            return None;
        }
        let leader = i32::try_from(self.child.process_id()?).ok()?;
        Some(crate::process_reap::ReapTarget {
            leader,
            foreground_group: self.master.process_group_leader(),
        })
    }
```
Also fix the file header line 8-9 claim ("The crate keeps its `unsafe` allowance for the socket") only if it now
misleads — process_reap.rs uses `unsafe { libc::kill }` with SAFETY comments (crate lints allow unsafe).

K6. `crates/roost-keeper/src/bin/roost-keeper.rs` `main` — after the `let reason = loop { .. };` and before the pid
file removal, add:
```rust
    // v2 `shutdown()` reaps every channel's tree before the process exits, for
    // every stop reason: a stopped keeper must not leave PTY children behind.
    server.keeper_mut().reap_all_channels();
```
(`server` must be `let mut server` — it already is.) Update the stale comment at the end ("Channels are dropped
here, which closes the master ends…") to say the trees were reaped above.

K7. `crates/roost-keeper/tests/support/daemon.rs` — `impl Keeper` add `pub fn pid(&self) -> u32 { self.child.id() }`.

K8. `crates/roost-keeper/tests/output_survives_control_round_trip.rs` ~line 79 — the `KeeperObservation { contract, live_channel_count }`
literal gains `keeper_pid: None, process_epoch: None,`.

### roost-worker

W1. `crates/roost-worker/src/keeper_pool/mod.rs` — add private mods `mod keeper_shutdown; mod runtime_proof; mod update_admission; mod update_host; mod update_prepare;`
(alphabetical with the existing list) and flat re-exports (the keeper_pool convention; siblings were told to import
flat from `crate::keeper_pool::…`):
```rust
pub use keeper_shutdown::{
    EmptyKeeperShutdownExpectation, ExitWatch, HostFuture, KEEPER_EXIT_CONFIRM_TIMEOUT,
    KEEPER_EXIT_POLL_INTERVAL, KEEPER_EXIT_PROBE_TIMEOUT, SocketExitWatch, endpoint_reachable,
    shutdown_empty_keeper_authenticated, shutdown_empty_on, shutdown_forced_on,
    shutdown_keeper_authenticated, wait_for_exit, wait_for_keeper_exit,
};
pub use runtime_proof::{
    KEEPER_IDENTITY_UNPROVEN, KEEPER_PROBE_TIMEOUT, KeeperRuntimeProbe, KeeperRuntimeProof,
    binding_digest, probe_endpoint, read_runtime_probe,
};
pub use update_admission::{
    JournaledKeeperUpdateActionV1, KeeperUpdateActionResult, KeeperUpdateHost, UpdateDirection,
    apply_journaled_keeper_update_action, shutdown_keeper_for_maintenance,
};
pub use update_host::PoolKeeperHost;
pub use update_prepare::{
    BoundaryRelease, HostKeeperUpdateActions, KeeperUpdateActions, KeeperUpdateBoundary,
    KeeperUpdatePreparer,
};
```
Update the mod.rs header if it claims the pool owns no lifecycle decision — it still decides none; the update
admission is a separate module the coordinator drives.

W2. `crates/roost-worker/src/link_ports.rs` — add (imports `roost_proto::DKeeperUpdatePrepare`):
```rust
/// Keeper replacement preparation. v2 `onKeeperUpdatePrepare`.
pub trait KeeperUpdatePort: Send + Sync + std::fmt::Debug {
    /// Channel admission closes synchronously in this call (v2 closes it before
    /// joining its serialized tail); the future resolves to the `rpc-ok` data,
    /// or the `rpc-error` message.
    fn prepare(&self, request: DKeeperUpdatePrepare) -> OwnerFuture<Result<serde_json::Value, String>>;
}
```
and `DownstreamOwners` gains `pub keeper_update: Arc<dyn KeeperUpdatePort>,` (header comment: add "keeper update
preparation"). Every `DownstreamOwners { .. }` literal must then set it: `runtime/owners.rs` (W4) and
`tests/link_downstream_support/mod.rs` (W6). Re-check whether another wave-2/3 slice already changed
DownstreamOwners' shape (e.g. to optional fields); follow whatever is in the tree.

W3. `crates/roost-worker/src/runtime/downstream/mod.rs` — add `mod keeper_update;` to the mod list (lines 15-17), and
replace the arm
```rust
            CoordWorkerDownstream::KeeperUpdatePrepare(request) => link.reply(replies::rpc_error(
                request.request_id,
                replies::KEEPER_UPDATE_PREPARE_UNSUPPORTED,
            )),
```
with `CoordWorkerDownstream::KeeperUpdatePrepare(request) => self.keeper_update_prepare(request, link),`.
`replies::KEEPER_UPDATE_PREPARE_UNSUPPORTED` stays (used when `owners` is `None`); `tests/link_downstream_absent.rs`
keeps passing unchanged (it dispatches with no owners).
**Replaced-arm report for the lead:** kind `keeper-update-prepare`; old reply `rpc-error "keeper update preparation
unsupported by this worker"` (still the owner-less answer); new owner `keeper_pool::KeeperUpdatePreparer` via
`DownstreamOwners.keeper_update`, answering `rpc-ok {outcome[, keeper_pid, keeper_epoch, binding_digest]}` or
`rpc-error <message>`.

W4. `crates/roost-worker/src/runtime/owners.rs` `WorkerOwners::build` — needs the keeper socket path: add a parameter
`keeper_socket: &std::path::Path` and pass `&boot.keeper_socket` from `runtime/boot_sequence.rs` (`WorkerOwners::build(`
call ~line 229; `boot.keeper_socket: PathBuf` in `runtime/boot.rs:104`). Keep `pool` alive: it is currently moved into
`let keeper: Arc<dyn KeeperPipelineSource> = pool;` — clone it first. Build:
```rust
        let keeper_update_host: Arc<dyn KeeperUpdateHost> =
            Arc::new(PoolKeeperHost::new(Arc::clone(&pool), keeper_socket));
        let keeper_update = KeeperUpdatePreparer::new(
            Arc::clone(&stack.manager),
            Arc::clone(&stack.table),
            boundary, // Arc<dyn KeeperUpdateBoundary>, see §4
            Arc::new(HostKeeperUpdateActions::new(keeper_update_host)),
        );
```
and `keeper_update: Arc::new(keeper_update) as Arc<dyn KeeperUpdatePort>` in the `DownstreamOwners` literal.
owners.rs was 182 lines at phase 1; siblings add to it too — keep it ≤400 (extract a helper fn if needed, never inline
into boot_sequence.rs).

W5. `crates/roost-worker/tests/keeper_pool_support/mod.rs` — `impl KeeperFixture` add
`pub fn socket(&self) -> &std::path::Path { &self.socket }`.

W6. `crates/roost-worker/tests/link_downstream_support/mod.rs` — `Fakes::owners()` literal gains
`keeper_update: Arc::clone(&fakes) as Arc<dyn KeeperUpdatePort>,` (add before `lifecycle: fakes as …`, which moves
`fakes`), import `KeeperUpdatePort` + `roost_proto::DKeeperUpdatePrepare`, and:
```rust
impl KeeperUpdatePort for Fakes {
    fn prepare(&self, request: DKeeperUpdatePrepare) -> OwnerFuture<Result<serde_json::Value, String>> {
        self.log.push(format!("keeper_update.prepare:{}", request.request_id));
        let answer = if request.maintenance {
            Ok(serde_json::json!({ "outcome": "shutdown" }))
        } else {
            Err("journaled keeper update request is malformed".to_owned())
        };
        self.settle(answer)
    }
}
```
(tests/link_downstream_keeper_update.rs relies on exactly these answers and on `OwnerMode::Panic` panicking with
"the fake owner failed on purpose".)

W7. `crates/roost-worker/tests/retire_support/mod.rs` ~line 224 — `KeeperObservation` literal gains
`keeper_pid: None, process_epoch: None,`.

## 3. Sibling interfaces (agreed by message in phase 1)

- **WHeart** (drafts/WHeart/crates/roost-worker/src/session/channel_creation_gate.rs): `SessionManager::begin_keeper_update_preparation(&self) -> impl Future<Output = PreparationRollback> + Send + 'static`
  (count incremented synchronously in the call; future awaits in-flight creation leases; drives
  `ControlLanes::set_keeper_update_prepared` on 0↔1), `PreparationRollback::rollback(&self)` (idempotent, NO rollback on
  drop), `SessionManager::keeper_update_prepared(&self) -> bool`. update_prepare.rs calls exactly these — it cannot
  compile before WHeart's gate lands. WHeart's heartbeat calls `pool.probe_runtime().await.ok().and_then(|p| p.observation(reconciled_at_ms))`.
  Test split: WHeart's `tests/channel_creation_gate.rs` ports v2 session-channel-creation-gate tests 1&2 at the manager
  seam; WKUpdate adds to tests/keeper_update_prepare.rs (once WHeart's `Harness::with_spawner` + `ScriptedSpawner` exist
  in tests/session_support): "with a held spawn, a preparation neither acquires the boundary nor calls the action until
  the leased spawn settles, then applies" (v2 test 1 `updateActionCalls==0 && reconcileBoundaryCalls==0` before the
  deferred spawn resolves) and "a failed admitted respawn drains, then the failed action releases the boundary" (v2 test 2).
- **WResume** (BLOCKED at phase 1, drafts in drafts/WResume): implements
  `impl crate::keeper_pool::KeeperUpdateBoundary for runtime::reconcile_gate::ReconcileGate` (draft
  drafts/WResume/crates/roost-worker/src/runtime/reconcile_gate.rs:233) and its WIRING says owners passes
  `Arc::new(gate.clone()) as Arc<dyn KeeperUpdateBoundary>` to the preparer (field `owners.reconcile`). WResume also uses
  `keeper_pool::{shutdown_forced_on, shutdown_empty_on, wait_for_keeper_exit, EmptyKeeperShutdownExpectation,
  read_runtime_probe, KeeperRuntimeProbe}` from runtime/keeper_boot.rs, and `roost_keeper::process_reap::terminate_process`.
  Keep those names stable. All imports are FLAT `crate::keeper_pool::X` (submodules are private).

## 4. Open decision for phase 2: the reconcile boundary

If WResume's `ReconcileGate` is in the tree when you wire W4, pass it. If it is NOT (WResume never landed), the Rust
worker has no mid-life reconcile serializer at all, so v2's boundary has nothing in flight to wait for and nothing to
block: ask the lead (agent://WorkerLead2W2) before inventing a stand-in; do not ship a no-op boundary silently
(silent-no-ops.md).

## 5. Tests to run (after wiring)

- `/tmp/wcargo.sh test -p roost-keeper --test keeper_child_reap --test keeper_update_proof --test pty_channel --test keeper_lifecycle --test keeper_input_fidelity --test keeper_daemon --test output_survives_control_round_trip`
- `/tmp/wcargo.sh test -p roost-worker --test keeper_update_action --test keeper_maintenance_admission --test keeper_update_preservation --test keeper_update_prepare --test keeper_update_terminal_freeze --test link_downstream_keeper_update --test link_downstream_absent --test link_downstream_terminal --test retire_support`
  (retire_support is a support dir; run the tests that include it: grep `retire_support` in tests/)
- clippy: `/tmp/wcargo.sh clippy -p roost-worker -p roost-keeper --all-targets -- -D warnings`
- The keeper tests need `/bin/bash`, `pgrep`, `ps` (present on this host).

v2 → Rust test map:
| v2 | Rust |
|---|---|
| apps/worker/tests/keeper-update-action.test.ts (8) | crates/roost-worker/tests/keeper_update_action.rs (8) |
| apps/worker/tests/keeper-maintenance-admission.test.ts (7) | crates/roost-worker/tests/keeper_maintenance_admission.rs (7) |
| apps/worker/tests/keeper-update-terminal-freeze.test.ts (2) | crates/roost-worker/tests/keeper_update_terminal_freeze.rs (2) |
| apps/worker/tests/keeper-child-reap.test.ts (3) | crates/roost-keeper/tests/keeper_child_reap.rs (3) |
| apps/worker/tests/keeper-force-live-refresh.test.ts (1, real keeper) | keeper half: crates/roost-keeper/tests/keeper_update_proof.rs `a_forced_shutdown_ends_the_keeper_and_every_pty_it_held`; refusal half: keeper_maintenance_admission.rs `refuses_a_keeper_holding_live_channels_without_force_live` (a worker test cannot start the keeper binary: CARGO_BIN_EXE is per package) |
| apps/worker/tests/session/session-channel-creation-gate.test.ts test 3 | keeper_update_prepare.rs `rejects_a_coordinator_session_set_that_differs_from_live_worker_state`; tests 1&2 split per §3 |
| v2 `bun run test:upgrade` keeper preservation | keeper_update_preservation.rs + keeper_update_proof.rs `the_hello_proves_one_keeper_identity_and_its_channels_across_reconnects` |

Things to verify at first run (unverified assumptions in drafts): `terminal_stream_support::Harness::scripted` starts
with no installed stream (`terminal_stream_facts(channel()).is_none()` before the first request); the default
`session_support` ScriptedKeeper acks input so `write_terminal_input` answers `Accepted { written_bytes }`
(`written_bytes` type — adjust the `as u32` cast to the real field type); `WorkerStreamResult::Rejected { reason, .. }`
reason equals `KEEPER_UPDATE_WRITE_REFUSAL` for a stream refused by the lane (session/terminal_control.rs:81); bash's
`--norc -i` prompt contains `"$ "`.

## 6. Mutations to perform (each: mutate, run, watch fail, revert; record file:line)

1. process_reap.rs `reap_channel_tree`: remove the `std::thread::Builder…spawn(… sweep_survivors …)` → expect
   keeper_child_reap `a_nohup_job_that_ignores_sighup_is_still_reaped` to fail.
2. process_reap.rs `reap_channel_tree`: remove `signal_group(target.leader, libc::SIGTERM)` AND the sweep → expect
   `an_interactive_shell_and_its_foreground_and_background_jobs_all_die` to fail (SIGHUP alone kills an interactive
   bash, so drop hang_up's fg-group signal too if the first variant still passes; record what was observed).
3. bin/roost-keeper.rs: delete the `reap_all_channels()` call → keeper_update_proof `a_forced_shutdown_ends_the_keeper_and_every_pty_it_held` fails (child ignores SIGHUP).
4. keeper_ops.rs hello: `process_epoch: None` → keeper_update_proof identity test and keeper_update_preservation fail.
5. update_admission.rs preserve branch: drop `|| (target && !admitted_identity)` → keeper_update_action `preserve_rejects_changed_epoch_proof_without_invoking_shutdown` fails.
6. update_admission.rs: drop the `keeper_channels != action.worker_open_channel_ids` check → `preserve_rejects_a_keeper_channel_absent_from_the_worker_session_map` fails.
7. keeper_shutdown.rs `wait_for_exit`: return true after the first sleep → `fails_only_after_the_full_exit_confirmation_budget` fails.
8. update_prepare.rs: drop the `coordinator_ids != worker_ids` check → `rejects_a_coordinator_session_set_that_differs_from_live_worker_state` fails.
9. update_prepare.rs: call `rollback_admission.rollback()` also on `Prepared::Journaled` → `a_journaled_preparation_that_succeeds_stays_closed_and_holds_the_boundary` fails.
10. downstream/mod.rs: restore the absent-owner arm → link_downstream_keeper_update tests fail.

## 7. Consumers of every new value (for the report)

- `KeeperObservation.keeper_pid/process_epoch` → `keeper_pool::read_runtime_probe` → admission proof, heartbeat
  `keeper_runtime` (WHeart), WResume's keeper_boot/keeper_probe.
- `KeeperRuntimeProbe::observation` → WHeart heartbeat sources. `probe_runtime` → `PoolKeeperHost::probe`, heartbeat.
- `EmptyKeeperShutdownExpectation`, `shutdown_*_on`, `wait_for_keeper_exit` → update_host.rs, WResume keeper_boot.rs.
- `KeeperUpdateActionResult` → update_prepare.rs → rpc-ok data → coord `deploy/keeper_update/identity.rs::verify_worker_result`.
- `UpdateDirection`, `JournaledKeeperUpdateActionV1` → update_prepare.rs → HostKeeperUpdateActions::apply.
- `KeeperUpdatePort` / `DownstreamOwners.keeper_update` → runtime/downstream/keeper_update.rs.
- `ReapTarget`, `reap_channel_tree`, `reap_all_channels` → pty_channel.rs kill, keeper.rs reap_all_channels, bin.
- `terminate_process` → WResume runtime/keeper_prepare.rs (degraded-keeper restart).
- `PtyChannel::reap_target` → `Keeper::reap_all_channels`.

## 8. Parity gaps / notes for the report

- Hangup is signal-emulated (SIGHUP+SIGCONT leader, SIGHUP fg pgrp) rather than a master close (fd dups held by the
  reader/input threads). Births are read after the group SIGTERM, as v2 does (race-only imprecision kept for parity).
- v2 `reapPosixChannel` closes the terminal + SIGTERMs the group twice (duplicate lines); ported once (idempotent).
- `ps` is spawned from the keeper's serving thread with a 2 s bound, as v2's spawnSync.
- The keeper-maintenance test's warn-log assertion is not ported (roost-worker has no log-capture facility).
- v2's `bun_abi` contract field has no Rust counterpart (already documented in roost-protocol contract.rs).
- The admission probe runs `Hello` + `ListChannels` on the pool connection; a disconnect during a wait surfaces as
  `ClientError::SpawnNotAcknowledged` (client's own mapping), i.e. reachable-but-unproven, not absent.
- Docs: `docs/v3-handoff/worker-v2-map.md` rows `update-admission.ts`, `keeper-process-reap.ts`,
  `coord-link-keeper-update.ts`, `keeper-probe.ts` (admin shutdown) flip to PORTED with the new paths; the lead owns
  that file — list the rows in the report.

## 9. Commit message draft

```
worker: keeper-update admission, keeperUpdatePrepare owner, keeper tree reap

The coordinator's keeperUpdatePrepare had only the absent-owner refusal, so a
deploy could never prove or preserve a live keeper, and a killed PTY leaked its
job-control groups. Ports v2 keeper/update-admission.ts,
transport/coord-link-keeper-update.ts, keeper-probe.ts (administrative
shutdown, exit wait), keeper-process-reap.ts (POSIX) and the keeper's
process epoch (multiplexed-main.ts). The keeper Hello now proves its pid and a
per-process uuid epoch; proofs and shutdowns go over the pool's own connection
because the keeper serves one connection at a time. A killed channel hangs up
its terminal, SIGTERMs the leader's group and SIGKILLs every survivor of a
pre-signal process-tree snapshot after 2 s; every keeper stop reaps all trees.
Mutations: <fill from §6 as observed>. Consumers: coord verify_worker_result
(rpc-ok data), heartbeat keeper_runtime (probe_runtime/observation), boot
survivor retirement (shutdown_*_on, wait_for_keeper_exit).
```
Path list: the 17 new files in §1 + K1–K8 + W1–W7 (+ boot_sequence.rs for the W4 argument).
