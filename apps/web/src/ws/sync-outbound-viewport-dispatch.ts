// Wire dispatch for the terminal viewport lane: the retry ladder, the result and
// repair deadlines, the coordinator's control results, and the reconciliation a
// Sync generation change forces. Split out of ws/sync-outbound.ts.
//
// Ordering here is load-bearing. A dispatch is only ever issued for the record
// the registry still recognises AND for the live socket/process/domain identity,
// and every deferred callback re-checks both, so a late deadline from a retired
// generation can never supersede the claim that replaced it.

import { markPhase } from "../lib/diag.ts";
import {
  currentSyncV2TerminalState,
  sendSyncV2Command,
  type SyncV2Control,
  type SyncV2TerminalState,
} from "../store/sync.ts";
import {
  armSmokeViewportRejection,
  consumeSmokeViewportRejection,
  smokeBackdoorEnabled,
} from "./sync-outbound-smoke.ts";
import {
  advanceViewportSequence,
  allViewportSessions,
  boundedViewportReason,
  clearViewportAttempt,
  clearViewportRetry,
  emitViewportStatus,
  existingViewportSession,
  isCurrentViewportSession,
  rebaseViewportSequenceFloor,
  settleViewportDesired,
  viewportSession,
} from "./sync-outbound-viewport-registry.ts";
import type {
  ViewportAttempt,
  ViewportDesired,
  ViewportOutcome,
  ViewportSession,
} from "./sync-outbound-viewport-types.ts";

const VIEWPORT_RESULT_TIMEOUT_MS = 10_000;
const VIEWPORT_RETRY_DELAYS_MS = [250, 500, 1_000, 2_000] as const;
export const VIEWPORT_REPAIR_TIMEOUT_MS = 3_000;
const equalFloorImmediateRetries = new WeakSet<ViewportDesired>();

type TerminalState = SyncV2TerminalState;
type OutboundCommand = Parameters<typeof sendSyncV2Command>[0];
type ResultControl = SyncV2Control;

/** Reject exactly one positive viewport attempt before it enters the wire.
 * Smoke-only: models a definite negative reclaim result while keeping the
 * desired owner claim live so the normal retry path must recover it. */
export function rejectNextViewportClaim(sessionId: string): void {
  if (!smokeBackdoorEnabled()) return;
  const sync = currentSyncV2TerminalState();
  const session = viewportSession(sessionId);
  armSmokeViewportRejection(sessionId, {
    afterSequence: session.desired?.sequence ?? session.sequenceFloor,
    socketId: sync?.socketId ?? null,
    domainGeneration: sync?.domainGeneration ?? null,
  });
}

/** The generated SyncClientFrame command union is a discriminated oneof the
 * protobuf runtime types structurally; both outbound lanes build their payload
 * literally and hand it through this one cast. */
export function command(value: unknown): OutboundCommand {
  return value as OutboundCommand;
}

export function sendViewportCommand(
  session: ViewportSession,
  desired: ViewportDesired,
  sync: TerminalState,
  cause = desired.cause,
): boolean {
  return sendSyncV2Command(command({
    case: "viewport",
    value: {
      sessionId: session.sessionId,
      cols: desired.cols,
      rows: desired.rows,
      clientSeq: desired.sequence,
      cause,
      heldCellSeq: desired.heldCellSeq,
      domainGeneration: sync.domainGeneration,
    },
  }));
}

export function finishViewportReady(session: ViewportSession, desired: ViewportDesired, attempt: ViewportAttempt): void {
  if (session.desired !== desired || session.attempt !== attempt || !attempt.accepted) return;
  const accepted = attempt.accepted;
  clearViewportAttempt(session);
  desired.retryCount = 0;
  desired.needsSequenceAdvance = false;
  desired.repairRequired = false;
  const outcome: Extract<ViewportOutcome, { status: "accepted" }> = {
    status: "accepted",
    sequence: desired.sequence,
    effectiveCols: accepted.effectiveCols,
    effectiveRows: accepted.effectiveRows,
    channelResizeSeq: accepted.channelResizeSeq,
  };
  emitViewportStatus(session, {
    status: "ready",
    sequence: outcome.sequence,
    effectiveCols: outcome.effectiveCols,
    effectiveRows: outcome.effectiveRows,
    channelResizeSeq: outcome.channelResizeSeq,
  });
  settleViewportDesired(session, outcome);
}

