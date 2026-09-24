// This replica is the single canonical terminal grid shared by every view of a session.
// It accepts only generation-matched, contiguous frames before mutating canonical state.
// Sync dispatch calls it for full frames and chunks, while subscribers enqueue DOM work.
// Liveness repair is separate but consumes the same stream, epoch, sequence, and viewport facts.

import type { CellGridFrame } from "@roost/protocol/cell";
import type {
  PbCellGridChunk,
  PbCellGridFrame,
} from "@roost/protocol/proto/cell_pb";
import { markPhaseOnce, recordCellLag } from "../lib/diag.ts";
import { noteTerminalReplicaTransition } from "../lib/terminalIncidentCaptureState.ts";
import {
  clearTerminalChunkTransfer,
  pushTerminalCellChunk,
} from "./terminal-stream-chunks.ts";
import {
  decodeTerminalWireFrame,
  foldTerminalFrame,
} from "./terminal-stream-frame-fold.ts";
import {
  clearTerminalSessionLiveness,
  terminalGenerationMatches,
} from "./terminal-stream-liveness.ts";
import {
  noteTerminalCellFrame,
  noteTerminalProofChunkProgress,
  requestTerminalResync,
} from "./terminal-stream-repair.ts";
import {
  emitTerminalViewStatus,
  notifyTerminalTransportStateChange,
  takePersistedTerminalRendererDrop,
  terminalDropNextFrames,
  terminalDroppedFrameCounts,
  terminalFrameCounts,
  terminalFullFrameCounts,
  terminalFullFrameScrollbackRows,
  terminalGridEpochs,
  terminalSessions,
} from "./terminal-stream-state.ts";
import type {
  TerminalGenerationToken,
  TerminalRendererSubscriber,
  TerminalSessionReplica,
} from "./terminal-stream-types.ts";

export {
  activeTerminalResyncView,
  clearTerminalSessionLiveness,
  terminalGenerationKey,
  terminalGenerationMatches,
  terminalGenerationToken,
} from "./terminal-stream-liveness.ts";
export {
  armTerminalForegroundIdleProbe,
  repairStaleTerminalSubscriberOnHeartbeat,
  requestTerminalDomReconcileRecovery,
  requestTerminalLivenessChallenge,
  sendLatchedTerminalResync,
} from "./terminal-stream-repair.ts";

/** Canonical replica frame, read by the terminal incident recorder so its
 *  evidence carries a viewport derived independently of the renderer. */
export function canonicalTerminalFrame(sessionId: string): CellGridFrame | null {
  return terminalSessions.get(sessionId)?.canonical ?? null;
}

/** A pane can detach while its replica lives; a pruned replica means the
 *  session itself is gone and its incident recorder owns nothing. */
export function hasTerminalSessionReplica(sessionId: string): boolean {
  return terminalSessions.has(sessionId);
}

export function installExpectedTerminalStream(
  session: TerminalSessionReplica,
  streamId: string,
  cols: number,
  rows: number,
): void {
  const priorBaselineReady = session.baselineReady;
  const streamChanged = session.expectedStreamId !== streamId;
  if (
    !streamChanged
    && session.effectiveCols === cols
    && session.effectiveRows === rows
  ) return;
  session.expectedStreamId = streamId;
  session.effectiveCols = cols;
  session.effectiveRows = rows;
  if (streamChanged) {
    clearTerminalSessionLiveness(session, "stream_replaced");
    session.requiresFreshBaseline = true;
    clearTerminalChunkTransfer(session);
  }
  session.baselineReady = !session.requiresFreshBaseline
    && !!session.canonical
    && session.canonical.streamId === streamId
    && session.canonical.cols === cols
    && session.canonical.rows === rows;
  if (session.baselineReady !== priorBaselineReady) notifyTerminalTransportStateChange();
}

function enqueueTerminalFrameForSubscriber(
  subscriber: TerminalRendererSubscriber,
  frame: CellGridFrame,
  canonical: CellGridFrame,
): void {
  subscriber.scheduler.setForeground(
    subscriber.viewActive && subscriber.isForeground(),
  );
  subscriber.scheduler.enqueue(frame, canonical);
}

export function deliverCanonicalToSubscriber(
  session: TerminalSessionReplica,
  subscriber: TerminalRendererSubscriber,
): void {
  if (!session.canonical || suppressNextRendererFrame(session)) return;
  enqueueTerminalFrameForSubscriber(subscriber, session.canonical, session.canonical);
}

export function dispatchTerminalCellFrame(
  pb: PbCellGridFrame,
  owner: TerminalGenerationToken,
): void {
  const session = terminalSessions.get(pb.sessionId);
  if (
    !session
    || pb.streamId !== session.expectedStreamId
    || !terminalGenerationMatches(session.generation, owner)
  ) return;
  acceptProtoFrame(session, pb, false, owner);
}

export function dispatchTerminalCellChunk(
  chunk: PbCellGridChunk,
  owner: TerminalGenerationToken,
): void {
  const part = chunk.part;
  if (!part) return;
  const session = terminalSessions.get(part.sessionId);
  if (
    !session
    || part.streamId !== session.expectedStreamId
    || !terminalGenerationMatches(session.generation, owner)
  ) return;
  pushTerminalCellChunk(
    session,
    chunk,
    (frame) => acceptProtoFrame(session, frame, true, owner),
    (reason) => requestTerminalResync(session, reason, owner, true, true),
    (progressChunk) => noteTerminalProofChunkProgress(session, progressChunk, owner),
  );
}

