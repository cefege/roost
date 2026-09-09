// Owns the one document-wide cadence for attach-stall diagnosis requests.
// attachDiagnosis.ts registers loading cards after their grace window.
// The supplied transport returns one bounded snapshot for every admitted session.
// Canceled registrations never receive a stale response or keep a timer alive.

import { diag } from "@roost/shared/diag";
import {
  attachDiagnosisReasonFromSnapshot,
} from "./attachDiagnosisReason.ts";

export const ATTACH_DIAGNOSIS_BATCH_INTERVAL_MS = 5_000;
export const ATTACH_DIAGNOSIS_BATCH_MAX_SESSIONS = 64;

export interface AttachDiagnosisHandle {
  dispose(): void;
}

export interface AttachDiagnosisBatchResponse {
  snapshotJson: string;
}

export type AttachDiagnosisBatchRequester = (
  sessionIds: readonly string[],
  signal: AbortSignal,
) => Promise<AttachDiagnosisBatchResponse>;

export interface AttachDiagnosisScheduler {
  dispose(): void;
  register(sessionId: string, onReason: (reason: string | null) => void): AttachDiagnosisHandle;
}

interface AttachDiagnosisRegistration {
  onReason: (reason: string | null) => void;
}

interface AttachDiagnosisSession {
  hasVisibleReason: boolean;
  previousTerminalScreenSeq: string | null;
  reason: string | null;
  sessionId: string;
  subscribers: Set<AttachDiagnosisRegistration>;
}

interface AttachDiagnosisBatchTarget {
  session: AttachDiagnosisSession;
}

interface AttachDiagnosisBatch {
  controller: AbortController;
  targets: readonly AttachDiagnosisBatchTarget[];
}

/** Creates the document scheduler; production keeps one instance in attachDiagnosis.ts. */
export function createAttachDiagnosisScheduler(
  requestSnapshots: AttachDiagnosisBatchRequester,
): AttachDiagnosisScheduler {
  const sessions = new Map<string, AttachDiagnosisSession>();
  const readySessionRing = new Set<AttachDiagnosisSession>();
  let disposed = false;
  let inFlight: AttachDiagnosisBatch | null = null;
  let nextBatchAtMs = 0;
  let batchTimer: Timer | null = null;

  const clearBatchTimer = (): void => {
    clearTimeout(batchTimer ?? undefined);
    batchTimer = null;
  };
  const isLiveSession = (session: AttachDiagnosisSession): boolean =>
    sessions.get(session.sessionId) === session && session.subscribers.size > 0;
  const enqueue = (session: AttachDiagnosisSession): void => {
    if (!isLiveSession(session) || readySessionRing.has(session)) return;
    readySessionRing.add(session);
  };
  const takeBatch = (): AttachDiagnosisBatchTarget[] => {
    const targets: AttachDiagnosisBatchTarget[] = [];
    for (const session of readySessionRing) {
      readySessionRing.delete(session);
      if (isLiveSession(session)) targets.push({ session });
      if (targets.length === ATTACH_DIAGNOSIS_BATCH_MAX_SESSIONS) break;
    }
    return targets;
  };
  const cancelEmptyBatch = (): void => {
    const activeBatch = inFlight;
    if (activeBatch === null) return;
    for (const { session } of activeBatch.targets) {
      if (isLiveSession(session)) return;
    }
    activeBatch.controller.abort();
  };

  const notify = (session: AttachDiagnosisSession, reason: string | null): void => {
    for (const subscriber of [...session.subscribers]) {
      try {
        subscriber.onReason(reason);
      } catch {
        // Diagnosis display is advisory and must not interrupt terminal recovery.
      }
    }
  };
  const acceptSnapshot = (
    batch: AttachDiagnosisBatch,
    response: AttachDiagnosisBatchResponse,
  ): void => {
    let snapshot: unknown;
    try {
      snapshot = JSON.parse(response.snapshotJson);
    } catch {
      return;
    }
    for (const { session } of batch.targets) {
      if (!isLiveSession(session)) continue;
      const outcome = attachDiagnosisReasonFromSnapshot(snapshot, session.sessionId, {
        previousTerminalScreenSeq: session.previousTerminalScreenSeq,
      });
      session.previousTerminalScreenSeq = outcome.terminalScreenSeq;
      if (session.reason === outcome.reason) continue;
      session.hasVisibleReason = outcome.reason !== null;
      session.reason = outcome.reason;
      diag("attach.stuck_reason", { sid: session.sessionId, reason: outcome.reason });
      notify(session, outcome.reason);
    }
  };
  const armBatch = (): void => {
    if (disposed || inFlight !== null || sessions.size === 0 || batchTimer !== null) return;
    const delayMs = Math.max(0, nextBatchAtMs - Date.now());
    batchTimer = setTimeout(() => {
      batchTimer = null;
      startBatch();
    }, delayMs);
  };
  const finishBatch = (
    batch: AttachDiagnosisBatch,
    response: AttachDiagnosisBatchResponse | null,
  ): void => {
    if (inFlight !== batch) return;
    inFlight = null;
    if (response !== null && !batch.controller.signal.aborted) acceptSnapshot(batch, response);
    for (const { session } of batch.targets) enqueue(session);
    armBatch();
  };
  const startBatch = (): void => {
    if (disposed || inFlight !== null) return;
    const targets = takeBatch();
    if (targets.length === 0) return;
    const batch: AttachDiagnosisBatch = {
      controller: new AbortController(),
      targets,
    };
    inFlight = batch;
    nextBatchAtMs = Date.now() + ATTACH_DIAGNOSIS_BATCH_INTERVAL_MS;
    let request: Promise<AttachDiagnosisBatchResponse>;
    try {
      request = requestSnapshots(targets.map(({ session }) => session.sessionId), batch.controller.signal);
    } catch {
      finishBatch(batch, null);
      return;
    }
    void request.then(
      (response) => finishBatch(batch, response),
      () => finishBatch(batch, null),
    );
  };

  return {
    dispose(): void {
      if (disposed) return;
      disposed = true;
      clearBatchTimer();
      inFlight?.controller.abort();
      inFlight = null;
      sessions.clear();
      readySessionRing.clear();
    },
    register(sessionId, onReason): AttachDiagnosisHandle {
      if (disposed || sessionId.length === 0) return { dispose(): void {} };
      let session = sessions.get(sessionId);
      if (!session) {
        session = {
          hasVisibleReason: false,
          previousTerminalScreenSeq: null,
          reason: null,
          sessionId,
          subscribers: new Set(),
        };
        sessions.set(sessionId, session);
      }
      const subscriber: AttachDiagnosisRegistration = { onReason };
      session.subscribers.add(subscriber);
      if (session.hasVisibleReason) notifySubscriber(subscriber, session.reason);
      enqueue(session);
      armBatch();
      let registrationDisposed = false;
      return {
        dispose(): void {
          if (registrationDisposed) return;
          registrationDisposed = true;
          session!.subscribers.delete(subscriber);
          if (session!.subscribers.size === 0 && sessions.get(sessionId) === session) {
            sessions.delete(sessionId);
            readySessionRing.delete(session);
          }
          cancelEmptyBatch();
        },
      };
    },
  };
}

function notifySubscriber(
  subscriber: AttachDiagnosisRegistration,
  reason: string | null,
): void {
  try {
    subscriber.onReason(reason);
  } catch {
    // Diagnosis display is advisory and must not interrupt terminal recovery.
  }
}
