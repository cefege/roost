// Scoped terminal repair sends same-generation resync commands before redial.
// It owns idle/proof deadlines and records ACK, chunk, and cell progress.
// terminal-stream-liveness.ts supplies generation identity and timer retirement.
// Replica and view-command owners call this module through their existing APIs.

import { create } from "@bufbuild/protobuf";
import { signal } from "@roost/shared/diag";
import { TerminalResyncCommandSchema } from "@roost/shared/proto/sync_pb";
import {
  TERMINAL_FOREGROUND_IDLE_PROBE_MS,
  TERMINAL_FOREGROUND_PROBE_DEADLINE_MS,
  TERMINAL_VIEW_HEARTBEAT_MS,
} from "@roost/shared/viewport";
import { isPageVisible } from "../lib/pageVisible.ts";
import {
  currentSyncV2TerminalState,
  requestSyncGenerationRecovery,
  sendSyncV2Command,
  type SyncV2TerminalState,
} from "./sync.ts";
import { clearTerminalChunkTransfer } from "./terminal-stream-chunks.ts";
import {
  activeTerminalResyncView,
  clearTerminalRepairLatch,
  clearTerminalSessionLiveness,
  terminalGenerationKey,
  terminalGenerationMatches,
} from "./terminal-stream-liveness.ts";
import type {
  TerminalGenerationToken,
  TerminalOutboundCommand,
  TerminalSessionReplica,
  TerminalViewRecord,
} from "./terminal-stream-types.ts";


export function requestTerminalLivenessChallenge(
  session: TerminalSessionReplica,
  reassert?: () => void,
): boolean {
  const sync = currentSyncV2TerminalState();
  const owner = session.generation;
  const view = activeForegroundTerminalView(session);
  if (
    !sync?.ready
    || !owner
    || !view
    || !terminalGenerationMatches(owner, sync)
    || (!session.expectedStreamId && !reassert)
  ) return false;
  if (
    session.proofDeadlineTimer !== null
    && terminalGenerationMatches(session.proofChallengeGeneration, owner)
  ) return false;

  const sent = session.expectedStreamId
    ? sendTerminalResyncCommand(session, view, sync)
    : false;
  if (!sent && !reassert) return false;
  const challengedAt = performance.now();
  beginTerminalScopedRepair(session, owner, challengedAt);
  signal("cell.foreground_stall", {
    sid: session.sessionId,
    stream_id: session.expectedStreamId,
    layer: "terminal_proof",
    action: "resync",
    cooldownKey: session.sessionId,
  });
  return true;
}

export function armTerminalForegroundIdleProbe(
  session: TerminalSessionReplica,
): void {
  if (session.idleProbeTimer !== null) return;
  const owner = session.generation;
  if (!owner || !session.baselineReady || !activeForegroundTerminalView(session)) return;
  const startedAt = terminalGenerationMatches(session.lastAcceptedFrameGeneration, owner)
    ? (session.lastAcceptedFrameAtMs ?? performance.now())
    : performance.now();
  const dueAt = startedAt + TERMINAL_FOREGROUND_IDLE_PROBE_MS;
  const timer = setTimeout(() => {
    if (session.idleProbeTimer !== timer) return;
    session.idleProbeTimer = null;
    const sync = currentSyncV2TerminalState();
    if (
      !activeForegroundTerminalView(session)
      || !terminalGenerationMatches(session.generation, owner)
      || !terminalGenerationMatches(owner, sync)
    ) {
      clearTerminalSessionLiveness(session, "inactive");
      return;
    }
    if (
      terminalGenerationMatches(session.lastAcceptedFrameGeneration, owner)
      && session.lastAcceptedFrameAtMs !== null
      && session.lastAcceptedFrameAtMs > startedAt
    ) {
      armTerminalForegroundIdleProbe(session);
      return;
    }
    requestTerminalLivenessChallenge(session);
  }, Math.max(0, dueAt - performance.now()));
  session.idleProbeTimer = timer;
}

export function sendLatchedTerminalResync(session: TerminalSessionReplica): void {
  if (!session.resyncLatched || !session.expectedStreamId) return;
  const sync = currentSyncV2TerminalState();
  const view = activeForegroundTerminalView(session);
  const owner = session.resyncLatchGeneration;
  if (
    !sync?.ready
    || !view
    || !owner
    || !terminalGenerationMatches(session.generation, sync)
    || !terminalGenerationMatches(owner, sync)
  ) return;
  const key = terminalGenerationKey(sync);
  const now = Date.now();
  if (
    session.resyncSentGeneration === key
    && session.resyncRetryGeneration === key
    && session.resyncRetryAtMs !== null
    && now - session.resyncRetryAtMs < TERMINAL_VIEW_HEARTBEAT_MS
  ) return;
  if (!sendTerminalResyncCommand(session, view, sync)) return;
  session.resyncSentGeneration = key;
  session.resyncRetryGeneration = key;
  session.resyncRetryAtMs = now;
  if (
    session.resyncLatchedAtMs !== null
    && (
      session.proofDeadlineTimer === null
      || !terminalGenerationMatches(session.proofChallengeGeneration, owner)
    )
  ) {
    beginTerminalScopedRepair(session, owner, performance.now());
  }
}

