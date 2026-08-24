import type {
  PbCellGridChunk,
  PbCellGridFrame,
  PbCellRow,
} from "../gen/roost/v1/cell_pb.ts";
import { isTerminalUuid } from "../viewport.ts";
import {
  CELL_GRID_CHUNK_STALL_MS,
  CELL_GRID_PART_MAX_BYTES,
  CELL_GRID_SNAPSHOT_MAX_BYTES,
  CELL_GRID_SNAPSHOT_MAX_CHUNKS,
  addSnapshotRows,
  addSnapshotHistoryRows,
  assertSnapshotScalars,
  createCellGridFramePart,
  encodedCellGridChunkSize,
  hasSameSnapshotMetadata,
  rejectCellGridChunk,
  type CellGridLinkMapping,
} from "./frame-chunk-validation.ts";

interface PartialSnapshot {
  snapshotId: string;
  streamId: string;
  chunkCount: number;
  nextChunkIndex: number;
  totalBytes: number;
  lastChunkAt: number;
  metadata: PbCellGridFrame;
  rows: Array<PbCellRow | undefined>;
  historyRows: PbCellRow[];
  nextHistoryIndex: bigint;
  seenRows: Set<number>;
  links: Map<string, CellGridLinkMapping>;
  spans: number;
}

export type CellGridChunkAssemblyResult =
  | { kind: "pending"; snapshotId: string; nextChunkIndex: number }
  | { kind: "complete"; snapshotId: string; frame: PbCellGridFrame };

/** One bounded, in-order, atomically completing snapshot assembly. */
export class CellGridChunkAssembler {
  private partial: PartialSnapshot | null = null;

  get activeSnapshotId(): string | null {
    return this.partial?.snapshotId ?? null;
  }

  /** Attach-progress read: how far the in-flight baseline has assembled.
   * Null whenever no partial exists — idle, completed, or reset — so a
   * single-frame baseline never reports progress. */
  get snapshotProgress(): {
    snapshotId: string;
    receivedChunks: number;
    totalChunks: number;
  } | null {
    const partial = this.partial;
    if (partial === null) return null;
    return {
      snapshotId: partial.snapshotId,
      receivedChunks: partial.nextChunkIndex,
      totalChunks: partial.chunkCount,
    };
  }

  reset(): void {
    this.partial = null;
  }

  /** Drop a partial at the shared transport-pressure stall boundary. */
  expire(nowMs = Date.now()): boolean {
    const partial = this.partial;
    if (partial === null || nowMs - partial.lastChunkAt < CELL_GRID_CHUNK_STALL_MS) return false;
    this.reset();
    return true;
  }

  push(chunk: PbCellGridChunk, nowMs = Date.now()): CellGridChunkAssemblyResult {
    try {
      return this.pushChecked(chunk, nowMs);
    } catch (error) {
      this.reset();
      throw error;
    }
  }

