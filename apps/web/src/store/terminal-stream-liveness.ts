// Terminal liveness binds repairs to the current carrier generation. This
// module owns exact identity comparison and timer retirement only; repair owns
// scoped resync/proof deadlines while views supply active leases.

import type { SyncV2TerminalState } from "./sync.ts";
import {
  terminalGenerationTokenEquals,
  type TerminalGenerationToken,
  type TerminalSessionReplica,
  type TerminalViewRecord,
} from "./terminal-stream-types.ts";

export function terminalGenerationToken(
  state: SyncV2TerminalState,
): TerminalGenerationToken {
  return {
    socketGeneration: state.socketGeneration,
    socketId: state.socketId,
    processEpoch: state.processEpoch,
    domainGeneration: state.domainGeneration,
    transportKind: "sync",
    workerFp: null,
  };
}

export function terminalGenerationMatches(
  token: TerminalGenerationToken | null,
  state: SyncV2TerminalState | TerminalGenerationToken | null,
): boolean {
  if (!token || !state) return false;
  if ("transportKind" in state) {
    return terminalGenerationTokenEquals(token, state);
  }
  return token.socketGeneration === state.socketGeneration
    && token.socketId === state.socketId
    && token.processEpoch === state.processEpoch
    && token.domainGeneration === state.domainGeneration
    && token.transportKind === "sync"
    && token.workerFp === null;
}

export function terminalGenerationKey(
  state: SyncV2TerminalState | TerminalGenerationToken,
): string {
  const transportKind = "transportKind" in state ? state.transportKind : "sync";
  const workerFp = "workerFp" in state ? state.workerFp : null;
  return JSON.stringify([
    state.socketGeneration,
    state.socketId,
    state.processEpoch,
    state.domainGeneration.toString(),
    transportKind,
    workerFp,
  ]);
}

export function activeTerminalResyncView(
  session: TerminalSessionReplica,
): TerminalViewRecord | null {
  for (const view of session.handles.values()) {
    if (!view.disposed && view.desired?.active) return view;
  }
  return null;
}

export function clearTerminalRepairLatch(session: TerminalSessionReplica): void {
  session.resyncLatched = false;
  session.resyncSentGeneration = null;
  session.resyncRetryGeneration = null;
  session.resyncRetryAtMs = null;
  session.resyncLatchedAtMs = null;
  session.resyncLatchGeneration = null;
}

export function clearTerminalSessionLiveness(
  session: TerminalSessionReplica,
  outcome: TerminalSessionReplica["repairOutcome"],
): void {
  clearTimeout(session.idleProbeTimer ?? undefined);
  session.idleProbeTimer = null;
  // Retirement ends the episode as surely as a published challenge does: a
  // flag surviving here would silence the FIRST rearm of the next episode,
  // which is the only one that gets reported.
  session.probeRearmReported = false;
  clearTimeout(session.proofDeadlineTimer ?? undefined);
  session.proofDeadlineTimer = null;
  session.lastAcceptedFrameAtMs = null;
  session.lastAcceptedFrameGeneration = null;
  session.proofChallengeAtMs = null;
  session.proofChallengeGeneration = null;
  session.proofChallengeStreamId = null;
  session.proofChallengeSeq = null;
  session.repairAttempts = 0;
  session.repairOutcome = outcome;
  clearTerminalRepairLatch(session);
}