export function requestTerminalResync(
  session: TerminalSessionReplica,
  reason: string,
  owner = session.generation,
  reportGap = true,
): void {
  if (!owner || !terminalGenerationMatches(session.generation, owner)) return;
  clearTerminalChunkTransfer(session);
  if (!session.resyncLatched) {
    session.resyncLatchedAtMs = performance.now();
    session.resyncLatchGeneration = owner;
    session.resyncLatched = true;
    if (reportGap) {
      signal("cell.seq_gap", {
        sid: session.sessionId,
        stream_id: session.expectedStreamId,
        reason: reason.slice(0, 200),
        cooldownKey: session.sessionId,
      });
    }
  }
  sendLatchedTerminalResync(session);
}

export function repairStaleTerminalSubscriberOnHeartbeat(
  session: TerminalSessionReplica,
): void {
  if (session.proofDeadlineTimer !== null || session.assembler.activeSnapshotId !== null) return;
  const canonical = session.canonical;
  if (!session.baselineReady || !canonical) {
    requestTerminalResync(session, "terminal baseline was still missing at renewal");
    return;
  }
  if (session.resyncLatched) {
    sendLatchedTerminalResync(session);
    return;
  }
  for (const subscriber of session.subscribers) {
    if (
      subscriber.streamId !== canonical.streamId
      || subscriber.gridEpoch !== canonical.gridEpoch
      || subscriber.seq !== canonical.seq
    ) {
      requestTerminalResync(
        session,
        "terminal renderer applied sequence trailed the canonical replica at renewal",
        session.generation,
        false,
      );
      return;
    }
  }
}


export function noteTerminalCellFrame(
  session: TerminalSessionReplica,
  full: boolean,
  owner: TerminalGenerationToken,
): void {
  if (!terminalGenerationMatches(session.generation, owner)) return;
  session.lastAcceptedFrameAtMs = performance.now();
  session.lastAcceptedFrameGeneration = session.generation;
  noteTerminalProgress(session, owner);
  if (full) {
    clearTerminalRepairLatch(session);
  } else if (session.resyncLatched) {
    // A delta proves the lane is live but cannot repair the canonical gap.
    session.resyncLatchedAtMs = null;
  }
  armTerminalForegroundIdleProbe(session);
}

function noteTerminalProgress(
  session: TerminalSessionReplica,
  owner: TerminalGenerationToken,
): void {
  if (!terminalGenerationMatches(session.generation, owner)) return;
  clearTimeout(session.proofDeadlineTimer ?? undefined);
  session.proofDeadlineTimer = null;
  session.proofChallengeAtMs = null;
  session.proofChallengeGeneration = null;
  session.repairOutcome = "proved";
}

function activeForegroundTerminalView(
  session: TerminalSessionReplica,
): TerminalViewRecord | null {
  return isPageVisible() ? activeTerminalResyncView(session) : null;
}

function sendTerminalResyncCommand(
  session: TerminalSessionReplica,
  view: TerminalViewRecord,
  sync: SyncV2TerminalState,
): boolean {
  if (!session.expectedStreamId) return false;
  const canonical = session.canonical;
  const outbound: TerminalOutboundCommand = {
    case: "terminalResync",
    value: create(TerminalResyncCommandSchema, {
      viewId: view.viewId,
      sessionId: session.sessionId,
      streamId: session.expectedStreamId,
      gridEpoch: canonical?.gridEpoch ?? "",
      seq: BigInt(canonical?.seq ?? 0),
      domainGeneration: sync.domainGeneration,
    }),
  };
  return sendSyncV2Command(outbound);
}

function beginTerminalScopedRepair(
  session: TerminalSessionReplica,
  owner: TerminalGenerationToken,
  challengedAt: number,
): void {
  session.repairAttempts++;
  session.repairOutcome = "requested";
  armTerminalProofDeadline(session, owner, challengedAt);
}

function armTerminalProofDeadline(
  session: TerminalSessionReplica,
  owner: TerminalGenerationToken,
  challengedAt: number,
): void {
  clearTimeout(session.proofDeadlineTimer ?? undefined);
  session.proofChallengeAtMs = challengedAt;
  session.proofChallengeGeneration = owner;
  const dueAt = challengedAt + TERMINAL_FOREGROUND_PROBE_DEADLINE_MS;
  const timer = setTimeout(() => {
    if (session.proofDeadlineTimer !== timer) return;
    session.proofDeadlineTimer = null;
    const sync = currentSyncV2TerminalState();
    if (
      !activeForegroundTerminalView(session)
      || !terminalGenerationMatches(session.generation, owner)
      || !terminalGenerationMatches(session.proofChallengeGeneration, owner)
      || !terminalGenerationMatches(owner, sync)
    ) {
      clearTerminalSessionLiveness(session, "inactive");
      return;
    }
    const proofAgeMs = Math.max(0, performance.now() - challengedAt);
    session.repairOutcome = "escalated";
    signal("cell.foreground_stall", {
      sid: session.sessionId,
      stream_id: session.expectedStreamId,
      layer: "terminal_proof",
      action: "redial",
      age_ms: proofAgeMs,
      cooldownKey: session.sessionId,
    });
    requestSyncGenerationRecovery(owner, "terminal-proof-timeout");
  }, Math.max(0, dueAt - performance.now()));
  session.proofDeadlineTimer = timer;
}
