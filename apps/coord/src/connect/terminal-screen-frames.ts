import { clone, create } from "@bufbuild/protobuf";
import { randomUUID } from "node:crypto";
import {
  assertCellGridSnapshot,
  CELL_GRID_PART_MAX_BYTES,
  chunkCellGridFrame,
  SB_RENEWAL_HISTORY_ROWS,
  encodedCellGridFrameSize,
  type CellGridFrame,
} from "@roost/shared/cell";
import {
  PbCellGridFrameSchema,
  type PbCellGridFrame,
} from "@roost/shared/proto/cell_pb";
import {
  FirehoseFrameSchema,
  type FirehoseFrame,
} from "@roost/shared/proto/sync_pb";

export function cellGridEnvelope(frame: PbCellGridFrame): FirehoseFrame {
  return create(FirehoseFrameSchema, {
    frame: { case: "cellGrid", value: frame },
  });
}

export function terminalSnapshotFrames(
  full: PbCellGridFrame,
): readonly FirehoseFrame[] {
  assertCellGridSnapshot(full);
  if (encodedCellGridFrameSize(full) <= CELL_GRID_PART_MAX_BYTES) {
    return [cellGridEnvelope(clone(PbCellGridFrameSchema, full))];
  }
  return chunkCellGridFrame(full, randomUUID()).map((chunk) =>
    create(FirehoseFrameSchema, {
      frame: { case: "cellGridChunk", value: chunk },
    }));
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
