worker: boot dials before reconciling; every pass waits for the durable replay and restores agent conversations

Ports v2 apps/worker/src/boot/worker-boot-admission.ts
(completeWorkerBootAdmission), boot/boot-reconcile.ts:69-80 (runReconcile:
referenceAdmission.runExclusive around beforeRecoveryRead then the pass),
main.ts:173-190/262-303 (link started before setupReconcile; the snapshot
provider activated only after the boot pass), and the restore half of
boot/boot-session-reconcile.ts (:83-91 recovery metadata read inside every
pass, :191-196 adopted reference claim, :278-296 one restore after a durable
respawn, `resumedReferenceKeys` per pass).

Before this the Rust boot ran the whole reconcile before `link.run`, so the
pass read the coordinator's recovery state before this worker's durable rows
(agent references among them) had been replayed to it, and the boot pass used
rows read before the keeper admission. Now boot holds the link's snapshot
(LinkLoop::hold_snapshot_until_activated), spawns the link, and
runtime::boot_admission reconciles, activates the snapshot, then marks ready.
ReconcileGate::run holds the AgentReferenceAdmissionGate and awaits
DurableDelivery::wait_for_replay before SessionReconciler::pass reads
sessionsList + recovery metadata (runtime::reconcile::OpenSessionSet, checked
by assert_exact_recovery_metadata). The reconcile pass signature loses its
pre-read rows (v2 reconcileOpenSessions(reason)); the boot-admitted keeper's
survivor capacity check runs on the first pass that reaches it. A link that
stops for good disposes the replay barrier, refusing a waiting pass (v2
dispose). runtime::reconcile_restore holds the per-pass claim set and the
production restorer (ROOST_AGENT_CONVERSATION_RESTORE); a restore that panics
is logged ambiguous and never re-enters respawn or the tombstone. The agent
stack is built before the reconcile gate so both share one admission gate.

Mutations (slice run2, each mutated, run, reverted):
- W1 reconcile_gate.rs skip wait_for_replay -> boot_admission::the_boot_pass_reads_only_after_the_replay_... FAILED
- W2 boot_admission.rs activate before reconcile -> same test FAILED
- W3 reconcile_gate.rs fresh reference gate -> a_reference_reporter_queued_during_a_pass_enters_only_after_it FAILED
- W4 durable_delivery.rs wait ignores disposed -> a_disposed_link_refuses_the_boot_pass_without_a_read FAILED
- R1 reconcile_restore.rs skip adopted claim -> an_adopted_sessions_reference_is_claimed_before_any_respawn_restore FAILED
- R2 reconcile_restore.rs no catch_unwind -> an_unexpected_restore_failure_never_escapes_the_pass FAILED

Consumers: SnapshotActivation -> runtime::boot_admission (activate) and
link_loop::durable_sync/link_serve (held checks); PassAdmission ->
ReconcileGate::run; OpenSessionSet -> SessionReconciler::pass, admit_keeper;
WorkerBoot.agent_conversation_restore -> WorkerConversationRestorer;
ReconcileInputs.platform -> WorkerConversationRestorer; LinkDisposed ->
ReconcileGate::run.

Lands after WResume, WAgentsPrompt and WDurable (shares runtime/owners.rs,
runtime/mod.rs, runtime/boot_sequence.rs and the reconcile files with WResume).

Series: one of the wave 2+3 per-slice commits (ProtoDoor..WAgentsPrompt). A file shared by
several slices lands whole in the FIRST slice commit that lists it, so intermediate commits
may not build; the track gate (tests x2, clippy, lint, fmt) ran on the series tip.
