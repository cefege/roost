// Terminal liveness state binds repairs to the current Sync generation.
// This module owns identity comparison and timer retirement only.
// terminal-stream-repair.ts owns scoped resync commands and proof deadlines.
// The session replica supplies continuity state while views supply active leases.

import type { SyncV2TerminalState } from "./sync.ts";
import type {
  TerminalGenerationToken,
  TerminalSessionReplica,
  TerminalViewRecord,
} from "./terminal-stream-types.ts";

export function terminalGenerationToken(
  state: SyncV2TerminalState,
): TerminalGenerationToken {
  return {
    socketGeneration: state.socketGeneration,
    socketId: state.socketId,
    processEpoch: state.processEpoch,
    domainGeneration: state.domainGeneration,
  };
}

export function terminalGenerationMatches(
  token: TerminalGenerationToken | null,
  state: SyncV2TerminalState | TerminalGenerationToken | null,
): boolean {
  return token !== null
    && state !== null
    && token.socketGeneration === state.socketGeneration
    && token.socketId === state.socketId
    && token.processEpoch === state.processEpoch
    && token.domainGeneration === state.domainGeneration;
}

export function terminalGenerationKey(state: SyncV2TerminalState): string {
  return [
    state.socketGeneration,
    state.socketId,
    state.processEpoch,
    state.domainGeneration,
  ].join("\u0000");
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
  clearTimeout(session.proofDeadlineTimer ?? undefined);
  session.proofDeadlineTimer = null;
  session.lastAcceptedFrameAtMs = null;
  session.lastAcceptedFrameGeneration = null;
  session.proofChallengeAtMs = null;
  session.proofChallengeGeneration = null;
  session.scopedRepairRounds = 0;
  session.scopedRepairStartedAtMs = null;
  session.scopedRepairGeneration = null;
  session.repairAttempts = 0;
  session.repairOutcome = outcome;
  clearTerminalRepairLatch(session);
}
