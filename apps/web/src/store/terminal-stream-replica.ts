// This replica is the single canonical terminal grid shared by every view of a session.
// It accepts only generation-matched, contiguous frames before mutating renderer state.
// Sync dispatch calls it for full frames and chunks, while view handles subscribe to deliveries.
// Liveness repair is separate but consumes the same stream, epoch, sequence, and viewport facts.

import {
  CELL_GRID_PART_MAX_BYTES,
  applyDelta,
  cloneCellGridFrame,
  encodedCellGridFrameSize,
  type CellGridFrame,
} from "@roost/shared/cell";
import { protoToCellFrame } from "@roost/shared/cell/cell-proto";
import { diag, isDiagEnabled } from "@roost/shared/diag";
import type {
  PbCellGridChunk,
  PbCellGridFrame,
} from "@roost/shared/proto/cell_pb";
import { markPhaseOnce, recordCellLag } from "../lib/diag.ts";
import {
  clearTerminalChunkTransfer,
  pushTerminalCellChunk,
} from "./terminal-stream-chunks.ts";
import {
  armTerminalForegroundIdleProbe,
  clearTerminalRepairLatch,
  clearTerminalSessionLiveness,
  requestTerminalResync,
  terminalGenerationMatches,
} from "./terminal-stream-liveness.ts";
import {
  emitTerminalViewStatus,
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
  armTerminalForegroundIdleProbe,
  clearTerminalSessionLiveness,
  repairStaleTerminalSubscriberOnHeartbeat,
  requestTerminalLivenessChallenge,
  sendLatchedTerminalResync,
  terminalGenerationKey,
  terminalGenerationMatches,
  terminalGenerationToken,
} from "./terminal-stream-liveness.ts";

export function installExpectedTerminalStream(
  session: TerminalSessionReplica,
  streamId: string,
  cols: number,
  rows: number,
): void {
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
}

export function applyTerminalFrameToSubscriber(
  subscriber: TerminalRendererSubscriber,
  frame: CellGridFrame,
): boolean {
  const diagnostics = isDiagEnabled();
  const startedAt = diagnostics ? performance.now() : 0;
  const applied = frame.full
    ? subscriber.renderer.applyFullFrame(frame)
    : subscriber.renderer.applyDeltaFrame(frame);
  if (!applied) return false;
  if (diagnostics) {
    diag("cell.apply_dur", {
      sid: subscriber.sessionId,
      seq: frame.seq,
      full: frame.full,
      dur_ms: performance.now() - startedAt,
    });
  }
  subscriber.streamId = frame.streamId;
  subscriber.gridEpoch = frame.gridEpoch;
  subscriber.seq = frame.seq;
  subscriber.onDelivery?.({ frame, full: frame.full });
  return true;
}

