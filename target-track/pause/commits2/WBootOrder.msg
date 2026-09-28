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

Mutations: target-track/wdurable/MUTATIONS.md (W1-W4, R1-R2); each
made its named test fail and was reverted.

Consumers: SnapshotActivation -> runtime::boot_admission (activate) and
link_loop::durable_sync/link_serve (held checks); PassAdmission ->
ReconcileGate::run; OpenSessionSet -> SessionReconciler::pass, admit_keeper;
WorkerBoot.agent_conversation_restore -> WorkerConversationRestorer;
ReconcileInputs.platform -> WorkerConversationRestorer; LinkDisposed ->
ReconcileGate::run.

Lands after WResume, WAgentsPrompt and WDurable (shares runtime/owners.rs,
runtime/mod.rs, runtime/boot_sequence.rs and the reconcile files with WResume).
