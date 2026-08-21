import { create } from "@bufbuild/protobuf";
import {
  PbCellGridChunkSchema,
  type PbCellGridChunk,
  type PbCellGridFrame,
  type PbCellRow,
} from "../gen/roost/v1/cell_pb.ts";
import { isTerminalUuid } from "../viewport.ts";
import {
  CELL_GRID_PART_MAX_BYTES,
  CELL_GRID_SNAPSHOT_MAX_BYTES,
  CELL_GRID_SNAPSHOT_MAX_CHUNKS,
  assertCellGridSnapshot,
  createCellGridFramePart,
  encodedCellGridChunkSize,
  rejectCellGridChunk,
} from "./frame-chunk-validation.ts";

export {
  CELL_GRID_CHUNK_STALL_MS,
  CELL_GRID_PART_MAX_BYTES,
  CELL_GRID_SNAPSHOT_MAX_BYTES,
  CELL_GRID_SNAPSHOT_MAX_CHUNKS,
  CELL_GRID_SNAPSHOT_MAX_LINK_MAPPINGS,
  CELL_GRID_SNAPSHOT_MAX_ROWS,
  CELL_GRID_SNAPSHOT_MAX_SPANS,
  CellGridChunkError,
  assertCellGridSnapshot,
  decodeCellGridChunk,
  encodeCellGridChunk,
  encodedCellGridChunkSize,
  encodedCellGridFrameSize,
  type CellGridChunkErrorCode,
  type CellGridSnapshotStats,
} from "./frame-chunk-validation.ts";
export {
  CellGridChunkAssembler,
  type CellGridChunkAssemblyResult,
} from "./frame-chunk-assembler.ts";

interface SnapshotRow {
  kind: "history" | "viewport";
  row: PbCellRow;
}

function encodedCandidateSize(
  snapshotId: string,
  frame: PbCellGridFrame,
  entries: readonly SnapshotRow[],
): number {
  const viewport = entries.filter((entry) => entry.kind === "viewport").map((entry) => entry.row);
  const history = entries.filter((entry) => entry.kind === "history").map((entry) => entry.row);
  return encodedCellGridChunkSize(create(PbCellGridChunkSchema, {
    snapshotId,
    chunkIndex: CELL_GRID_SNAPSHOT_MAX_CHUNKS - 1,
    chunkCount: CELL_GRID_SNAPSHOT_MAX_CHUNKS,
    part: createCellGridFramePart(frame, viewport, history),
  }));
}

/** Deterministically split a complete full into whole-row bounded chunks. */
export function chunkCellGridFrame(
  frame: PbCellGridFrame,
  snapshotId: string,
): PbCellGridChunk[] {
  if (!isTerminalUuid(snapshotId)) {
    rejectCellGridChunk(
      "invalid-snapshot-id",
      `cell snapshot_id is not a UUID: ${JSON.stringify(snapshotId)}`,
    );
  }
  assertCellGridSnapshot(frame);
  const rows: SnapshotRow[] = [
    ...[...frame.scrollbackRows]
      .sort((a, b) => a.index - b.index)
      .map((row) => ({ kind: "history" as const, row })),
    ...[...frame.viewportRows]
      .sort((a, b) => a.index - b.index)
      .map((row) => ({ kind: "viewport" as const, row })),
  ];
  const groups: SnapshotRow[][] = [];
  let current: SnapshotRow[] = [];
  for (const entry of rows) {
    current.push(entry);
    if (encodedCandidateSize(snapshotId, frame, current) <= CELL_GRID_PART_MAX_BYTES) continue;
    current.pop();
    if (current.length === 0) {
      rejectCellGridChunk(
        "single-row-oversize",
        `cell snapshot ${entry.kind} row ${entry.row.index} cannot fit in ${CELL_GRID_PART_MAX_BYTES} bytes`,
      );
    }
    groups.push(current);
    current = [entry];
    if (encodedCandidateSize(snapshotId, frame, current) > CELL_GRID_PART_MAX_BYTES) {
      rejectCellGridChunk(
        "single-row-oversize",
        `cell snapshot ${entry.kind} row ${entry.row.index} cannot fit in ${CELL_GRID_PART_MAX_BYTES} bytes`,
      );
    }
  }
  if (current.length > 0) groups.push(current);
  if (groups.length === 0 || groups.length > CELL_GRID_SNAPSHOT_MAX_CHUNKS) {
    rejectCellGridChunk(
      "chunk-count",
      `cell snapshot requires ${groups.length} chunks; maximum is ${CELL_GRID_SNAPSHOT_MAX_CHUNKS}`,
    );
  }

  let totalBytes = 0;
  const chunks = groups.map((group, chunkIndex) => {
    const viewport = group.filter((entry) => entry.kind === "viewport").map((entry) => entry.row);
    const history = group.filter((entry) => entry.kind === "history").map((entry) => entry.row);
    const chunk = create(PbCellGridChunkSchema, {
      snapshotId,
      chunkIndex,
      chunkCount: groups.length,
      part: createCellGridFramePart(frame, viewport, history),
    });
    const bytes = encodedCellGridChunkSize(chunk);
    if (bytes > CELL_GRID_PART_MAX_BYTES) {
      rejectCellGridChunk("chunk-size", `cell snapshot chunk ${chunkIndex} is ${bytes} bytes`);
    }
    totalBytes += bytes;
    return chunk;
  });
  if (totalBytes > CELL_GRID_SNAPSHOT_MAX_BYTES) {
    rejectCellGridChunk(
      "snapshot-size",
      `cell snapshot is ${totalBytes} encoded bytes; maximum is ${CELL_GRID_SNAPSHOT_MAX_BYTES}`,
    );
  }
  return chunks;
}
