// Wraps canonical terminal cells for Sync and exposes reusable lazy snapshot
// sources. TerminalScreenHub owns each immutable canonical full; socket cursors
// receive one materialized part at a time without duplicating the entire full.
import { create } from "@bufbuild/protobuf";
import { randomUUID } from "node:crypto";
import {
  CELL_GRID_COORD_FANOUT_STAMP_MAX,
  createCellGridSnapshotSource,
  type CellGridFrame,
  type CellGridSnapshotCursor,
  type CellGridSnapshotSource,
} from "@roost/protocol/cell";
import { type PbCellGridFrame } from "@roost/protocol/proto/cell_pb";
import {
  FirehoseFrameSchema,
  type FirehoseFrame,
} from "@roost/protocol/proto/sync_pb";

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

function reserveFanoutStamp(frame: PbCellGridFrame): PbCellGridFrame {
  // Snapshot planning includes the egress-only value that replaces this per recipient.
  return { ...frame, coordFanoutMs: CELL_GRID_COORD_FANOUT_STAMP_MAX };
}

export function terminalSnapshotSource(
  produceFull: () => PbCellGridFrame,
  lease?: TerminalSnapshotLease,
): TerminalSnapshotSource {
  let materializedSource: CellGridSnapshotSource | null = null;
  return {
    createCursor() {
      if (lease && !lease.acquire()) {
        throw new Error("terminal snapshot source is no longer resident");
      }
      try {
        // A cache may be superseded before scheduler admission; only a leased cursor may encode it.
        const source = materializedSource
          ?? createCellGridSnapshotSource(reserveFanoutStamp(produceFull()));
        materializedSource = source;
        return firehoseSnapshotCursor(source.createCursor(randomUUID()), lease);
      } catch (error) {
        lease?.release();
        throw error;
      }
    },
  };
}

/** Cache residency is viewport-only even when an older worker supplies a
 * history-bearing frame that was validated before canonical normalization. */
export function countTerminalScreenCacheSpans(frame: CellGridFrame): number {
  let spans = 0;
  for (const row of frame.viewportRows) spans += row.spans.length;
  return spans;
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
