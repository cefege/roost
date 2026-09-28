worker: keeper-update admission, keeperUpdatePrepare owner, keeper tree reap

The coordinator's keeperUpdatePrepare had only the absent-owner refusal, so a
deploy could never prove or preserve a live keeper, and a killed PTY leaked its
job-control groups. Ports v2 keeper/update-admission.ts,
transport/coord-link-keeper-update.ts, keeper-probe.ts (administrative
shutdown, exit wait, proof fields), keeper-process-reap.ts (POSIX), the
keeper's processEpoch and shutdown reapAllChannels (multiplexed-main.ts), and
heartbeat.ts observeKeeperRuntime (KeeperRuntimeProbe::observation).

The keeper Hello now proves its pid and a per-process uuid epoch
(KeeperObservation.keeper_pid/process_epoch, serde-default). Proofs and
shutdowns go over the pool's own connection because the keeper serves one
connection at a time. A killed channel hangs up its terminal, SIGTERMs the
leader's group and SIGKILLs every survivor of a pre-signal process-tree
snapshot after 2 s; every keeper stop reason reaps all trees first.

Replaced arm: keeper-update-prepare. Old: rpc-error "keeper update preparation
unsupported by this worker" (kept as the owner-less answer). New owner:
keeper_pool::KeeperUpdatePreparer via DownstreamOwners.keeper_update, answering
rpc-ok {outcome[, keeper_pid, keeper_epoch, binding_digest]} or rpc-error.
The preparer closes admission through SessionManager's channel-creation gate
and holds runtime::reconcile_gate::ReconcileGate as its reconcile boundary.

Consumers: coord deploy/keeper_update/identity.rs verify_worker_result (rpc-ok
data); heartbeat keeper_runtime (KeeperPool::probe_runtime + observation);
runtime keeper_prepare/keeper_retire (shutdown_*_on, wait_for_keeper_exit,
read_runtime_probe, roost_keeper::process_reap::terminate_process).

Cross-owner: roost-keeper lib.rs, payloads.rs, keeper.rs, keeper_ops.rs,
pty_channel.rs, bin/roost-keeper.rs, tests/support/daemon.rs,
tests/output_survives_control_round_trip.rs.

Mutations (each applied under the build lock, run, reverted):
- update_admission.rs preserve: drop `|| (target && !admitted_identity)` ->
  keeper_update_action preserve_rejects_changed_epoch_proof_without_invoking_shutdown FAILED.
- keeper_shutdown.rs wait_for_exit: return true after the first poll sleep ->
  keeper_maintenance_admission fails_only_after_the_full_exit_confirmation_budget and
  accepts_an_exit_that_takes_longer_than_two_seconds FAILED.
- update_prepare.rs: `if false && coordinator_ids != worker_ids` ->
  keeper_update_prepare rejects_a_coordinator_session_set_that_differs_from_live_worker_state FAILED.
- update_prepare.rs: acquire the boundary before awaiting admission ->
  keeper_update_prepare_drain (both) + a_journaled_preparation_that_succeeds_stays_closed_and_holds_the_boundary,
  preparations_run_one_at_a_time_in_arrival_order FAILED.
- downstream/mod.rs: restore the absent-owner arm -> link_downstream_keeper_update (3) FAILED.
- process_reap.rs: skip the SIGKILL sweep -> keeper_child_reap stayed green (the group SIGTERM
  reached the nohup job); the test now makes the job ignore SIGTERM too (v2's "escalation
  SIGKILL path"), re-mutation queued (log /tmp/wkupdate-mut-M1b.log).
