// This replica is the single canonical terminal grid shared by every view of a session.
// It accepts only generation-matched, contiguous frames before mutating canonical state.
// Sync dispatch calls it for full frames and chunks, while subscribers enqueue DOM work.
// Liveness repair is separate but consumes the same stream, epoch, sequence, and viewport facts.

import {
  CELL_GRID_PART_MAX_BYTES,
  applyDelta,
  encodedCellGridFrameSize,
  normalizeCellGridFrame,
  type CellGridFrame,
} from "@roost/shared/cell";
import { protoToCellFrame } from "@roost/shared/cell/cell-proto";
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
  clearTerminalSessionLiveness,
  terminalGenerationMatches,
} from "./terminal-stream-liveness.ts";
import {
  noteTerminalCellFrame,
  requestTerminalResync,
} from "./terminal-stream-repair.ts";
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
  clearTerminalSessionLiveness,
  terminalGenerationKey,
  terminalGenerationMatches,
  terminalGenerationToken,
} from "./terminal-stream-liveness.ts";
export {
  armTerminalForegroundIdleProbe,
  repairStaleTerminalSubscriberOnHeartbeat,
  requestTerminalLivenessChallenge,
  sendLatchedTerminalResync,
} from "./terminal-stream-repair.ts";

export function installExpectedTerminalStream(
  session: TerminalSessionReplica,
  streamId: string,
  cols: number,
  rows: number,
): void {
  const streamChanged = session.expectedStreamId !== streamId;
  const streamReplaced = streamChanged && session.expectedStreamId !== null;
  if (
    !streamChanged
    && session.effectiveCols === cols
    && session.effectiveRows === rows
  ) return;
  session.expectedStreamId = streamId;
  session.effectiveCols = cols;
  session.effectiveRows = rows;
  if (streamChanged) {
    if (streamReplaced) clearTerminalSessionLiveness(session, "stream_replaced");
    session.requiresFreshBaseline = true;
    clearTerminalChunkTransfer(session);
  }
  session.baselineReady = !session.requiresFreshBaseline
    && !!session.canonical
    && session.canonical.streamId === streamId
    && session.canonical.cols === cols
    && session.canonical.rows === rows;
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
    (reason) => requestTerminalResync(session, reason, owner),
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
    enqueueTerminalFrameForSubscriber(subscriber, canonical, canonical);
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


function acceptFull(
  session: TerminalSessionReplica,
  frame: CellGridFrame,
  owner: TerminalGenerationToken,
): void {
  if (!validFull(session, frame)) {
    requestTerminalResync(session, "invalid full terminal baseline", owner);
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
  noteTerminalCellFrame(session, true, owner);
  clearTerminalChunkTransfer(session);
  const suppressRendererDelivery = suppressNextRendererFrame(session);
  if (!suppressRendererDelivery) deliverFull(session);
  notifyBaselineState(session);
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
      owner,
    );
    return;
  }

  const folded = applyDelta(base, delta);
  if (!folded) {
    requestTerminalResync(
      session,
      "terminal delta fold rejected its canonical base",
      owner,
    );
    return;
  }
  normalizeCellGridFrame(folded);
  session.canonical = folded;
  noteTerminalCellFrame(session, false, owner);

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
    requestTerminalResync(session, "terminal frame session mismatch", owner);
    return;
  }
  if (pb.streamId !== session.expectedStreamId) return;
  if (!assembled && encodedCellGridFrameSize(pb) > CELL_GRID_PART_MAX_BYTES) {
    requestTerminalResync(
      session,
      "terminal frame exceeded the encoded part ceiling",
      owner,
    );
    return;
  }
  let frame: CellGridFrame;
  try {
    frame = protoToCellFrame(pb);
  } catch (error) {
    requestTerminalResync(session, String(error), owner);
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
