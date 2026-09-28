WDurableP3 mutation verdicts (run2, target-track/wdurable/mutations.log; script wdurable/mutate.py)
B1 link_drain.rs skip sync_durable_rows -> durable_delivery::sink_rows_reach_the_coordinator_before_the_snapshot_and_while_live FAILED (guard holds)
B2 durable_sync.rs skip reassign_snapshot_sequence -> not caught by the_snapshot_and_a_journal_row_never_share_a_sequence (pump own draw coincides with the journal draw within one process); new guard a_restarted_link_numbers_its_snapshot_above_the_previous_process verified in run3 (see run3.log)
B3 durable.rs attach without durable_resync -> equivalent mutant: authorise_snapshot's journal draw refuses while rows are unoffered and resumes replay, so restart rows still precede the snapshot (one extra draw + log line)
B4 durable_sync.rs decide ignores blocking -> the_replay_barrier_waits_for_acked_rows_and_unblocked_claims FAILED
B5 journal_sink.rs emit without note_store_changed -> caught by a_held_snapshot_drains... and the_replay_barrier_waits... (not the named sink_rows test)
W1 reconcile_gate.rs skip wait_for_replay -> boot_admission::the_boot_pass_reads_only_after_the_replay_... FAILED
W2 boot_admission.rs activate before reconcile -> same test FAILED
W3 reconcile_gate.rs fresh reference gate -> a_reference_reporter_queued_during_a_pass_enters_only_after_it FAILED
W4 durable_delivery.rs wait ignores disposed -> a_disposed_link_refuses_the_boot_pass_without_a_read FAILED
W5 durable_sync.rs hold check off -> a_held_snapshot_drains_the_replay_and_publishes_only_once_activated FAILED
W6 durable_sync.rs reopen off -> same FAILED
W7 link_barrier.rs abandon give-back off -> same FAILED
R1 reconcile_restore.rs skip adopted claim -> an_adopted_sessions_reference_is_claimed_before_any_respawn_restore FAILED
R2 reconcile_restore.rs no catch_unwind -> an_unexpected_restore_failure_never_escapes_the_pass FAILED
A1-A10 (WIRING §A14) -> every named test FAILED (guard holds)
