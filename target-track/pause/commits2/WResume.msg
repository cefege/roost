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

Mutations: see the WResume P2 report.