function suppressNextRendererFrame(session: TerminalSessionReplica): boolean {
  if (session.subscribers.size === 0) return false;
  const runtimeDrop = terminalDropNextFrames.delete(session.sessionId);
  const persistedDrop = takePersistedTerminalRendererDrop(session.sessionId);
  if (!runtimeDrop && !persistedDrop) return false;
  terminalDroppedFrameCounts.set(
    session.sessionId,
    (terminalDroppedFrameCounts.get(session.sessionId) ?? 0) + 1,
  );
  return true;
}

export function notifyTerminalBaselineState(session: TerminalSessionReplica): void {
  for (const view of session.handles.values()) {
    const status = view.status;
    if (
      status?.status !== "accepted"
      || !status.active
      || status.streamId !== session.expectedStreamId
    ) continue;
    emitTerminalViewStatus(view, { ...status, baselineReady: session.baselineReady });
  }
}

export function deliverTerminalCanonicalFull(session: TerminalSessionReplica): void {
  const canonical = session.canonical;
  if (!canonical) return;
  for (const subscriber of session.subscribers) {
    enqueueTerminalFrameForSubscriber(subscriber, canonical, canonical);
  }
}

function acceptFull(
  session: TerminalSessionReplica,
  frame: CellGridFrame,
  owner: TerminalGenerationToken,
  assembled: boolean,
): void {
  const result = foldTerminalFrame(session, frame);
  if (result.kind !== "full") {
    const reason = result.kind === "invalid" && result.reason === "invalid_full"
      ? "invalid full terminal baseline"
      : "terminal full conflicted with canonical state";
    const phase = result.kind === "invalid" && result.reason === "invalid_full"
      ? "invalid_full"
      : "full_conflict";
    noteTerminalReplicaTransition(session.sessionId, "replica_repair", frame, phase);
    requestTerminalResync(session, reason, owner, true, assembled);
    return;
  }
  session.canonical = frame;
  noteTerminalReplicaTransition(session.sessionId, "replica_admitted", frame, "full");
  session.baselineReady = true;
  session.requiresFreshBaseline = false;
  session.resyncLatched = false;
  session.resyncSentGeneration = null;
  session.resyncRetryGeneration = null;
  session.resyncRetryAtMs = null;
  noteTerminalCellFrame(session, frame, true, owner);
  notifyTerminalTransportStateChange();
  clearTerminalChunkTransfer(session);
  const suppressRendererDelivery = suppressNextRendererFrame(session);
  if (!suppressRendererDelivery) deliverTerminalCanonicalFull(session);
  notifyTerminalBaselineState(session);
}

function acceptDelta(
  session: TerminalSessionReplica,
  delta: CellGridFrame,
  owner: TerminalGenerationToken,
  assembled: boolean,
): void {
  const result = foldTerminalFrame(session, delta);
  if (result.kind !== "delta") {
    const reason = result.kind === "invalid" && result.reason === "delta_fold_rejected"
      ? "terminal delta fold rejected its canonical base"
      : "terminal delta did not follow the canonical baseline";
    const phase = result.kind === "invalid" && result.reason === "delta_fold_rejected"
      ? "delta_fold_rejected"
      : "delta_unfollowed";
    noteTerminalReplicaTransition(session.sessionId, "replica_repair", delta, phase);
    requestTerminalResync(session, reason, owner, true, assembled);
    return;
  }
  const folded = result.canonical;
  noteTerminalReplicaTransition(session.sessionId, "replica_admitted", folded, "delta");
  noteTerminalCellFrame(session, folded, false, owner);

  if (suppressNextRendererFrame(session)) return;

  for (const subscriber of session.subscribers) {
    enqueueTerminalFrameForSubscriber(subscriber, delta, folded);
  }
}

function noteWireFrame(session: TerminalSessionReplica, frame: PbCellGridFrame): void {
  session.wireStreamId = frame.streamId || null;
  session.wireGridEpoch = frame.gridEpoch || null;
  session.wireSeq = frame.seq <= BigInt(Number.MAX_SAFE_INTEGER)
    ? Number(frame.seq)
    : null;
  terminalFrameCounts.set(
    session.sessionId,
    (terminalFrameCounts.get(session.sessionId) ?? 0) + 1,
  );
  terminalGridEpochs.set(session.sessionId, frame.gridEpoch);
  if (frame.full) {
    terminalFullFrameCounts.set(
      session.sessionId,
      (terminalFullFrameCounts.get(session.sessionId) ?? 0) + 1,
    );
    terminalFullFrameScrollbackRows.set(session.sessionId, frame.scrollbackRows.length);
  }
}

function acceptProtoFrame(
  session: TerminalSessionReplica,
  pb: PbCellGridFrame,
  assembled: boolean,
  owner: TerminalGenerationToken,
): void {
  if (!terminalGenerationMatches(session.generation, owner)) return;
  if (pb.sessionId !== session.sessionId) {
    requestTerminalResync(
      session,
      "terminal frame session mismatch",
      owner,
      true,
      assembled,
    );
    return;
  }
  if (pb.streamId !== session.expectedStreamId) return;
  const decoded = decodeTerminalWireFrame(pb, assembled);
  if (decoded.kind === "invalid") {
    requestTerminalResync(session, decoded.reason, owner, true, assembled);
    return;
  }
  const frame = decoded.frame;
  noteWireFrame(session, pb);
  recordCellLag(pb, Date.now());
  markPhaseOnce("first_cell_receive", session.sessionId, {
    sessionId: session.sessionId,
    sequence: pb.seq,
    full: pb.full,
  });
  if (frame.full) acceptFull(session, frame, owner, assembled);
  else acceptDelta(session, frame, owner, assembled);
}
