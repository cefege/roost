// The per-session viewport claim: one desired geometry per mounted pane, and the
// tokenized owner handle a pane holds for its lifetime. Split out of
// ws/sync-outbound.ts; the registry, wire dispatch and shared types live in the
// sync-outbound-viewport-{registry,dispatch,types}.ts siblings.
//
// claimTerminalViewport is deliberately ONE function. Its branches — equivalent
// re-claim, optimistic preclaim adoption, and a fresh sequenced claim — share
// the sequence floor and the supersede ordering, and pulling any of them out has
// historically let two branches disagree about which claim is current.

import { markPhase } from "../lib/diag.ts";
import { currentSyncV2TerminalState } from "../store/sync.ts";
// handleGeneration reconciles BOTH outbound lanes, so it stays with the input
// lane in sync-outbound.ts. The heartbeat below can observe a newer Sync
// generation before the store's notification reaches that module, which is the
// one place this file has to call back up.
import { handleGeneration } from "./sync-outbound.ts";
import {
  dispatchViewportDesired,
  finishViewportReady,
  scheduleViewportRetry,
  sendViewportCommand,
  VIEWPORT_REPAIR_TIMEOUT_MS,
} from "./sync-outbound-viewport-dispatch.ts";
import {
  clearViewportAttempt,
  clearViewportRetry,
  emitViewportStatus,
  existingViewportSession,
  isCurrentViewportSession,
  MAX_SAFE_CELL_SEQ,
  MAX_VIEWPORT_STATUS_LISTENERS,
  MAX_WIRE_DIMENSION,
  settleViewportDesired,
  supersedeViewportDesired,
  updateViewportSequence,
  viewportSession,
} from "./sync-outbound-viewport-registry.ts";
import type {
  TerminalViewportClaim,
  TerminalViewportOwner,
  ViewportAdmission,
  ViewportAttempt,
  ViewportDesired,
  ViewportOutcome,
} from "./sync-outbound-viewport-types.ts";

const HEARTBEAT_CAUSE = 6;

let nextViewportOwnerToken = 0n;

function inactiveViewportAdmission(sessionId: string, token: bigint, reason: string): ViewportAdmission {
  const session = existingViewportSession(sessionId);
  const sequence = session?.desired?.sequence ?? session?.sequenceFloor ?? 0n;
  return {
    sequence,
    result: Promise.resolve({
      status: session?.ownerToken === token ? "rejected" : "superseded",
      sequence,
      reason,
    }),
  };
}

