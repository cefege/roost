// Plans and materializes bounded cell-grid snapshot parts from one immutable
// canonical full. Terminal coordinators retain the source and materialize only
// the next cursor part; worker and browser callers may still request every
// deterministic chunk through chunkCellGridFrame.
import { clone, create } from "@bufbuild/protobuf";
import {
  PbCellGridChunkSchema,
  PbCellGridFrameSchema,
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
  encodedCellGridFrameSize,
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

interface PlannedSnapshotPart {
  readonly entries: readonly SnapshotRow[];
  readonly encodedPartBytes: number;
}

export type CellGridSnapshotPart =
  | { readonly kind: "frame"; readonly value: PbCellGridFrame }
  | { readonly kind: "chunk"; readonly value: PbCellGridChunk };

export interface CellGridSnapshotCursor {
  readonly partCount: number;
  materialize(partIndex: number): CellGridSnapshotPart;
}

/**
 * One validated immutable full plus its deterministic part plan. A cursor owns
 * its snapshot UUID, so many recipients can stream the same canonical source.
 */
export interface CellGridSnapshotSource {
  readonly partCount: number;
  createCursor(snapshotId: string): CellGridSnapshotCursor;
}

export interface CreateCellGridSnapshotSourceOptions {
  /** Preserve chunkCellGridFrame's historical one-chunk result for small fulls. */
  readonly forceChunking?: boolean;
}

const PLANNING_SNAPSHOT_ID = "00000000-0000-4000-8000-000000000000";

function encodedVarintBytes(value: number): number {
  let remaining = value;
  let bytes = 1;
  while (remaining >= 0x80) {
    remaining = Math.floor(remaining / 0x80);
    bytes++;
  }
  return bytes;
}

function snapshotRows(frame: PbCellGridFrame): SnapshotRow[] {
  const historyByIndex = new Map(frame.scrollbackRows.map((row) => [row.index, row]));
  const viewportByIndex = new Map(frame.viewportRows.map((row) => [row.index, row]));
  const rows: SnapshotRow[] = [];
  const firstHistoryIndex = Number(frame.sbBase);
  for (let offset = 0; offset < frame.scrollbackRows.length; offset++) {
    const row = historyByIndex.get(firstHistoryIndex + offset);
    if (!row) throw new Error("validated cell snapshot history changed during planning");
    rows.push({ kind: "history", row });
  }
  for (let index = 0; index < frame.rows; index++) {
    const row = viewportByIndex.get(index);
    if (!row) throw new Error("validated cell snapshot viewport changed during planning");
    rows.push({ kind: "viewport", row });
  }
  return rows;
}


function rowContribution(
  frame: PbCellGridFrame,
  emptyPartBytes: number,
  entry: SnapshotRow,
): number {
  const onlyRow = entry.kind === "history"
    ? createCellGridFramePart(frame, [], [entry.row])
    : createCellGridFramePart(frame, [entry.row], []);
  return encodedCellGridFrameSize(onlyRow) - emptyPartBytes;
}

function chunkHeaderBytes(snapshotId: string, chunkIndex: number, chunkCount: number): number {
  const header = create(PbCellGridChunkSchema, { snapshotId, chunkIndex, chunkCount });
  return encodedCellGridChunkSize(header);
}

function partFieldTagBytes(
  emptyFramePart: PbCellGridFrame,
  emptyPartBytes: number,
  headerBytes: number,
): number {
  const encoded = encodedCellGridChunkSize(create(PbCellGridChunkSchema, {
    snapshotId: PLANNING_SNAPSHOT_ID,
    chunkIndex: CELL_GRID_SNAPSHOT_MAX_CHUNKS - 1,
    chunkCount: CELL_GRID_SNAPSHOT_MAX_CHUNKS,
    part: emptyFramePart,
  }));
  return encoded - headerBytes - encodedVarintBytes(emptyPartBytes) - emptyPartBytes;
}

function encodedChunkBytes(
  headerBytes: number,
  partTagBytes: number,
  encodedPartBytes: number,
): number {
  return headerBytes + partTagBytes + encodedVarintBytes(encodedPartBytes) + encodedPartBytes;
}

function planSnapshotParts(frame: PbCellGridFrame): PlannedSnapshotPart[] {
  const basePart = createCellGridFramePart(frame, [], []);
  const basePartBytes = encodedCellGridFrameSize(basePart);
  const largestHeaderBytes = chunkHeaderBytes(
    PLANNING_SNAPSHOT_ID,
    CELL_GRID_SNAPSHOT_MAX_CHUNKS - 1,
    CELL_GRID_SNAPSHOT_MAX_CHUNKS,
  );
  const tagBytes = partFieldTagBytes(basePart, basePartBytes, largestHeaderBytes);
  const parts: PlannedSnapshotPart[] = [];
  let entries: SnapshotRow[] = [];
  let encodedPartBytes = basePartBytes;

  for (const entry of snapshotRows(frame)) {
    const contribution = rowContribution(frame, basePartBytes, entry);
    const nextPartBytes = encodedPartBytes + contribution;
    if (encodedChunkBytes(largestHeaderBytes, tagBytes, nextPartBytes) <= CELL_GRID_PART_MAX_BYTES) {
      entries.push(entry);
      encodedPartBytes = nextPartBytes;
      continue;
    }
    if (entries.length === 0) {
      rejectCellGridChunk(
        "single-row-oversize",
        `cell snapshot ${entry.kind} row ${entry.row.index} cannot fit in ${CELL_GRID_PART_MAX_BYTES} bytes`,
      );
    }
    parts.push({ entries, encodedPartBytes });
    entries = [entry];
    encodedPartBytes = basePartBytes + contribution;
    if (encodedChunkBytes(largestHeaderBytes, tagBytes, encodedPartBytes) > CELL_GRID_PART_MAX_BYTES) {
      rejectCellGridChunk(
        "single-row-oversize",
        `cell snapshot ${entry.kind} row ${entry.row.index} cannot fit in ${CELL_GRID_PART_MAX_BYTES} bytes`,
      );
    }
  }
  if (entries.length > 0) parts.push({ entries, encodedPartBytes });
  if (parts.length === 0 || parts.length > CELL_GRID_SNAPSHOT_MAX_CHUNKS) {
    rejectCellGridChunk(
      "chunk-count",
      `cell snapshot requires ${parts.length} chunks; maximum is ${CELL_GRID_SNAPSHOT_MAX_CHUNKS}`,
    );
  }

  const totalBytes = parts.reduce((total, part, index) => total + encodedChunkBytes(
    chunkHeaderBytes(PLANNING_SNAPSHOT_ID, index, parts.length),
    tagBytes,
    part.encodedPartBytes,
  ), 0);
  if (totalBytes > CELL_GRID_SNAPSHOT_MAX_BYTES) {
    rejectCellGridChunk(
      "snapshot-size",
      `cell snapshot is ${totalBytes} encoded bytes; maximum is ${CELL_GRID_SNAPSHOT_MAX_BYTES}`,
    );
  }
  return parts;
}

class PlannedCellGridSnapshotSource implements CellGridSnapshotSource {
  readonly partCount: number;

  constructor(
    private readonly frame: PbCellGridFrame,
    private readonly parts: readonly PlannedSnapshotPart[] | null,
  ) {
    this.partCount = parts?.length ?? 1;
  }

  createCursor(snapshotId: string): CellGridSnapshotCursor {
    if (this.parts && !isTerminalUuid(snapshotId)) {
      rejectCellGridChunk(
        "invalid-snapshot-id",
        `cell snapshot_id is not a UUID: ${JSON.stringify(snapshotId)}`,
      );
    }
    return {
      partCount: this.partCount,
      materialize: (partIndex) => this.materialize(snapshotId, partIndex),
    };
  }

  private materialize(snapshotId: string, partIndex: number): CellGridSnapshotPart {
    if (!Number.isInteger(partIndex) || partIndex < 0 || partIndex >= this.partCount) {
      rejectCellGridChunk(
        "chunk-index",
        `cell snapshot part index ${partIndex} is outside 0..${this.partCount - 1}`,
      );
    }
    if (!this.parts) {
      return { kind: "frame", value: clone(PbCellGridFrameSchema, this.frame) };
    }
    const plan = this.parts[partIndex]!;
    const viewportRows: PbCellRow[] = [];
    const scrollbackRows: PbCellRow[] = [];
    for (const entry of plan.entries) {
      if (entry.kind === "history") scrollbackRows.push(entry.row);
      else viewportRows.push(entry.row);
    }
    const chunk = create(PbCellGridChunkSchema, {
      snapshotId,
      chunkIndex: partIndex,
      chunkCount: this.partCount,
      part: createCellGridFramePart(this.frame, viewportRows, scrollbackRows),
    });
    const bytes = encodedCellGridChunkSize(chunk);
    if (bytes > CELL_GRID_PART_MAX_BYTES) {
      rejectCellGridChunk("chunk-size", `cell snapshot chunk ${partIndex} is ${bytes} bytes`);
    }
    return { kind: "chunk", value: chunk };
  }
}

/**
 * Validate once, plan whole-row boundaries once, then materialize only a
 * cursor's requested part. The caller must not mutate the canonical frame.
 */
export function createCellGridSnapshotSource(
  frame: PbCellGridFrame,
  options: CreateCellGridSnapshotSourceOptions = {},
): CellGridSnapshotSource {
  assertCellGridSnapshot(frame);
  if (!options.forceChunking && encodedCellGridFrameSize(frame) <= CELL_GRID_PART_MAX_BYTES) {
    return new PlannedCellGridSnapshotSource(frame, null);
  }
  return new PlannedCellGridSnapshotSource(frame, planSnapshotParts(frame));
}

/** Deterministically split a complete full into whole-row bounded chunks. */
export function chunkCellGridFrame(
  frame: PbCellGridFrame,
  snapshotId: string,
): PbCellGridChunk[] {
  const cursor = createCellGridSnapshotSource(frame, { forceChunking: true })
    .createCursor(snapshotId);
  const chunks: PbCellGridChunk[] = [];
  for (let partIndex = 0; partIndex < cursor.partCount; partIndex++) {
    const part = cursor.materialize(partIndex);
    if (part.kind !== "chunk") throw new Error("forced cell snapshot chunk was not chunked");
    chunks.push(part.value);
  }
  return chunks;
}
