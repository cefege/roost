worker: survivors adopt from keeper history; respawn, keeper death and retirement follow v2

Keeper (cross-owner, roost-keeper): the history is v2's ring+marker model and
codec — 1 MiB raw ring, head = every emitted byte, resize markers at the byte
they took effect, an evicted marker becomes the base geometry, a full marker
budget discards the window (keeper-history.ts); GetHistoryRecordsResp carries
[version][head][base cols/rows][count] + tagged records (protocol-terminal.ts
196-315); GetHistory answers GetHistoryResp [head][ring]; an unknown channel
answers the empty default. The legacy Resize no longer moves the applied
sequence and records marker seq 0 (keeper-frame-handler.ts:350-378). The client
reads history at an exact ordered boundary: pre-answer PtyOut of the channel,
including previously deferred frames, is dropped (client_history.rs), an
unreadable answer is ClientError::Protocol, and poll_event reports a closed
connection.

Worker:
- keeper_pool: reattach + history under the routing lock so the binding gets
  exactly the bytes after the head (keeper-pool-channels.ts); a closed
  connection is a keeper death — every channel gets on_exit(None) once and the
  death hook fires (keeper-pool-lifecycle.ts, multiplexed-client.ts);
  reconnect; PendingResizes (keeper-pool-io.ts) refuses a duplicate in-flight
  seq and feeds KEEPER_RESIZE_PENDING evidence (terminal-pipeline-snapshot.ts).
- session: adopt_survivor follows session-resume.ts (reattach at the boundary,
  fresh stream id, kill + recently-closed + tombstone on post-reattach failure);
  respawn_session / respawn_lost_child follow session-respawn*.ts (held record
  replaced under the same session id, its PTY killed only after the durable
  respawned — no leaked PTY); live exits close the session (closedByKeeper,
  session-emit.ts:316-322); orphan-output burst + dead births fire the degraded
  hook (session-emit.ts:88-113); spawn claims are released on EVERY failure
  when the spawn owns them (early refusals used to leak).
- runtime: ReconcileGate (boot-reconcile.ts) serializes boot/keeper-death/
  degraded passes, holds the keeper-update boundary, restarts a degraded keeper
  after 90 s grace within 2 restarts / 5 min; SessionReconciler
  (boot-session-reconcile.ts) reserves every outcome first, prepares the keeper,
  adopts or respawns with bounded transient retries, reaps strays, and a
  durability loss stops the worker. boot_keeper::admit adopts any compatible
  keeper (boot-keeper.ts:94-141) — refusing off-target keepers bricked boot on
  every keeper upgrade; ForceLiveRetire and ReplaceEmpty shut the old keeper
  down on the probe connection, wait for exit, clean the endpoint, start fresh.
  runtime/adoption*.rs deleted (replaced by the gate).

Consumers: ReconcileGate — boot_sequence step 9, KeeperPool death hook,
KeeperHealth degraded hook, KeeperUpdatePreparer (KeeperUpdateBoundary);
SessionOutcome::Live — respawn-if-missing reply; pending_resize_starts —
terminal_pipeline; StopReason::DurabilityLost — link loop stop.

Mutation batch at resume (WorkerLead; mutbatch.py: co-applied mutants never share a file
or a test binary; KILLED = the named guard failed under the mutant; every file restored):
- K1 roost-keeper client_history.rs: `dropped += frame.payload.len();` -> `self.defer(frame);` => FAILS pre_boundary_output_is_dropped_exactly_once
- K2 roost-keeper channel_history.rs: `self.base_cols = evicted.cols; self.base_rows = evicted.rows;` -> `` => FAILS an_evicted_marker_becomes_the_base_geometry
- W1 roost-worker keeper_pool/pool_lifecycle.rs: `channel.output().on_exit(None);` -> `channel.output().on_error(reason.clone());` => FAILS a_lost_keeper_ends_every_channel_exactly_once
- W2 roost-worker session/respawn_replace.rs: `if let Err(fault) = self.keeper.kill_channel(raw) {` -> `if let Err(fault) = Ok::<(), super::keeper_channels::KeeperFault>((...` => FAILS respawning_a_held_session_kills_the_pty_it_replaces
- W3 roost-worker boot_keeper.rs: `if probe.authenticated && probe.protocol_compatible {` -> `if probe.authenticated && probe.protocol_compatible && probe.exact_...` => FAILS a_compatible_survivor_is_adopted_even_off_the_exact_target
- W4 roost-worker runtime/reconcile_gate.rs: `if state.keeper_restarts.len() >= KEEPER_RESTART_BUDGET {` -> `if state.keeper_restarts.len() > KEEPER_RESTART_BUDGET {` => FAILS a_degraded_keeper_restarts_only_past_the_grace_window_and_within_budget
- W5 roost-worker terminal_pipeline/mod.rs: `facts.resize_frames += 1;` -> `` => FAILS keeper_facts_count_and_age_each_pending_resize
- W6 roost-worker session/resume.rs: `if let Err(fault) = self.keeper.kill_channel(channel) {` -> `if let Err(fault) = Ok::<(), super::keeper_channels::KeeperFault>((...` => FAILS a_forced_post_reattach_rejection_kills_the_adopted_keeper_channel, a_channel_list_that_cannot_be_read_refuses_with_the_keeper_s_reason …
- K3 roost-keeper keeper_ops.rs: `if request.seq > before && state.applied_seq == request.seq {` -> `if false {` => FAILS the_ordered_history_is_the_resize_the_keeper_recorded

Reds classified against v2 at resume:
- boot_adoption_gate respawn/adopt tests: TEST defects — session ids must be UUIDs (v2
  session-spawn.ts:53 randomUUID) and the original pool must outlive the survivor's marker
  (v2 keeper-survivor-continuity.test.ts:274-288).
- session_adoption channel-list refusal: TEST defect — v2 session-resume.ts:73 lists inside
  the try, and the catch at :303-320 kills, marks closed and tombstones; asserts now match.
- session_adoption non-converging replay: the refusal text is v2's word for word
  (session-resume.ts:212-217); the test's message-wording assert was deleted.
- session_spawn refused geometry: TEST defect — v2 session-spawn.ts:143-146 releases both
  owned claims; renamed a_refused_geometry_opens_no_pty_and_releases_both_claims.
- PRODUCT: spawn.rs double-released claims on the opened-emit failure path (spawn_shell
  already releases under ClaimsOnFailure::Release; under Keep the caller owns them); v2
  session-spawn.ts:144-145 releases once.

Series: one of the wave 2+3 per-slice commits (ProtoDoor..WAgentsPrompt). A file shared by
several slices lands whole in the FIRST slice commit that lists it, so intermediate commits
may not build; the track gate (tests x2, clippy, lint, fmt) ran on the series tip.
