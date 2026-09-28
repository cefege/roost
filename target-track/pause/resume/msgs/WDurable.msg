worker: durable session events reach the coordinator; the snapshot is a sequenced Event frame

Ports v2 apps/worker/src/transport/event-sink.ts coordLinkSink,
coord-link-unacked.ts (pump, oldestDurable, hasBlockingSessionEventReservation,
startSnapshotBarrier's store.nextClientSeq(), snapshotStateChanged) and
coord-link-replay-barrier.ts.

Before this, rows the session layer wrote through session::journal_sink went
into the SQLite outbox and nothing offered them to the link, and the snapshot
went out as raw JSON under a pump-local sequence the coordinator could not
acknowledge (and which a later journal row could reuse). Now JournalSink raises
DurableDelivery::note_store_changed on every append/reserve/hold/release; the
link loop (runtime::link_loop::durable_sync) offers every row beyond what the
pump holds under its stored client_seq, including a restart's unacknowledged
rows, holds the snapshot while a claim blocks it, and frames the snapshot as
CoordWorkerUp Event{snapshot, client_seq} with the sequence drawn from the
Journal (Journal::snapshot_sequence) only while no committed row is left
unoffered. Appends hold the sequence window through commit, so sequence order
is commit order. DurableDelivery::wait_for_replay is the durable-replay
barrier the boot reconcile awaits before reading recovery metadata.

attach_durable_outbox no longer seeds the pump past handed_over_at (that made a
restart's unacked rows AlreadyPassed); Pump::seed_next_sequence and the unused
note_sequence_used are removed.

v2 `send`/`snapshotStateChanged`: a row or blocking claim that appears while
the snapshot is wanted but not yet written sends the barrier back to replay
(durable_sync reopen_replay_before_unsent_snapshot), and Pump::abandon_snapshot
gives back its own unsent sequence draw so the outbox's next row is not
refused as AlreadyPassed. Boot may hold the snapshot
(LinkLoop::hold_snapshot_until_activated -> SnapshotActivation, v2
activateSnapshotProvider): the replay drains at the snapshot stage, nothing
publishes and the starvation bound does not run until activation wakes the
link. The link's run disposes the replay barrier on its way out (v2 dispose),
so a waiter is refused (LinkDisposed) rather than left waiting.

Test fix: the loopback snapshot double (tests/link_downstream_support/live.rs)
was stamped ts 0, which the proto decode refuses (session_event.snapshot.ts
must be >= 1; v2 stamps Date.now()), so every go_live test panicked.

Mutations (slice run2/run3, each mutated, run, reverted):
- B1 link_drain.rs skip sync_durable_rows -> durable_delivery::sink_rows_reach_the_coordinator_before_the_snapshot_and_while_live FAILED (guard holds)
- B2 durable_sync.rs skip reassign_snapshot_sequence -> not caught by the_snapshot_and_a_journal_row_never_share_a_sequence (pump own draw coincides with the journal draw within one process); new guard a_restarted_link_numbers_its_snapshot_above_the_previous_process verified in run3 (see run3.log)
- B3 durable.rs attach without durable_resync -> equivalent mutant: authorise_snapshot's journal draw refuses while rows are unoffered and resumes replay, so restart rows still precede the snapshot (one extra draw + log line)
- B4 durable_sync.rs decide ignores blocking -> the_replay_barrier_waits_for_acked_rows_and_unblocked_claims FAILED
- B5 journal_sink.rs emit without note_store_changed -> caught by a_held_snapshot_drains... and the_replay_barrier_waits... (not the named sink_rows test)
- W5 durable_sync.rs hold check off -> a_held_snapshot_drains_the_replay_and_publishes_only_once_activated FAILED
- W6 durable_sync.rs reopen off -> same FAILED
- W7 link_barrier.rs abandon give-back off -> same FAILED
- B2 re-run (run3): new guard a_restarted_link_numbers_its_snapshot_above_the_previous_process FAILED (guard holds).

Series: one of the wave 2+3 per-slice commits (ProtoDoor..WAgentsPrompt). A file shared by
several slices lands whole in the FIRST slice commit that lists it, so intermediate commits
may not build; the track gate (tests x2, clippy, lint, fmt) ran on the series tip.