  private pushChecked(chunk: PbCellGridChunk, nowMs: number): CellGridChunkAssemblyResult {
    if (!isTerminalUuid(chunk.snapshotId)) {
      rejectCellGridChunk(
        "invalid-snapshot-id",
        `cell snapshot_id is not a UUID: ${JSON.stringify(chunk.snapshotId)}`,
      );
    }
    if (!Number.isInteger(chunk.chunkCount)
      || chunk.chunkCount < 1
      || chunk.chunkCount > CELL_GRID_SNAPSHOT_MAX_CHUNKS) {
      rejectCellGridChunk(
        "chunk-count",
        `cell snapshot chunk_count ${chunk.chunkCount} is outside 1..${CELL_GRID_SNAPSHOT_MAX_CHUNKS}`,
      );
    }
    if (!Number.isInteger(chunk.chunkIndex)
      || chunk.chunkIndex < 0
      || chunk.chunkIndex >= chunk.chunkCount) {
      rejectCellGridChunk(
        "chunk-index",
        `cell snapshot chunk_index ${chunk.chunkIndex} is outside its chunk_count`,
      );
    }
    const part = chunk.part;
    if (part === undefined) rejectCellGridChunk("missing-part", "cell snapshot chunk has no part");
    assertSnapshotScalars(part);
    if (part.viewportRows.length === 0 && part.scrollbackRows.length === 0) {
      rejectCellGridChunk("missing-row", "cell snapshot chunk carries no row");
    }

    const encodedBytes = encodedCellGridChunkSize(chunk);
    if (encodedBytes > CELL_GRID_PART_MAX_BYTES) {
      rejectCellGridChunk(
        "chunk-size",
        `cell snapshot chunk is ${encodedBytes} bytes; maximum is ${CELL_GRID_PART_MAX_BYTES}`,
      );
    }

    let partial = this.partial;
    if (partial !== null && nowMs - partial.lastChunkAt >= CELL_GRID_CHUNK_STALL_MS) {
      rejectCellGridChunk("snapshot-stalled", `cell snapshot ${partial.snapshotId} stalled between chunks`);
    }
    if (partial !== null && partial.snapshotId !== chunk.snapshotId) {
      if (chunk.chunkIndex !== 0) {
        rejectCellGridChunk("chunk-order", "a replacement cell snapshot must start at chunk_index=0");
      }
      if (partial.streamId !== part.streamId) {
        rejectCellGridChunk(
          "metadata-mismatch",
          "a partial cell snapshot can only be replaced within the same stream",
        );
      }
      this.reset();
      partial = null;
    }

    if (partial === null) {
      if (chunk.chunkIndex !== 0) {
        rejectCellGridChunk("chunk-order", "cell snapshot must start at chunk_index=0");
      }
      partial = {
        snapshotId: chunk.snapshotId,
        streamId: part.streamId,
        chunkCount: chunk.chunkCount,
        nextChunkIndex: 0,
        totalBytes: 0,
        lastChunkAt: nowMs,
        metadata: createCellGridFramePart(part, []),
        rows: new Array<PbCellRow | undefined>(part.rows),
        historyRows: [],
        nextHistoryIndex: part.sbBase,
        seenRows: new Set<number>(),
        links: new Map<string, CellGridLinkMapping>(),
        spans: 0,
      };
      this.partial = partial;
    } else {
      if (chunk.chunkCount !== partial.chunkCount) {
        rejectCellGridChunk("chunk-count", "cell snapshot chunk_count changed during assembly");
      }
      if (!hasSameSnapshotMetadata(partial.metadata, part)) {
        rejectCellGridChunk(
          "metadata-mismatch",
          "cell snapshot scalar metadata changed between chunks",
        );
      }
    }

    if (chunk.chunkIndex !== partial.nextChunkIndex) {
      rejectCellGridChunk(
        "chunk-order",
        `cell snapshot expected chunk_index=${partial.nextChunkIndex}, got ${chunk.chunkIndex}`,
      );
    }
    if (partial.totalBytes + encodedBytes > CELL_GRID_SNAPSHOT_MAX_BYTES) {
      rejectCellGridChunk(
        "snapshot-size",
        `cell snapshot exceeds ${CELL_GRID_SNAPSHOT_MAX_BYTES} encoded bytes`,
      );
    }

    const history = addSnapshotHistoryRows(
      part,
      part.scrollbackRows,
      partial.nextHistoryIndex,
      partial.links,
      partial.spans,
      true,
    );
    partial.spans = history.spans;
    partial.nextHistoryIndex = history.nextIndex;
    partial.historyRows.push(...part.scrollbackRows);
    partial.spans = addSnapshotRows(
      part,
      part.viewportRows,
      partial.seenRows,
      partial.links,
      partial.spans,
      true,
    );
    for (const row of part.viewportRows) partial.rows[row.index] = row;
    partial.totalBytes += encodedBytes;
    partial.nextChunkIndex += 1;
    partial.lastChunkAt = nowMs;

    if (partial.nextChunkIndex < partial.chunkCount) {
      return {
        kind: "pending",
        snapshotId: partial.snapshotId,
        nextChunkIndex: partial.nextChunkIndex,
      };
    }
    if (partial.seenRows.size !== part.rows) {
      rejectCellGridChunk(
        "missing-row",
        `cell snapshot has ${partial.seenRows.size} of ${part.rows} required viewport rows`,
      );
    }
    if (partial.nextHistoryIndex !== part.scrollbackTotal) {
      rejectCellGridChunk("missing-row", "cell snapshot history does not reach scrollback_total");
    }
    const rows = partial.rows;
    for (let index = 0; index < rows.length; index += 1) {
      if (rows[index] === undefined) {
        rejectCellGridChunk("missing-row", `cell snapshot is missing viewport row ${index}`);
      }
    }
    const complete = createCellGridFramePart(
      partial.metadata,
      rows as PbCellRow[],
      partial.historyRows,
    );
    const snapshotId = partial.snapshotId;
    this.reset();
    return { kind: "complete", snapshotId, frame: complete };
  }
}
