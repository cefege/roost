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
  currentTerminalGenerationToken,
  requestTerminalGenerationRecovery,
  terminalPublicationTarget,
  type TerminalPublicationTarget,
} from "./terminal-stream-publication.ts";
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
  TerminalSessionReplica,
  TerminalViewRecord,
} from "./terminal-stream-types.ts";

export function requestTerminalLivenessChallenge(
  session: TerminalSessionReplica,
  reassert?: () => void,
): boolean {
  const target = terminalPublicationTarget(session.sessionId);
  const owner = session.generation;
  const view = activeForegroundTerminalView(session);
  if (
    !target
    || !owner
    || !view
    || !terminalGenerationMatches(owner, target.token)
    || (!session.expectedStreamId && !reassert)
    || hasPendingTerminalProofChallenge(session, owner)
  ) return false;

  // sendTerminalResyncCommand refuses a session with no expected stream, so a
  // write that did not happen and no reassert leaves nothing to prove.
  if (!sendTerminalResyncCommand(session, view, target) && !reassert) return false;
  beginTerminalScopedRepair(session, owner, performance.now());
  signalTerminalProofStall(session, owner, "resync");
  return true;
}

export function requestTerminalDomReconcileRecovery(
  session: TerminalSessionReplica,
): boolean {
  const target = terminalPublicationTarget(session.sessionId);
  const owner = session.generation;
  if (
    !target
    || !owner
    || !activeForegroundTerminalView(session)
    || !terminalGenerationMatches(owner, target.token)
  ) return false;
  return requestTerminalGenerationRecovery(owner, "terminal-dom-reconcile-timeout");
}

export function armTerminalForegroundIdleProbe(session: TerminalSessionReplica): void {
  const owner = session.generation;
  const observedAt = terminalGenerationMatches(session.lastAcceptedFrameGeneration, owner)
    ? session.lastAcceptedFrameAtMs
    : null;
  armTerminalIdleProbeSince(session, observedAt ?? performance.now());
}

export function sendLatchedTerminalResync(session: TerminalSessionReplica): void {
  if (!session.resyncLatched || !session.expectedStreamId) return;
  const target = terminalPublicationTarget(session.sessionId);
  const view = activeForegroundTerminalView(session);
  const owner = session.resyncLatchGeneration;
  if (
    !target
    || !view
    || !owner
    || !terminalGenerationMatches(session.generation, target.token)
    || !terminalGenerationMatches(owner, target.token)
  ) return;
  const key = terminalGenerationKey(target.token);
  const now = Date.now();
  if (
    session.resyncSentGeneration === key
    && session.resyncRetryGeneration === key
    && session.resyncRetryAtMs !== null
    && now - session.resyncRetryAtMs < TERMINAL_VIEW_HEARTBEAT_MS
  ) return;
  if (!sendTerminalResyncCommand(session, view, target)) return;
  session.resyncSentGeneration = key;
  session.resyncRetryGeneration = key;
  session.resyncRetryAtMs = now;
  // Only a full frame repairs the canonical gap a latch owns, so a latched
  // send with no proof deadline armed owes the session one: the accepted delta
  // that cleared this latch's timestamp proved the lane, not the gap.
  if (!hasPendingTerminalProofChallenge(session, owner)) {
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
  target: TerminalPublicationTarget,
): boolean {
  if (!session.expectedStreamId) return false;
  const canonical = session.canonical;
  return target.publishResync(create(TerminalResyncCommandSchema, {
    viewId: view.viewId,
    sessionId: session.sessionId,
    streamId: session.expectedStreamId,
    gridEpoch: canonical?.gridEpoch ?? "",
    seq: BigInt(canonical?.seq ?? 0),
    domainGeneration: target.domainGeneration,
  }));
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

/** A pane that stopped painting has no other watchdog, so every exit from this
 * callback either escalates to a proof deadline or re-arms the probe. `since`
 * anchors the next due time, and a challenge that could not be published
 * re-anchors at now so the retry stays one full interval away. */
function armTerminalIdleProbeSince(
  session: TerminalSessionReplica,
  since: number,
): void {
  if (session.idleProbeTimer !== null) return;
  const owner = session.generation;
  if (!owner || !session.baselineReady || !activeForegroundTerminalView(session)) return;
  const dueAt = since + TERMINAL_FOREGROUND_IDLE_PROBE_MS;
  const timer = setTimeout(() => {
    if (session.idleProbeTimer !== timer) return;
    session.idleProbeTimer = null;
    if (
      !activeForegroundTerminalView(session)
      || !terminalGenerationMatches(session.generation, owner)
      || !terminalGenerationMatches(owner, currentTerminalGenerationToken(session.sessionId))
    ) {
      clearTerminalSessionLiveness(session, "inactive");
      return;
    }
    if (
      terminalGenerationMatches(session.lastAcceptedFrameGeneration, owner)
      && session.lastAcceptedFrameAtMs !== null
      && session.lastAcceptedFrameAtMs > since
    ) {
      armTerminalForegroundIdleProbe(session);
      return;
    }
    if (requestTerminalLivenessChallenge(session)) return;
    signalTerminalProofStall(session, owner, "rearm", Math.max(0, performance.now() - since));
    armTerminalIdleProbeSince(session, performance.now());
  }, Math.max(0, dueAt - performance.now()));
  session.idleProbeTimer = timer;
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
    const current = currentTerminalGenerationToken(session.sessionId);
    if (
      !activeForegroundTerminalView(session)
      || !terminalGenerationMatches(session.generation, owner)
      || !hasPendingTerminalProofChallenge(session, owner)
      || !terminalGenerationMatches(owner, current)
    ) {
      clearTerminalSessionLiveness(session, "inactive");
      return;
    }
    const proofAgeMs = Math.max(0, performance.now() - challengedAt);
    session.repairOutcome = "escalated";
    signalTerminalProofStall(session, owner, "redial", proofAgeMs);
    requestTerminalGenerationRecovery(owner, "terminal-proof-timeout");
  }, Math.max(0, dueAt - performance.now()));
  session.proofDeadlineTimer = timer;
}

/** One payload shape for this layer's transitions, so a field added here
 * reaches all of them. A rearm keeps its own cooldown scope: it repeats for as
 * long as the challenge cannot be published, and sharing the session's scope
 * would coalesce away the resync or redial that follows it. */
function signalTerminalProofStall(
  session: TerminalSessionReplica,
  owner: TerminalGenerationToken,
  action: "resync" | "redial" | "rearm",
  ageMs: number | null = null,
): void {
  const canonical = session.canonical;
  signal("cell.foreground_stall", {
    sid: session.sessionId,
    stream_id: session.expectedStreamId,
    layer: "terminal_proof",
    action,
    age_ms: ageMs,
    cooldownKey: action === "rearm" ? `${session.sessionId}|rearm` : session.sessionId,
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
  });
}