export function scheduleViewportRetry(
  session: ViewportSession,
  desired: ViewportDesired,
  reason: string,
  mode: "backoff" | "immediate" = "backoff",
): void {
  if (session.desired !== desired) return;
  clearViewportAttempt(session);
  clearViewportRetry(session);
  desired.needsSequenceAdvance = true;
  const retryInMs = mode === "immediate"
    ? 0
    : VIEWPORT_RETRY_DELAYS_MS[
      Math.min(desired.retryCount, VIEWPORT_RETRY_DELAYS_MS.length - 1)
    ]!;
  if (mode === "backoff") desired.retryCount += 1;
  session.retryAt = Date.now() + retryInMs;
  session.retryReason = boundedViewportReason(reason);
  const failedSync = currentSyncV2TerminalState();
  session.retrySocketId = failedSync?.socketId ?? null;
  session.retryDomainGeneration = failedSync?.domainGeneration ?? null;
  emitViewportStatus(session, {
    status: "retrying",
    sequence: desired.sequence,
    reason: session.retryReason,
    retryInMs,
  });
  session.retryTimer = setTimeout(() => {
    if (!isCurrentViewportSession(session) || session.desired !== desired) return;
    session.retryTimer = null;
    session.retryAt = null;
    session.retryReason = null;
    session.retrySocketId = null;
    session.retryDomainGeneration = null;
    const sync = currentSyncV2TerminalState();
    if (!sync?.ready) {
      emitViewportStatus(session, {
        status: "pending",
        sequence: desired.sequence,
        repairRequired: desired.repairRequired,
      });
      return;
    }
    dispatchViewportDesired(session, desired, sync);
  }, retryInMs);
}

export function dispatchViewportDesired(session: ViewportSession, desired: ViewportDesired, sync: TerminalState): void {
  if (!isCurrentViewportSession(session)
    || session.desired !== desired
    || session.attempt
    || session.retryTimer
    || !sync.ready) return;
  if (desired.needsSequenceAdvance) advanceViewportSequence(session, desired);
  markPhase("viewport_enqueue", {
    sessionId: session.sessionId,
    generation: sync.domainGeneration,
    sequence: desired.sequence,
  });
  if (consumeSmokeViewportRejection(session, desired, sync)) {
    scheduleViewportRetry(session, desired, "smoke-injected viewport reclaim rejection");
    return;
  }
  const attempt: ViewportAttempt = {
    sequence: desired.sequence,
    socketId: sync.socketId,
    domainGeneration: sync.domainGeneration,
    processEpoch: sync.processEpoch,
    fullFrameReceiptFloor: session.fullFrameReceipt,
    fullFrameReady: false,
    phase: "result",
    deadlineAt: Date.now() + VIEWPORT_RESULT_TIMEOUT_MS,
    timer: null,
    accepted: null,
  };
  if (!sendViewportCommand(session, desired, sync)) {
    scheduleViewportRetry(session, desired, "terminal Sync did not admit the viewport command");
    return;
  }
  session.attempt = attempt;
  emitViewportStatus(session, {
    status: "pending",
    sequence: desired.sequence,
    repairRequired: desired.repairRequired,
  });
  attempt.timer = setTimeout(() => {
    if (!isCurrentViewportSession(session)
      || session.desired !== desired
      || session.attempt !== attempt) return;
    scheduleViewportRetry(session, desired, "viewport result deadline expired");
  }, VIEWPORT_RESULT_TIMEOUT_MS);
}

export function handleViewportControl(control: ResultControl, state: TerminalState): boolean {
  if (control.case !== "viewportAccepted"
    && control.case !== "viewportRejected"
    && control.case !== "viewportAmbiguous") return false;
  const value = control.value;
  const session = existingViewportSession(value.sessionId);
  const desired = session?.desired;
  const attempt = session?.attempt;
  const current = currentSyncV2TerminalState();
  if (!session
    || !desired
    || !attempt
    || attempt.sequence !== value.clientSeq
    || desired.sequence !== value.clientSeq
    || attempt.socketId !== state.socketId
    || attempt.processEpoch !== state.processEpoch
    || attempt.domainGeneration !== value.domainGeneration
    || state.domainGeneration !== value.domainGeneration
    || !current
    || current.socketId !== attempt.socketId
    || current.processEpoch !== attempt.processEpoch
    || current.domainGeneration !== attempt.domainGeneration) return true;

  if (control.case === "viewportAccepted") {
    const accepted = control.value;
    clearTimeout(attempt.timer ?? undefined);
    attempt.timer = null;
    attempt.accepted = {
      effectiveCols: accepted.effectiveCols,
      effectiveRows: accepted.effectiveRows,
      channelResizeSeq: accepted.channelResizeSeq,
    };
    session.confirmed = {
      sequence: accepted.clientSeq,
      socketId: state.socketId,
      domainGeneration: accepted.domainGeneration,
      processEpoch: state.processEpoch,
      effectiveCols: accepted.effectiveCols,
      effectiveRows: accepted.effectiveRows,
    };
    markPhase("viewport_accept", {
      sessionId: session.sessionId,
      generation: accepted.domainGeneration,
      sequence: accepted.clientSeq,
    });
    if (!desired.repairRequired || attempt.fullFrameReady) {
      finishViewportReady(session, desired, attempt);
      return true;
    }
    attempt.phase = "repair";
    attempt.deadlineAt = Date.now() + VIEWPORT_REPAIR_TIMEOUT_MS;
    emitViewportStatus(session, {
      status: "repairing",
      sequence: desired.sequence,
      effectiveCols: accepted.effectiveCols,
      effectiveRows: accepted.effectiveRows,
      channelResizeSeq: accepted.channelResizeSeq,
    });
    attempt.timer = setTimeout(() => {
      if (!isCurrentViewportSession(session)
        || session.desired !== desired
        || session.attempt !== attempt) return;
      scheduleViewportRetry(session, desired, "accepted viewport did not produce a newer full frame");
    }, VIEWPORT_REPAIR_TIMEOUT_MS);
    return true;
  }

  const reason = control.case === "viewportRejected"
    ? `viewport rejected: ${control.value.reason}`
    : `viewport outcome ambiguous: ${control.value.reason}`;
  if (control.case === "viewportRejected"
    && control.value.sequenceFloor !== undefined) {
    const sequenceFloor = control.value.sequenceFloor;
    const floorAdvanced = rebaseViewportSequenceFloor(session, sequenceFloor);
    const equalFloor = sequenceFloor === desired.sequence;
    if (floorAdvanced || (equalFloor && !equalFloorImmediateRetries.has(desired))) {
      if (equalFloor) equalFloorImmediateRetries.add(desired);
      scheduleViewportRetry(session, desired, reason, "immediate");
      return true;
    }
  }
  scheduleViewportRetry(session, desired, reason);
  return true;
}