export function claimTerminalViewport(
  sessionId: string,
  token: bigint,
  value: TerminalViewportClaim,
): ViewportAdmission {
  const session = existingViewportSession(sessionId);
  if (!session || session.ownerToken !== token) {
    return inactiveViewportAdmission(sessionId, token, "viewport owner was superseded");
  }

  let heldCellSeq: bigint;
  try {
    heldCellSeq = BigInt(value.heldCellSeq ?? 0);
  } catch {
    return inactiveViewportAdmission(sessionId, token, "held cell sequence is invalid");
  }
  if (!Number.isSafeInteger(value.cols)
    || !Number.isSafeInteger(value.rows)
    || !Number.isSafeInteger(value.cause)
    || value.cols < 0
    || value.cols > MAX_WIRE_DIMENSION
    || value.rows > MAX_WIRE_DIMENSION
    || value.rows < 0
    || value.cause < 0
    || heldCellSeq < 0n
    || heldCellSeq > MAX_SAFE_CELL_SEQ) {
    return inactiveViewportAdmission(sessionId, token, "viewport claim is invalid");
  }
  const positive = value.cols > 0 && value.rows > 0;
  const repairRequired = positive
    && (value.repairRequired === true || heldCellSeq === 0n);
  const currentDesired = session.desired;
  if (currentDesired
    && currentDesired.cols === value.cols
    && currentDesired.rows === value.rows
    && currentDesired.cause === value.cause
    && currentDesired.heldCellSeq === heldCellSeq
    && currentDesired.repairRequired === repairRequired) {
    return currentDesired.admission;
  }

  const preclaim = session.preclaim;
  session.preclaim = null;
  if (preclaim
    && value.cause === 1
    && preclaim.cols === value.cols
    && preclaim.rows === value.rows) {
    if (currentDesired) supersedeViewportDesired(session, "optimistic viewport preclaim replaced the prior intent");
    const { promise, resolve } = Promise.withResolvers<ViewportOutcome>();
    const admission: ViewportAdmission = {
      sequence: preclaim.sequence,
      result: promise,
    };
    const desired: ViewportDesired = {
      sequence: preclaim.sequence,
      cols: preclaim.cols,
      rows: preclaim.rows,
      cause: value.cause,
      heldCellSeq,
      repairRequired,
      updatedAt: Date.now(),
      admission,
      resolve,
      settled: false,
      retryCount: 0,
      needsSequenceAdvance: false,
    };
    session.desired = desired;
    const sync = currentSyncV2TerminalState();
    if (!sync?.ready) {
      desired.needsSequenceAdvance = true;
      emitViewportStatus(session, {
        status: "pending",
        sequence: desired.sequence,
        repairRequired: desired.repairRequired,
      });
      return admission;
    }

    session.processEpoch = sync.processEpoch;
    session.confirmed = {
      sequence: preclaim.sequence,
      socketId: sync.socketId,
      domainGeneration: sync.domainGeneration,
      processEpoch: sync.processEpoch,
      effectiveCols: preclaim.cols,
      effectiveRows: preclaim.rows,
    };
    const accepted = {
      effectiveCols: preclaim.cols,
      effectiveRows: preclaim.rows,
      channelResizeSeq: 0n,
    };
    markPhase("viewport_enqueue", {
      sessionId,
      generation: sync.domainGeneration,
      sequence: preclaim.sequence,
    });
    markPhase("viewport_accept", {
      sessionId,
      generation: sync.domainGeneration,
      sequence: preclaim.sequence,
    });
    if (!repairRequired || session.fullFrameReceipt > 0) {
      desired.repairRequired = false;
      const outcome: Extract<ViewportOutcome, { status: "accepted" }> = {
        status: "accepted",
        sequence: preclaim.sequence,
        ...accepted,
      };
      emitViewportStatus(session, {
        status: "ready",
        sequence: outcome.sequence,
        effectiveCols: outcome.effectiveCols,
        effectiveRows: outcome.effectiveRows,
        channelResizeSeq: outcome.channelResizeSeq,
      });
      settleViewportDesired(session, outcome);
      return admission;
    }

    const attempt: ViewportAttempt = {
      sequence: preclaim.sequence,
      socketId: sync.socketId,
      domainGeneration: sync.domainGeneration,
      processEpoch: sync.processEpoch,
      fullFrameReceiptFloor: session.fullFrameReceipt,
      fullFrameReady: false,
      phase: "repair",
      deadlineAt: Date.now() + VIEWPORT_REPAIR_TIMEOUT_MS,
      timer: null,
      accepted,
    };
    session.attempt = attempt;
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
      scheduleViewportRetry(session, desired, "optimistic viewport did not produce its authoritative full frame");
    }, VIEWPORT_REPAIR_TIMEOUT_MS);
    return admission;
  }

  if (currentDesired) supersedeViewportDesired(session, "newer viewport intent");
  const sequence = session.sequenceFloor + 1n;
  const { promise, resolve } = Promise.withResolvers<ViewportOutcome>();
  const admission: ViewportAdmission = { sequence, result: promise };
  const desired: ViewportDesired = {
    sequence,
    cols: value.cols,
    rows: value.rows,
    cause: value.cause,
    heldCellSeq,
    repairRequired,
    updatedAt: Date.now(),
    admission,
    resolve,
    settled: false,
    retryCount: 0,
    needsSequenceAdvance: false,
  };
  session.desired = desired;
  updateViewportSequence(session, desired, sequence);
  const sync = currentSyncV2TerminalState();
  if (sync?.ready) {
    dispatchViewportDesired(session, desired, sync);
  } else {
    emitViewportStatus(session, { status: "pending", sequence, repairRequired });
  }
  return admission;
}