export function deliverCanonicalToSubscriber(
  session: TerminalSessionReplica,
  subscriber: TerminalRendererSubscriber,
): void {
  if (!session.canonical || suppressNextRendererFrame(session)) return;
  applyTerminalFrameToSubscriber(subscriber, cloneCellGridFrame(session.canonical));
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
    (reason) => requestTerminalResync(session, reason, "initial", owner),
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

function notifyBaselineState(session: TerminalSessionReplica): void {
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

function deliverFull(session: TerminalSessionReplica): void {
  const canonical = session.canonical;
  if (!canonical) return;
  for (const subscriber of session.subscribers) {
    applyTerminalFrameToSubscriber(subscriber, cloneCellGridFrame(canonical));
  }
}

function validFull(session: TerminalSessionReplica, frame: CellGridFrame): boolean {
  if (
    !frame.full
    || frame.baseSeq !== 0
    || frame.streamId !== session.expectedStreamId
    || frame.cols !== session.effectiveCols
    || frame.rows !== session.effectiveRows
    || frame.viewportRows.length !== frame.rows
    || frame.scrollbackAppend.length !== 0
  ) return false;
  for (let index = 0; index < frame.rows; index++) {
    if (frame.viewportRows[index]?.index !== index) return false;
  }
  let historyIndex = frame.sbBase;
  for (const row of frame.scrollbackRows) {
    if (row.index !== historyIndex || row.index >= frame.scrollbackTotal) return false;
    historyIndex++;
  }
  if (historyIndex !== frame.scrollbackTotal) return false;
  return true;
}

function recordAcceptedTerminalFrame(
  session: TerminalSessionReplica,
  full: boolean,
  owner: TerminalGenerationToken,
): void {
  if (!terminalGenerationMatches(session.generation, owner)) return;
  session.lastAcceptedFrameAtMs = performance.now();
  session.lastAcceptedFrameGeneration = session.generation;
  clearTimeout(session.proofDeadlineTimer ?? undefined);
  session.proofDeadlineTimer = null;
  session.proofChallengeAtMs = null;
  session.proofChallengeGeneration = null;
  session.repairOutcome = "proved";
  if (full) {
    clearTerminalRepairLatch(session);
  } else if (session.resyncLatched) {
    // A delta proves the lane is live but cannot repair the canonical gap.
    // Keep retrying the latch; do not escalate a challenge that received proof.
    session.resyncLatchedAtMs = null;
  }
  armTerminalForegroundIdleProbe(session);
}

function acceptFull(
  session: TerminalSessionReplica,
  frame: CellGridFrame,
  owner: TerminalGenerationToken,
): void {
  if (!validFull(session, frame)) {
    requestTerminalResync(session, "invalid full terminal baseline", "initial", owner);
    return;
  }
  frame.full = true;
  frame.baseSeq = 0;
  frame.scrollbackAppend = [];
  session.canonical = frame;
  session.baselineReady = true;
  session.requiresFreshBaseline = false;
  session.resyncLatched = false;
  session.resyncSentGeneration = null;
  session.resyncRetryGeneration = null;
  session.resyncRetryAtMs = null;
  recordAcceptedTerminalFrame(session, true, owner);
  clearTerminalChunkTransfer(session);
  const suppressRendererDelivery = suppressNextRendererFrame(session);
  if (!suppressRendererDelivery) deliverFull(session);
  notifyBaselineState(session);
  markPhaseOnce("first_cell_apply", session.sessionId, {
    sessionId: session.sessionId,
    sequence: frame.seq,
    full: true,
  });
}

function acceptDelta(
  session: TerminalSessionReplica,
  delta: CellGridFrame,
  owner: TerminalGenerationToken,
): void {
  const base = session.canonical;
  if (
    delta.full
    || !session.baselineReady
    || !base
    || session.assembler.activeSnapshotId !== null
    || delta.streamId !== session.expectedStreamId
    || base.streamId !== session.expectedStreamId
    || delta.gridEpoch !== base.gridEpoch
    || delta.cols !== session.effectiveCols
    || delta.rows !== session.effectiveRows
    || delta.baseSeq !== base.seq
    || delta.seq !== delta.baseSeq + 1
  ) {
    requestTerminalResync(
      session,
      "terminal delta did not follow the canonical baseline",
      "initial",
      owner,
    );
    return;
  }

  const eligible = new Map<TerminalRendererSubscriber, CellGridFrame>();
  for (const subscriber of session.subscribers) {
    if (
      subscriber.streamId === delta.streamId
      && subscriber.gridEpoch === delta.gridEpoch
      && subscriber.seq === delta.baseSeq
    ) eligible.set(subscriber, cloneCellGridFrame(delta));
  }

  const folded = applyDelta(base, delta);
  if (!folded) {
    requestTerminalResync(
      session,
      "terminal delta fold rejected its canonical base",
      "initial",
      owner,
    );
    return;
  }
  folded.full = true;
  folded.baseSeq = 0;
  folded.scrollbackRows = [];
  folded.scrollbackAppend = [];
  folded.sbBase = folded.scrollbackTotal;
  session.canonical = folded;
  recordAcceptedTerminalFrame(session, false, owner);

  if (suppressNextRendererFrame(session)) return;

  for (const subscriber of session.subscribers) {
    const sparse = eligible.get(subscriber);
    if (sparse && applyTerminalFrameToSubscriber(subscriber, sparse)) continue;
    applyTerminalFrameToSubscriber(subscriber, cloneCellGridFrame(folded));
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
    requestTerminalResync(session, "terminal frame session mismatch", "initial", owner);
    return;
  }
  if (pb.streamId !== session.expectedStreamId) return;
  if (!assembled && encodedCellGridFrameSize(pb) > CELL_GRID_PART_MAX_BYTES) {
    requestTerminalResync(
      session,
      "terminal frame exceeded the encoded part ceiling",
      "initial",
      owner,
    );
    return;
  }
  let frame: CellGridFrame;
  try {
    frame = protoToCellFrame(pb);
  } catch (error) {
    requestTerminalResync(session, String(error), "initial", owner);
    return;
  }
  noteWireFrame(session, pb);
  recordCellLag(pb, Date.now());
  markPhaseOnce("first_cell_receive", session.sessionId, {
    sessionId: session.sessionId,
    sequence: pb.seq,
    full: pb.full,
  });
  if (frame.full) acceptFull(session, frame, owner);
  else acceptDelta(session, frame, owner);
}
