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

Mutations: target-track/wdurable/MUTATIONS.md (B1-B5, W5-W7).
