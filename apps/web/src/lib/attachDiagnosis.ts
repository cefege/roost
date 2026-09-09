// Public attach-stall diagnosis registration for terminal loading cards.
// One lazy document scheduler batches bounded coordinator diagnostics.
// The reason mapper remains exported for focused wire-shape tests.

import { coordClient } from "../connect.ts";
import type { BaselineProgress } from "../store/terminal-stream-types.ts";
import {
  ATTACH_DIAGNOSIS_BATCH_INTERVAL_MS,
  ATTACH_DIAGNOSIS_BATCH_MAX_SESSIONS,
  createAttachDiagnosisScheduler,
  type AttachDiagnosisHandle,
  type AttachDiagnosisScheduler,
} from "./attachDiagnosisScheduler.ts";

export {
  ATTACH_DIAGNOSIS_BATCH_INTERVAL_MS,
  ATTACH_DIAGNOSIS_BATCH_MAX_SESSIONS,
};
export type { AttachDiagnosisHandle };
export {
  attachDiagnosisReasonFromSnapshot,
  type AttachDiagnosisReasonContext,
  type AttachDiagnosisReasonOutcome,
} from "./attachDiagnosisReason.ts";

/** Identifies the loading/progress state that must restart diagnosis grace. */
export function attachDiagnosisWaitKey(
  stage: string | null,
  progress: BaselineProgress | null,
): string | null {
  if (stage !== "viewport" && stage !== "frame") return null;
  if (progress === null) return `${stage}\u0000`;
  return `${stage}\u0000${progress.snapshotId}\u0000${progress.receivedChunks}\u0000${progress.totalChunks}`;
}

let documentScheduler: AttachDiagnosisScheduler | null = null;

/** Register a loading card for best-effort, document-batched diagnosis. */
export function startAttachDiagnosis(
  sessionId: string,
  onReason: (reason: string | null) => void,
): AttachDiagnosisHandle {
  if (documentScheduler === null) {
    documentScheduler = createAttachDiagnosisScheduler(requestAttachDiagnosisSnapshots);
  }
  return documentScheduler.register(sessionId, onReason);
}

/** Deterministic test reset for the document-owned scheduler. */
export function _resetAttachDiagnosisSchedulerForTest(): void {
  documentScheduler?.dispose();
  documentScheduler = null;
}

function requestAttachDiagnosisSnapshots(
  sessionIds: readonly string[],
  signal: AbortSignal,
) {
  return coordClient.diagSnapshot({ sessionFilterIds: [...sessionIds] }, { signal });
}
