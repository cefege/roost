// Scoped terminal repair sends same-generation resync commands before redial.
// It owns idle/proof deadlines and records ACK, chunk, and cell progress.
// terminal-stream-liveness.ts supplies generation identity and timer retirement.
// Replica and view-command owners call this module through their existing APIs.

import type { CellGridFrame } from "@roost/shared/cell";
import type { PbCellGridChunk } from "@roost/shared/proto/cell_pb";
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
    || hasPendingTerminalProofChallenge(session, owner)
  ) return false;

  const sent = session.expectedStreamId
    ? sendTerminalResyncCommand(session, view, sync)
    : false;
  if (!sent && !reassert) return false;
  beginTerminalScopedRepair(session, owner, performance.now());
  signal("cell.foreground_stall", {
    sid: session.sessionId,
    stream_id: session.expectedStreamId,
    layer: "terminal_proof",
    action: "resync",
    cooldownKey: session.sessionId,
    ...terminalProofSignalFields(session, owner),
  });
  return true;
}

export function requestTerminalDomReconcileRecovery(
  session: TerminalSessionReplica,
): boolean {
  const sync = currentSyncV2TerminalState();
  const owner = session.generation;
  if (
    !sync?.ready
    || !owner
    || !activeForegroundTerminalView(session)
    || !terminalGenerationMatches(owner, sync)
  ) return false;
  return requestSyncGenerationRecovery(owner, "terminal-dom-reconcile-timeout");
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
    && !hasPendingTerminalProofChallenge(session, owner)
  ) {
    beginTerminalScopedRepair(session, owner, performance.now());
  }
}

export function requestTerminalResync(
  session: TerminalSessionReplica,
  reason: string,
  owner = session.generation,
  reportGap = true,
  rearmChunkProofDeadline = false,
): void {
  if (!owner || !terminalGenerationMatches(session.generation, owner)) return;
  const interruptedChunkTransfer = rearmChunkProofDeadline
    || session.assembler.activeSnapshotId !== null;
  const pendingChunkProof = hasPendingTerminalProofChallenge(session, owner);
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
  if (interruptedChunkTransfer && pendingChunkProof) {
    rearmTerminalProofDeadline(session, owner);
  }
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
  canonical: CellGridFrame,
  full: boolean,
  owner: TerminalGenerationToken,
): void {
  if (!terminalGenerationMatches(session.generation, owner)) return;
  session.lastAcceptedFrameAtMs = performance.now();
  session.lastAcceptedFrameGeneration = session.generation;
  const sourceProof = terminalProofSourceMatches(
    session,
    owner,
    canonical.streamId,
    canonical.seq,
  );
  if (sourceProof) {
    clearTerminalProofChallenge(session);
    session.repairOutcome = "proved";
  }
  if (full) {
    clearTerminalRepairLatch(session);
  } else if (session.resyncLatched) {
    // A delta proves the lane is live but cannot repair the canonical gap.
    session.resyncLatchedAtMs = null;
  }
  armTerminalForegroundIdleProbe(session);
}

export function noteTerminalProofChunkProgress(
  session: TerminalSessionReplica,
  chunk: PbCellGridChunk,
  owner: TerminalGenerationToken,
): void {
  const part = chunk.part;
  if (!part || !terminalProofSourceMatches(session, owner, part.streamId, part.seq)) return;
  clearTimeout(session.proofDeadlineTimer ?? undefined);
  session.proofDeadlineTimer = null;
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
  const canonical = session.canonical;
  session.proofChallengeAtMs = challengedAt;
  session.proofChallengeGeneration = owner;
  session.proofChallengeStreamId = session.expectedStreamId;
  session.proofChallengeSeq = canonical?.streamId === session.expectedStreamId
    ? canonical.seq
    : 0;
  session.repairAttempts++;
  session.repairOutcome = "requested";
  armTerminalProofDeadline(session, owner, challengedAt);
}

function rearmTerminalProofDeadline(
  session: TerminalSessionReplica,
  owner: TerminalGenerationToken,
): void {
  if (!hasPendingTerminalProofChallenge(session, owner)) return;
  const challengedAt = performance.now();
  session.proofChallengeAtMs = challengedAt;
  session.repairAttempts++;
  session.repairOutcome = "requested";
  armTerminalProofDeadline(session, owner, challengedAt);
}

function hasPendingTerminalProofChallenge(
  session: TerminalSessionReplica,
  owner: TerminalGenerationToken,
): boolean {
  return session.proofChallengeAtMs !== null
    && terminalGenerationMatches(session.proofChallengeGeneration, owner);
}

function terminalProofSourceMatches(
  session: TerminalSessionReplica,
  owner: TerminalGenerationToken,
  streamId: string,
  seq: number | bigint,
): boolean {
  const challengeSeq = session.proofChallengeSeq;
  if (
    challengeSeq === null
    || !terminalGenerationMatches(session.generation, owner)
    || session.proofChallengeStreamId !== streamId
    || !hasPendingTerminalProofChallenge(session, owner)
  ) return false;
  return (typeof seq === "bigint" ? seq : BigInt(seq)) > BigInt(challengeSeq);
}

function clearTerminalProofChallenge(session: TerminalSessionReplica): void {
  clearTimeout(session.proofDeadlineTimer ?? undefined);
  session.proofDeadlineTimer = null;
  session.proofChallengeAtMs = null;
  session.proofChallengeGeneration = null;
  session.proofChallengeStreamId = null;
  session.proofChallengeSeq = null;
}

function armTerminalProofDeadline(
  session: TerminalSessionReplica,
  owner: TerminalGenerationToken,
  challengedAt: number,
): void {
  clearTimeout(session.proofDeadlineTimer ?? undefined);
  const dueAt = challengedAt + TERMINAL_FOREGROUND_PROBE_DEADLINE_MS;
  const timer = setTimeout(() => {
    if (session.proofDeadlineTimer !== timer) return;
    session.proofDeadlineTimer = null;
    const sync = currentSyncV2TerminalState();
    if (
      !activeForegroundTerminalView(session)
      || !terminalGenerationMatches(session.generation, owner)
      || !hasPendingTerminalProofChallenge(session, owner)
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
      ...terminalProofSignalFields(session, owner),
    });
    requestSyncGenerationRecovery(owner, "terminal-proof-timeout");
  }, Math.max(0, dueAt - performance.now()));
  session.proofDeadlineTimer = timer;
}

function terminalProofSignalFields(
  session: TerminalSessionReplica,
  owner: TerminalGenerationToken,
) {
  const canonical = session.canonical;
  return {
    expected_stream_id: session.expectedStreamId,
    checkpoint_stream_id: canonical?.streamId ?? null,
    checkpoint_seq: canonical?.seq ?? null,
    challenge_stream_id: session.proofChallengeStreamId,
    challenge_seq: session.proofChallengeSeq,
    baseline_ready: session.baselineReady,
    resync_latched: session.resyncLatched,
    repair_attempts: session.repairAttempts,
    socket_generation: owner.socketGeneration,
    socket_id: owner.socketId,
    process_epoch: owner.processEpoch,
    domain_generation: owner.domainGeneration.toString(),
  };
}