export function acquireTerminalViewportOwner(sessionId: string): TerminalViewportOwner {
  const session = viewportSession(sessionId);
  if (session.desired) {
    supersedeViewportDesired(session, "newer viewport owner");
  } else {
    clearViewportAttempt(session);
    clearViewportRetry(session);
  }
  if (session.ownerToken !== null && session.listeners.size > 0) {
    emitViewportStatus(session, {
      status: "superseded",
      sequence: session.sequenceFloor,
      reason: "newer viewport owner",
    });
  }
  session.listeners.clear();
  session.confirmed = null;
  session.status = null;
  const token = ++nextViewportOwnerToken;
  session.ownerToken = token;
  let disposed = false;

  return {
    token,
    claim(value) {
      if (disposed) return inactiveViewportAdmission(sessionId, token, "viewport owner was disposed");
      return claimTerminalViewport(sessionId, token, value);
    },
    heartbeat(heldCellSeqValue) {
      if (disposed) return;
      const currentSession = existingViewportSession(sessionId);
      const desired = currentSession?.desired;
      if (!currentSession
        || currentSession.ownerToken !== token
        || !desired
        || desired.cols <= 0
        || desired.rows <= 0) return;
      let heldCellSeq: bigint;
      try {
        heldCellSeq = BigInt(heldCellSeqValue);
      } catch {
        return;
      }
      if (heldCellSeq < 0n || heldCellSeq > MAX_SAFE_CELL_SEQ) return;
      desired.heldCellSeq = heldCellSeq;
      const sync = currentSyncV2TerminalState();
      if (!sync?.ready || currentSession.retryTimer) return;
      if (currentSession.attempt
        && (currentSession.attempt.socketId !== sync.socketId
          || currentSession.attempt.processEpoch !== sync.processEpoch
          || currentSession.attempt.domainGeneration !== sync.domainGeneration)) {
        handleGeneration(sync);
      }
      if (!currentSession.attempt) {
        const confirmedCurrent = currentSession.confirmed?.socketId === sync.socketId
          && currentSession.confirmed.processEpoch === sync.processEpoch
          && currentSession.confirmed.domainGeneration === sync.domainGeneration
          && currentSession.confirmed.sequence === desired.sequence;
        if (!confirmedCurrent) {
          dispatchViewportDesired(currentSession, desired, sync);
          return;
        }
      }
      sendViewportCommand(currentSession, desired, sync, HEARTBEAT_CAUSE);
    },
    noteFullFrame(frame) {
      if (disposed) return;
      const currentSession = existingViewportSession(sessionId);
      if (!currentSession
        || currentSession.ownerToken !== token
        || frame.gridEpoch.length === 0
        || !Number.isSafeInteger(frame.seq)
        || frame.seq <= 0) return;
      if (currentSession.fullFrameGridEpoch === frame.gridEpoch
        && frame.seq <= currentSession.fullFrameSeq) return;
      currentSession.fullFrameGridEpoch = frame.gridEpoch;
      currentSession.fullFrameSeq = frame.seq;
      currentSession.fullFrameReceipt += 1;
      const attempt = currentSession.attempt;
      const desired = currentSession.desired;
      if (!attempt
        || !desired
        || currentSession.fullFrameReceipt <= attempt.fullFrameReceiptFloor) return;
      attempt.fullFrameReady = true;
      if (attempt.accepted) finishViewportReady(currentSession, desired, attempt);
    },
    subscribeStatus(listener) {
      if (disposed) return () => undefined;
      const currentSession = existingViewportSession(sessionId);
      if (!currentSession
        || currentSession.ownerToken !== token
        || currentSession.listeners.size >= MAX_VIEWPORT_STATUS_LISTENERS) return () => undefined;
      currentSession.listeners.add(listener);
      if (currentSession.status) {
        try {
          listener(currentSession.status);
        } catch {
          // A status observer cannot perturb terminal ownership.
        }
      }
      let subscribed = true;
      return () => {
        if (!subscribed) return;
        subscribed = false;
        if (existingViewportSession(sessionId)?.ownerToken === token) {
          currentSession.listeners.delete(listener);
        }
      };
    },
    dispose() {
      if (disposed) return;
      disposed = true;
      const currentSession = existingViewportSession(sessionId);
      if (!currentSession || currentSession.ownerToken !== token) return;
      currentSession.ownerToken = null;
      currentSession.listeners.clear();
    },
  };
}
