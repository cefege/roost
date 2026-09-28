//! Boot's admission phase, run while the coordinator link already dials and
//! replays: the first reconcile pass (which waits for the durable replay before
//! it reads the coordinator), then snapshot activation, then readiness, in that
//! order and no other. Ports `apps/worker/src/boot/worker-boot-admission.ts`
//! (`completeWorkerBootAdmission`) and the provider activation of v2
//! `main.ts:296-303`. Called by `runtime::boot_sequence` once.

use super::boot_order::{BootSequence, Readiness, ReadyStep, StepId};
use super::reconcile_gate::ReconcileGate;
use super::session_reconcile::ReconcileSummary;
use super::snapshot_source::SnapshotActivation;

/// Reconcile, activate the snapshot, mark ready. A refused pass is a boot
/// refusal and leaves the snapshot held: the snapshot describes the session
/// set the pass reserved, so publishing it first would show the coordinator a
/// set it has not confirmed, and acting on that closes live sessions.
pub async fn complete_worker_boot_admission(
    reconcile: &ReconcileGate,
    snapshot: &SnapshotActivation,
    sequence: &mut BootSequence,
) -> anyhow::Result<(ReconcileSummary, Readiness)> {
    let summary = reconcile
        .reconcile_open_sessions("boot")
        .await
        .map_err(|failure| anyhow::anyhow!("boot refused: {failure}"))?;
    tracing::info!(
        ?summary,
        "boot: the coordinator's open sessions were reconciled"
    );
    let because = sequence
        .complete(StepId::SessionReconcile)
        .map_err(|refusal| anyhow::anyhow!("boot refused: {refusal}"))?;
    tracing::info!(
        step = StepId::SessionReconcile.name(),
        because,
        "boot: the local session set is reconciled and reserved"
    );
    // `Readiness::advance` refuses any other order rather than trusting this
    // function, and an advance that fails is a boot refusal for that reason.
    let mut readiness = Readiness::default();
    readiness
        .advance(ReadyStep::Reconciled)
        .map_err(|refusal| anyhow::anyhow!("boot refused: {refusal}"))?;
    snapshot.activate();
    readiness
        .advance(ReadyStep::SnapshotProviderActivated)
        .map_err(|refusal| anyhow::anyhow!("boot refused: {refusal}"))?;
    readiness
        .advance(ReadyStep::MarkedReady)
        .map_err(|refusal| anyhow::anyhow!("boot refused: {refusal}"))?;
    let because = sequence
        .complete(StepId::Ready)
        .map_err(|refusal| anyhow::anyhow!("boot refused: {refusal}"))?;
    tracing::info!(
        step = StepId::Ready.name(),
        because,
        readiness = ?readiness,
        "boot: this worker is ready, and readiness is a claim about every step above it"
    );
    Ok((summary, readiness))
}
