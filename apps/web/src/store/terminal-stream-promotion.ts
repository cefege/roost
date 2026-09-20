// Direct ingress reaches this facade before canonical terminal dispatch.
// It owns the document-wide staged-source budget and candidate lookup indexes.
// Candidate folding and atomic canonical application live in focused adjacent owners.
// The direct registry consumes the prepared object returned by each candidate.

import type { LocalTerminalServerFrame } from "@roost/shared/proto/local_terminal_pb";
import {
  dispatchTerminalCellChunk,
  dispatchTerminalCellFrame,
} from "./terminal-stream-replica.ts";
import { registerTerminalPromotionStateDisposer, terminalSessions } from "./terminal-stream-state.ts";
import { dispatchTerminalViewState } from "./terminal-stream-view-commands.ts";
import {
  TerminalPromotionCandidate,
  type TerminalPromotionCandidateRegistry,
  type TerminalPromotionSourceBudget,
  type TerminalSessionPromotion,
  type TerminalSessionPromotionOptions,
} from "./terminal-stream-promotion-candidate.ts";
import {
  terminalGenerationTokenEquals,
  type TerminalGenerationToken,
} from "./terminal-stream-types.ts";
import type { TerminalDirectConnection } from "./terminal-stream-transport.ts";

const TERMINAL_PROMOTION_SOURCE_MAX_BYTES = 128 * 1024 * 1024;
const candidatesBySession = new Map<string, TerminalPromotionCandidate>();
const candidatesByViewId = new Map<string, TerminalPromotionCandidate>();
let retainedCandidateSourceBytes = 0;

const candidateRegistry: TerminalPromotionCandidateRegistry = {
  unregister(candidate): void {
    if (candidatesBySession.get(candidate.sessionId) === candidate) {
      candidatesBySession.delete(candidate.sessionId);
    }
    for (const viewId of candidate.prospectiveViewIds) {
      if (candidatesByViewId.get(viewId) === candidate) candidatesByViewId.delete(viewId);
    }
  },
};

const candidateSourceBudget: TerminalPromotionSourceBudget = {
  reserve(bytes): boolean {
    if (
      !Number.isSafeInteger(bytes)
      || bytes < 1
      || retainedCandidateSourceBytes > TERMINAL_PROMOTION_SOURCE_MAX_BYTES - bytes
    ) return false;
    retainedCandidateSourceBytes += bytes;
    return true;
  },
  release(bytes): void {
    if (bytes === 0) return;
    if (!Number.isSafeInteger(bytes) || bytes < 0 || bytes > retainedCandidateSourceBytes) {
      throw new Error("terminal promotion source budget release mismatch");
    }
    retainedCandidateSourceBytes -= bytes;
  },
};

export type {
  TerminalSessionPromotion,
  TerminalSessionPromotionOptions,
} from "./terminal-stream-promotion-candidate.ts";

export function createTerminalSessionPromotion(
  options: TerminalSessionPromotionOptions,
): TerminalSessionPromotion | null {
  const session = terminalSessions.get(options.sessionId);
  if (
    !session
    || session.workerFp !== options.token.workerFp
    || options.token.transportKind === "sync"
    || !options.connection.allowsSession(options.sessionId)
  ) return null;
  candidatesBySession.get(options.sessionId)?.cancel("candidate replaced");
  const candidate = new TerminalPromotionCandidate(
    session,
    options,
    candidateSourceBudget,
    candidateRegistry,
  );
  if (!candidate.start()) return null;
  candidatesBySession.set(options.sessionId, candidate);
  for (const viewId of candidate.prospectiveViewIds) candidatesByViewId.set(viewId, candidate);
  return candidate;
}

/** Connection currently staging this session, including an already-active worker peer. */
export function terminalSessionPromotionConnection(
  sessionId: string,
): TerminalDirectConnection | null {
  return candidatesBySession.get(sessionId)?.connection() ?? null;
}

/** Routes authenticated direct frames only after their complete transport token matches. */
export function dispatchDirectTerminalFrame(
  token: TerminalGenerationToken,
  frame: LocalTerminalServerFrame,
): void {
  const oneof = frame.frame;
  if (oneof.case === "terminalViewState") {
    const candidate = candidatesByViewId.get(oneof.value.viewId);
    if (candidate && terminalGenerationTokenEquals(candidate.token, token)) {
      candidate.acceptViewState(oneof.value);
      return;
    }
    dispatchTerminalViewState(oneof.value, token);
    return;
  }
  if (oneof.case === "cellGrid") {
    const candidate = candidatesBySession.get(oneof.value.sessionId);
    if (candidate && terminalGenerationTokenEquals(candidate.token, token)) {
      candidate.acceptCellFrame(oneof.value, false);
      return;
    }
    dispatchTerminalCellFrame(oneof.value, token);
    return;
  }
  if (oneof.case === "cellGridChunk") {
    const sessionId = oneof.value.part?.sessionId;
    const candidate = sessionId ? candidatesBySession.get(sessionId) : undefined;
    if (candidate && terminalGenerationTokenEquals(candidate.token, token)) {
      candidate.acceptCellChunk(oneof.value);
      return;
    }
    dispatchTerminalCellChunk(oneof.value, token);
  }
}

registerTerminalPromotionStateDisposer((sessionId, reason) => {
  candidatesBySession.get(sessionId)?.cancel(reason);
});
