// Owns shallow terminal payload shells and egress-only timing stamps for Sync v2.
// State retains one shell per recipient; egress fills only recipient scalar fields.
// Immutable row/span arrays remain shared with canonical terminal material.
// Snapshot cursors provide a stable stamp so every chunk keeps identical metadata.

import { toBinary } from "@bufbuild/protobuf";
import { CELL_GRID_COORD_FANOUT_STAMP_MAX } from "@roost/protocol/cell";
import {
  FirehoseFrameSchema,
  SyncDomain,
  type FirehoseFrame,
} from "@roost/protocol/proto/sync_pb";
import type {
  SyncV2QueuedFrame,
  SyncV2SocketState,
} from "./sync-ws-v2-state.ts";

export interface TerminalChunkTransfer {
  readonly sessionId: string;
  readonly snapshotId: string;
  readonly chunkIndex: number;
  readonly chunkCount: number;
  readonly transferMs: number;
}

export interface PreparedTerminalApplicationFrame {
  readonly frame: FirehoseFrame;
  readonly chunkTransfer: TerminalChunkTransfer | null;
}

function shallowMessageShell<T extends object>(message: T): T {
  return Object.assign(Object.create(Object.getPrototypeOf(message)), message) as T;
}

export function ownTerminalApplicationFrame(
  frame: FirehoseFrame,
  generation: bigint,
): FirehoseFrame {
  const owned = shallowMessageShell(frame);
  owned.deliverySeq = 0n;
  owned.domain = SyncDomain.TERMINAL;
  owned.domainGeneration = generation;
  switch (frame.frame.case) {
    case "cellGrid":
      owned.frame = {
        case: "cellGrid",
        value: shallowMessageShell(frame.frame.value),
      };
      break;
    case "cellGridChunk": {
      const chunk = shallowMessageShell(frame.frame.value);
      if (chunk.part) chunk.part = shallowMessageShell(chunk.part);
      owned.frame = { case: "cellGridChunk", value: chunk };
      break;
    }
    default:
      owned.frame = { ...frame.frame };
      break;
  }
  return owned;
}

export function conservativeTerminalApplicationFrameBytes(frame: FirehoseFrame): number {
  const cell = frame.frame.case === "cellGrid"
    ? frame.frame.value
    : frame.frame.case === "cellGridChunk"
      ? frame.frame.value.part
      : undefined;
  const priorDeliverySeq = frame.deliverySeq;
  const priorFanoutMs = cell?.coordFanoutMs ?? 0n;
  frame.deliverySeq = CELL_GRID_COORD_FANOUT_STAMP_MAX;
  if (cell) cell.coordFanoutMs = CELL_GRID_COORD_FANOUT_STAMP_MAX;
  try {
    return toBinary(FirehoseFrameSchema, frame).byteLength;
  } finally {
    frame.deliverySeq = priorDeliverySeq;
    if (cell) cell.coordFanoutMs = priorFanoutMs;
  }
}

export function prepareTerminalApplicationFrame(
  v2: SyncV2SocketState,
  queued: SyncV2QueuedFrame,
  deliverySeq: bigint,
  preparedAtMs: number,
): PreparedTerminalApplicationFrame {
  const frame = queued.frame;
  let fanoutMs = BigInt(preparedAtMs);
  if (frame.frame.case === "cellGridChunk") {
    const { sessionId, terminalCursorIndex, terminalStreamId } = queued.meta;
    const cursor = sessionId !== undefined && terminalCursorIndex !== undefined
      ? v2.terminalSessions.get(sessionId)?.cursor
      : null;
    if (
      cursor
      && cursor.source !== null
      && cursor.index === terminalCursorIndex
      && cursor.streamId === terminalStreamId
    ) {
      const snapshotFanoutMs = cursor.fanoutMs ?? fanoutMs;
      cursor.fanoutMs = snapshotFanoutMs;
      fanoutMs = snapshotFanoutMs;
    }
  }
  frame.deliverySeq = deliverySeq;
  if (frame.frame.case === "cellGrid") {
    frame.frame.value.coordFanoutMs = fanoutMs;
    return { frame, chunkTransfer: null };
  }
  if (frame.frame.case !== "cellGridChunk") {
    return { frame, chunkTransfer: null };
  }
  const chunk = frame.frame.value;
  const part = chunk.part;
  if (!part) return { frame, chunkTransfer: null };
  part.coordFanoutMs = fanoutMs;
  return {
    frame,
    chunkTransfer: {
      sessionId: part.sessionId,
      snapshotId: chunk.snapshotId,
      chunkIndex: chunk.chunkIndex,
      chunkCount: chunk.chunkCount,
      transferMs: Math.max(0, preparedAtMs - Number(fanoutMs)),
    },
  };
}
