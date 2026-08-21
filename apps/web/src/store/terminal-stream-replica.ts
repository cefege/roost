import { create } from "@bufbuild/protobuf";
import {
  CELL_GRID_PART_MAX_BYTES,
  applyDelta,
  cloneCellGridFrame,
  encodedCellGridFrameSize,
  type CellGridFrame,
} from "@roost/shared/cell";
import { protoToCellFrame } from "@roost/shared/cell/cell-proto";
import type {
  PbCellGridChunk,
  PbCellGridFrame,
} from "@roost/shared/proto/cell_pb";
import { TerminalResyncCommandSchema } from "@roost/shared/proto/sync_pb";
import { signal } from "@roost/shared/diag";
import { markPhaseOnce, recordCellLag } from "../lib/diag.ts";
import {
  currentSyncV2TerminalState,
  sendSyncV2Command,
  type SyncV2TerminalState,
} from "./sync.ts";
import {
  clearTerminalChunkTransfer,
  pushTerminalCellChunk,
} from "./terminal-stream-chunks.ts";
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
  TerminalOutboundCommand,
  TerminalRendererSubscriber,
  TerminalSessionReplica,
  TerminalViewRecord,
} from "./terminal-stream-types.ts";

export function terminalGenerationKey(state: SyncV2TerminalState): string {
  return `${state.socketId}:${state.domainGeneration}`;
}

export function activeTerminalResyncView(
  session: TerminalSessionReplica,
): TerminalViewRecord | null {
  for (const view of session.handles.values()) {
    if (!view.disposed && view.desired?.active) return view;
  }
  return null;
}

export function sendLatchedTerminalResync(session: TerminalSessionReplica): void {
  if (!session.resyncLatched || !session.expectedStreamId) return;
  const sync = currentSyncV2TerminalState();
  const view = activeTerminalResyncView(session);
  if (!sync?.ready || !view) return;
  const key = terminalGenerationKey(sync);
  if (session.resyncSentGeneration === key) return;
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
  if (sendSyncV2Command(outbound)) session.resyncSentGeneration = key;
}

function requestTerminalResync(
  session: TerminalSessionReplica,
  reason: string,
): void {
  clearTerminalChunkTransfer(session);
  if (!session.resyncLatched) {
    session.resyncLatched = true;
    signal("cell.seq_gap", {
      sid: session.sessionId,
      stream_id: session.expectedStreamId,
      reason: reason.slice(0, 200),
      cooldownKey: session.sessionId,
    });
  }
  sendLatchedTerminalResync(session);
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

/** An exact view heartbeat is also the renderer's applied-sequence proof.
 * The session replica may be ahead when a renderer rejected or a smoke probe
 * deliberately suppressed one delivery. Repair through the ordinary
 * coordinator rebaseline path rather than copying around that path locally. */
export function repairStaleTerminalSubscriberOnHeartbeat(
  session: TerminalSessionReplica,
): void {
  const canonical = session.canonical;
  if (
    !session.baselineReady
    || !canonical
    || session.assembler.activeSnapshotId !== null
  ) return;
  for (const subscriber of session.subscribers) {
    if (
      subscriber.streamId !== canonical.streamId
      || subscriber.gridEpoch !== canonical.gridEpoch
      || subscriber.seq !== canonical.seq
    ) {
      requestTerminalResync(
        session,
        "terminal renderer applied sequence trailed the canonical replica at heartbeat",
      );
      return;
    }
  }
}

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
    session.requiresFreshBaseline = true;
    clearTerminalChunkTransfer(session);
    session.resyncLatched = false;
    session.resyncSentGeneration = null;
  }
  session.baselineReady = !session.requiresFreshBaseline
    && !!session.canonical
    && session.canonical.streamId === streamId
    && session.canonical.cols === cols
    && session.canonical.rows === rows;
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

export function applyTerminalFrameToSubscriber(
  subscriber: TerminalRendererSubscriber,
  frame: CellGridFrame,
): boolean {
  const applied = frame.full
    ? subscriber.renderer.applyFullFrame(frame)
    : subscriber.renderer.applyDeltaFrame(frame);
  if (!applied) return false;
  subscriber.streamId = frame.streamId;
  subscriber.gridEpoch = frame.gridEpoch;
  subscriber.seq = frame.seq;
  subscriber.onDelivery?.({ frame, full: frame.full });
  return true;
}

function deliverFull(session: TerminalSessionReplica): void {
  const canonical = session.canonical;
  if (!canonical) return;
  for (const subscriber of session.subscribers) {
    applyTerminalFrameToSubscriber(subscriber, cloneCellGridFrame(canonical));
  }
}
export function deliverCanonicalToSubscriber(
  session: TerminalSessionReplica,
  subscriber: TerminalRendererSubscriber,
): void {
  if (!session.canonical || suppressNextRendererFrame(session)) return;
  applyTerminalFrameToSubscriber(subscriber, cloneCellGridFrame(session.canonical));
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

function acceptFull(session: TerminalSessionReplica, frame: CellGridFrame): void {
  if (!validFull(session, frame)) {
    requestTerminalResync(session, "invalid full terminal baseline");
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

function acceptDelta(session: TerminalSessionReplica, delta: CellGridFrame): void {
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
    requestTerminalResync(session, "terminal delta did not follow the canonical baseline");
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
    requestTerminalResync(session, "terminal delta fold rejected its canonical base");
    return;
  }
  folded.full = true;
  folded.baseSeq = 0;
  folded.scrollbackRows = [];
  folded.scrollbackAppend = [];
  folded.sbBase = folded.scrollbackTotal;
  session.canonical = folded;

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
): void {
  if (pb.sessionId !== session.sessionId) {
    requestTerminalResync(session, "terminal frame session mismatch");
    return;
  }
  if (pb.streamId !== session.expectedStreamId) return;
  if (!assembled && encodedCellGridFrameSize(pb) > CELL_GRID_PART_MAX_BYTES) {
    requestTerminalResync(session, "terminal frame exceeded the encoded part ceiling");
    return;
  }
  let frame: CellGridFrame;
  try {
    frame = protoToCellFrame(pb);
  } catch (error) {
    requestTerminalResync(session, String(error));
    return;
  }
  noteWireFrame(session, pb);
  recordCellLag(pb, Date.now());
  markPhaseOnce("first_cell_receive", session.sessionId, {
    sessionId: session.sessionId,
    sequence: pb.seq,
    full: pb.full,
  });
  if (frame.full) acceptFull(session, frame);
  else acceptDelta(session, frame);
}

export function dispatchTerminalCellFrame(pb: PbCellGridFrame): void {
  const session = terminalSessions.get(pb.sessionId);
  if (!session || pb.streamId !== session.expectedStreamId) return;
  acceptProtoFrame(session, pb, false);
}

export function dispatchTerminalCellChunk(chunk: PbCellGridChunk): void {
  const part = chunk.part;
  if (!part) return;
  const session = terminalSessions.get(part.sessionId);
  if (!session || part.streamId !== session.expectedStreamId) return;
  pushTerminalCellChunk(
    session,
    chunk,
    (frame) => acceptProtoFrame(session, frame, true),
    (reason) => requestTerminalResync(session, reason),
  );
}