/** Viewport half of a Sync generation change: a new socket, a replaced worker
 * process epoch, or a new domain generation. Retires whatever the retired
 * generation left behind and re-dispatches every live desired claim. */
export function reconcileViewportGeneration(state: TerminalState | null): void {
  for (const session of allViewportSessions()) {
    const desired = session.desired;
    const producerChanged = state?.ready === true
      && session.processEpoch !== null
      && session.processEpoch !== state.processEpoch;
    if (state?.ready) session.processEpoch = state.processEpoch;
    if (producerChanged && desired && desired.cols > 0 && desired.rows > 0) {
      desired.repairRequired = true;
      desired.needsSequenceAdvance = true;
    }

    const confirmedCurrent = state?.ready === true
      && session.confirmed?.socketId === state.socketId
      && session.confirmed.processEpoch === state.processEpoch
      && session.confirmed.domainGeneration === state.domainGeneration;
    const confirmationInvalid = session.confirmed !== null && !confirmedCurrent;
    if (confirmationInvalid) {
      session.confirmed = null;
      if (desired) desired.needsSequenceAdvance = true;
    }

    const invalidAttempt = session.attempt !== null
      && (!state?.ready
        || session.attempt.socketId !== state.socketId
        || session.attempt.processEpoch !== state.processEpoch
        || session.attempt.domainGeneration !== state.domainGeneration);
    if (invalidAttempt) {
      clearViewportAttempt(session);
      if (desired) {
        desired.needsSequenceAdvance = true;
        desired.retryCount = 0;
      }
    }

    const invalidRetry = session.retryTimer !== null
      && (!state?.ready
        || session.retrySocketId !== state.socketId
        || session.retryDomainGeneration !== state.domainGeneration);
    if (invalidRetry) {
      clearViewportRetry(session);
      if (desired) {
        desired.needsSequenceAdvance = true;
        desired.retryCount = 0;
      }
    }

    if (!desired) continue;
    if (!state?.ready) {
      if (confirmationInvalid || invalidAttempt || invalidRetry) {
        emitViewportStatus(session, {
          status: "pending",
          sequence: desired.sequence,
          repairRequired: desired.repairRequired,
        });
      }
      continue;
    }
    if (confirmedCurrent
      && session.confirmed?.sequence === desired.sequence
      && !session.attempt
      && !session.retryTimer
      && !producerChanged) continue;
    dispatchViewportDesired(session, desired, state);
  }
}

/** `respawned` and a worker boot/reconcile `snapshot` change the PRODUCING
 * worker generation for these sessions: the old core is gone, and the claims the
 * worker held for it went with it (`_dropChannelState` / worker restart). This
 * is the same repair edge `processEpoch` already contracts, so it runs through
 * the same path — a tab holding a current positive owner sends a NEWER claim
 * with `heldCellSeq = 0` and requires the new core's authoritative full frame,
 * while a tab with no positive owner does nothing. The terminal never remounts.
 *
 * Call it after the session projection lands, so a claim can only be dispatched
 * once the store agrees with the coordinator about the session's route. */
export function noteTerminalProducerGeneration(sessionIds: Iterable<string>): void {
  const state = currentSyncV2TerminalState();
  for (const sessionId of sessionIds) {
    const session = existingViewportSession(sessionId);
    const desired = session?.desired;
    if (!session || !desired || desired.cols <= 0 || desired.rows <= 0) continue;
    // The new core holds nothing this tab ever applied, and any acceptance on
    // record belongs to the retired producer.
    desired.heldCellSeq = 0n;
    desired.repairRequired = true;
    desired.needsSequenceAdvance = true;
    desired.retryCount = 0;
    session.confirmed = null;
    if (!state?.ready) {
      emitViewportStatus(session, {
        status: "pending",
        sequence: desired.sequence,
        repairRequired: true,
      });
      continue;
    }
    // An attempt or a scheduled retry aimed at the retired producer can never
    // become this repair: supersede it with the newer claim on the live
    // generation instead of waiting for its deadline.
    clearViewportAttempt(session);
    clearViewportRetry(session);
    dispatchViewportDesired(session, desired, state);
  }
}
