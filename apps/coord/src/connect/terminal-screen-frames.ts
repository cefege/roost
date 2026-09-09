// Wraps canonical terminal cells for Sync and exposes reusable lazy snapshot
// sources. TerminalScreenHub owns each immutable canonical full; socket cursors
// receive one materialized part at a time without duplicating the entire full.
import { create } from "@bufbuild/protobuf";
import { randomUUID } from "node:crypto";
import {
  SB_RENEWAL_HISTORY_ROWS,
  createCellGridSnapshotSource,
  type CellGridFrame,
  type CellGridSnapshotCursor,
} from "@roost/shared/cell";
import { type PbCellGridFrame } from "@roost/shared/proto/cell_pb";
import {
  FirehoseFrameSchema,
  type FirehoseFrame,
} from "@roost/shared/proto/sync_pb";

export function cellGridEnvelope(frame: PbCellGridFrame): FirehoseFrame {
  return create(FirehoseFrameSchema, {
    frame: { case: "cellGrid", value: frame },
  });
}

export interface TerminalSnapshotLease {
  acquire(): boolean;
  release(): void;
}

export interface TerminalSnapshotCursor {
  readonly partCount: number;
  materialize(partIndex: number): FirehoseFrame;
  release(): void;
}

export interface TerminalSnapshotSource {
  createCursor(): TerminalSnapshotCursor;
}

function firehoseSnapshotCursor(
  cursor: CellGridSnapshotCursor,
  lease: TerminalSnapshotLease | undefined,
): TerminalSnapshotCursor {
  let released = false;
  return {
    partCount: cursor.partCount,
    materialize(partIndex) {
      if (released) throw new Error("terminal snapshot cursor has been released");
      const part = cursor.materialize(partIndex);
      return part.kind === "frame"
        ? cellGridEnvelope(part.value)
        : create(FirehoseFrameSchema, {
          frame: { case: "cellGridChunk", value: part.value },
        });
    },
    release() {
      if (released) return;
      released = true;
      lease?.release();
    },
  };
}

export function terminalSnapshotSource(
  full: PbCellGridFrame,
  lease?: TerminalSnapshotLease,
): TerminalSnapshotSource {
  const source = createCellGridSnapshotSource(full);
  return {
    createCursor() {
      if (lease && !lease.acquire()) {
        throw new Error("terminal snapshot source is no longer resident");
      }
      try {
        return firehoseSnapshotCursor(source.createCursor(randomUUID()), lease);
      } catch (error) {
        lease?.release();
        throw error;
      }
    },
  };
}

export function countCellGridSpans(frame: CellGridFrame): number {
  let spans = 0;
  for (const row of frame.scrollbackRows) spans += row.spans.length;
  for (const row of frame.viewportRows) spans += row.spans.length;
  return spans;
}

export function countCellGridRows(frame: CellGridFrame): number {
  return frame.scrollbackRows.length + frame.rows;
}

export function normalizeCellGridFrame(frame: CellGridFrame): CellGridFrame {
  frame.full = true;
  frame.baseSeq = 0;
  frame.scrollbackAppend = [];
  if (frame.scrollbackRows.length > SB_RENEWAL_HISTORY_ROWS) {
    frame.scrollbackRows = frame.scrollbackRows.slice(-SB_RENEWAL_HISTORY_ROWS);
  }
  frame.sbBase = frame.scrollbackRows[0]?.index ?? frame.scrollbackTotal;
  return frame;
}

export interface TerminalScreenSnapshot {
  streamId: string;
  gridEpoch: string;
  seq: number;
  cols: number;
  rows: number;
  valid: boolean;
}

export function terminalScreenSnapshot(
  expected: { streamId: string } | null | undefined,
  cache: { frame: CellGridFrame; valid: boolean } | null | undefined,
): TerminalScreenSnapshot | null {
  if (!expected || !cache) return null;
  return {
    streamId: expected.streamId,
    gridEpoch: cache.frame.gridEpoch,
    seq: cache.frame.seq,
    cols: cache.frame.cols,
    rows: cache.frame.rows,
    valid: cache.valid,
  };
}
