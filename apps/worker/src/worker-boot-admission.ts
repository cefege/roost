// Admission order shared by worker boot and focused reconcile tests.
// Reconciliation must reserve every durable session before snapshots publish,
// so a failed keeper adoption cannot expose a partial worker state.

import type { ReconcileAdmissionOutcome, ReconcileAdmissionSuccess } from "./boot-reconcile.ts";

export async function completeWorkerBootAdmission(deps: {
  reconcile: () => Promise<ReconcileAdmissionOutcome>;
  activateSnapshotProvider: () => void;
  markReady: () => void;
}): Promise<ReconcileAdmissionSuccess> {
  const outcome = await deps.reconcile();
  if (!outcome.admitted) throw outcome.error;
  deps.activateSnapshotProvider();
  deps.markReady();
  return outcome;
}
