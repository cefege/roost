worker: heartbeat, folder facts, stray reap and channel-creation gate reach v2 parity

Ports v2 apps/worker/src/transport/heartbeat.ts (+ the heartbeat half of
transport/coord-client.ts), host/{host-sample-linux,host-sample-darwin,
git-branch,listening-ports,pr-status}.ts, packages/host/src/tailnet.ts,
session/session-git-ports.ts, session/session-channel-creation-gate.ts +
session-manager.ts:44-74, and the stray reap of session-lifecycle.ts
(reapStrayKeeperChannels) + session-manager-state.ts (stray timer).

Behaviour: heartbeats go out 30 s after the previous one settles (10 s RPC
deadline, `heartbeat.stalled` on the 3rd consecutive miss) and carry exactly
v2's fields: host_metrics (60 s cached resample, last good kept on failure),
git_sha (omitted for dev builds), os, host_identity, keeper_runtime (only after
a reconciliation, dropped if it changed mid-beat; proof from
KeeperPool::probe_runtime), reachable_addr (tailnet MagicDNS, 5 min cache,
ROOST_REACHABLE_ADDR fallback), terminal_core_capacity. Git branch/remote, PR
and listening ports are NOT heartbeat fields in v2: they are git/pr/ports
session events from the per-session folder watcher, restarted on spawn,
respawn, adoption and after every OSC 7 cwd event. Strays are reaped on the
second consecutive sighting (strike dropped on reap). Spawn and respawn hold a
creation lease; keeper-update preparation closes admission synchronously,
freezes terminal writes and waits for leases in flight.

Fixes found while porting: git/gh were run with a PATH string as the program
(never ran); /proc/net/dev only read its first colon line (no bandwidth ever);
macOS CPU always 0 and memory 0 (vm_stat's trailing "."); ss hardcoded at
/usr/sbin/ss (absent on Debian/Ubuntu); a host tool whose stdout filled the
pipe hung until its timeout; StrayTracker had a closed_at exemption v2 lacks.

Mutations (each seen to fail, then reverted):
- heartbeat.rs:174-175 next sleep set before the attempt -> never_overlaps_calls_and_schedules_the_next_attempt_from_settlement
- heartbeat.rs:251 mid-beat reconciliation check removed -> drops_an_observation_whose_reconciliation_was_superseded_mid_beat
- heartbeat.rs:198 last good metrics dropped -> retains_the_last_good_metrics_when_collection_is_unknown
- heartbeat.rs:216 miss count not reset on success: survived the v2-ported asserts (the signal
  cooldown hides it); new phase in counts_one_miss_per_settlement_resets_on_success_and_isolates_instances
  now FAILS under it (resume batch, below).
- heartbeat_metrics.rs:77 cache gate disabled -> a_sample_is_reused_for_a_minute_and_then_replaced
- heartbeat_metrics.rs:96-98 bandwidth baseline never updated -> bandwidth_is_the_rate_between_the_last_two_real_samples
- channel_creation_gate.rs:73 admission ignores preparations -> preparation_drains_an_admitted_spawn_rejects_a_new_spawn_and_stays_closed_on_success
- channel_creation_gate.rs:104 preparation does not wait for leases -> a_failed_admitted_respawn_drains_before_preparation_and_rollback_reopens_admission (+ test 1)
- channel_creation_gate.rs:145 rollback not idempotent -> a_rollback_releases_only_its_own_preparation_once
- channel_creation_gate.rs:92 write freeze never raised -> a_rollback_releases_only_its_own_preparation_once
- respawn.rs:152 open_shell lease dropped at once -> preparation_drains_an_admitted_spawn_rejects_a_new_spawn_and_stays_closed_on_success
- strays.rs:114 strike kept on reap -> a_reaped_channel_still_listed_starts_its_strikes_over
- strays.rs:110 reap on first sighting -> a_just_spawned_channel_is_never_reaped_on_its_first_sighting
- stray_reap.rs:66 nothing counted as tracked -> a_stray_dies_after_two_sweeps_a_tracked_channel_never_does_and_a_later_sweep_is_a_no_op
- stray_reap.rs:108 maintenance restarted on every call -> the_periodic_sweep_starts_once_and_reaps_on_its_second_interval
- git_ports.rs:127 record-generation guard removed -> a_reading_for_a_channel_now_held_by_another_session_is_dropped
- git_ports.rs:134 raw branch compare -> a_changed_branch_publishes_one_git_event_and_a_repeat_publishes_none
- samples.rs:226-227 first colon line only -> every_interface_row_of_proc_net_dev_yields_counters
- pr_status.rs:178 conclusion without `?? state` -> an_entry_without_a_conclusion_is_judged_by_its_state

Consumers: HeartOwners (runtime::owners) starts the heartbeat after readiness
(boot_sequence) and stops it first on shutdown; KeeperReconciliation is written
by runtime::reconcile_gate and read by the heartbeat; StraySweeper is driven by
runtime::session_reconcile (start_post_admission_maintenance, reap) and its own
60 s timer; SessionFolderHooks are fired by session::respawn (open_under),
session::respawn_replace, session::resume (adopt_survivor) and
session::cwd_events, and consumed by session::git_ports; ChannelCreationGate /
PreparationRollback / keeper_update_prepared() are consumed by respawn leases
and the keeper-update preparer; tool_path::run_on_path/run_bounded by ports,
pr_status, samples, samples_darwin and tailnet.

Downstream arms: none replaced (W-HEART adds no link arm).
Cross-owner/shared edits: runtime/bootstrap_redeem/mod.rs (AUTHORIZATION,
build_sha -> pub(crate)); tests/session_support/mod.rs (Harness::with_spawner);
mod lines in host/, session/, runtime/; SessionManager fields in
session/lifecycle.rs; one-line hooks in session/{respawn,resume,cwd_events}.rs;
runtime/{owners,boot_sequence}.rs wiring. No Cargo.toml/Cargo.lock change
(tokio test-util was already in the dev-dependencies).

Mutation batch at resume (WorkerLead; mutbatch.py: co-applied mutants never share a file
or a test binary; KILLED = the named guard failed under the mutant; every file restored):
- M4 roost-worker runtime/heartbeat.rs: `self.consecutive_misses = 0;` -> `` => FAILS counts_one_miss_per_settlement_resets_on_success_and_isolates_instances

Red classified at resume: heartbeat_support's OBSERVATION fixture lacked running_contract.bun_abi
(restored by b8cb104c); TEST defect, fixed with v2's literal (heartbeat.test.ts:212 "1.3.14").

Series: one of the wave 2+3 per-slice commits (ProtoDoor..WAgentsPrompt). A file shared by
several slices lands whole in the FIRST slice commit that lists it, so intermediate commits
may not build; the track gate (tests x2, clippy, lint, fmt) ran on the series tip.
