import { clone, create, fromBinary, toBinary } from "@bufbuild/protobuf";
import {
  PbCellGridChunkSchema,
  PbCellGridFrameSchema,
  PbCellRowSchema,
  type PbCellGridChunk,
  type PbCellGridFrame,
  type PbCellRow,
} from "../gen/roost/v1/cell_pb.ts";
import {
  TERMINAL_MAX_COLS,
  TERMINAL_MAX_ROWS,
  isTerminalUuid,
  isTerminalGeometry,
} from "../viewport.ts";
import { assertCellRowSpans } from "./types.ts";

/** Maximum canonical protobuf encoding of one PbCellGridChunk. */
export const CELL_GRID_PART_MAX_BYTES = 1024 * 1024;
/** Maximum sum of encoded chunks accepted for one atomic snapshot. */
export const CELL_GRID_SNAPSHOT_MAX_BYTES = 64 * 1024 * 1024;
export const CELL_GRID_SNAPSHOT_MAX_CHUNKS = 256;
export const CELL_GRID_SNAPSHOT_MAX_ROWS = TERMINAL_MAX_ROWS;
export const CELL_GRID_SNAPSHOT_MAX_SPANS = 65_536;
export const CELL_GRID_SNAPSHOT_MAX_LINK_MAPPINGS = 1_024;
export const CELL_GRID_CHUNK_STALL_MS = 10_000;

export type CellGridChunkErrorCode =
  | "invalid-snapshot-id"
  | "invalid-stream-id"
  | "missing-part"
  | "invalid-full"
  | "invalid-geometry"
  | "invalid-sequence"
  | "chunk-count"
  | "chunk-index"
  | "chunk-order"
  | "chunk-size"
  | "snapshot-size"
  | "snapshot-stalled"
  | "metadata-mismatch"
  | "row-index"
  | "duplicate-row"
  | "missing-row"
  | "span-limit"
  | "link-limit"
  | "link-conflict"
  | "single-row-oversize";

export class CellGridChunkError extends Error {
  readonly code: CellGridChunkErrorCode;

  constructor(code: CellGridChunkErrorCode, message: string) {
    super(message);
    this.name = "CellGridChunkError";
    this.code = code;
  }
}

export function rejectCellGridChunk(code: CellGridChunkErrorCode, message: string): never {
  throw new CellGridChunkError(code, message);
}

export function encodedCellGridFrameSize(frame: PbCellGridFrame): number {
  return toBinary(PbCellGridFrameSchema, frame).byteLength;
}

export function encodedCellGridChunkSize(chunk: PbCellGridChunk): number {
  return toBinary(PbCellGridChunkSchema, chunk).byteLength;
}

export function encodeCellGridChunk(chunk: PbCellGridChunk): Uint8Array {
  const encoded = toBinary(PbCellGridChunkSchema, chunk);
  if (encoded.byteLength > CELL_GRID_PART_MAX_BYTES) {
    rejectCellGridChunk(
      "chunk-size",
      `cell snapshot chunk is ${encoded.byteLength} bytes; maximum is ${CELL_GRID_PART_MAX_BYTES}`,
    );
  }
  return encoded;
}

export function decodeCellGridChunk(encoded: Uint8Array): PbCellGridChunk {
  if (encoded.byteLength > CELL_GRID_PART_MAX_BYTES) {
    rejectCellGridChunk(
      "chunk-size",
      `cell snapshot chunk is ${encoded.byteLength} bytes; maximum is ${CELL_GRID_PART_MAX_BYTES}`,
    );
  }
  return fromBinary(PbCellGridChunkSchema, encoded, { readUnknownFields: false });
}

export interface CellGridSnapshotStats {
  spans: number;
  linkMappings: number;
}

export function assertSnapshotScalars(frame: PbCellGridFrame): void {
  if (!frame.full || frame.baseSeq !== 0n) {
    rejectCellGridChunk("invalid-full", "cell snapshot part must be a full frame with base_seq=0");
  }
  if (!isTerminalUuid(frame.streamId)) {
    rejectCellGridChunk(
      "invalid-stream-id",
      `cell snapshot stream_id is not a UUID: ${JSON.stringify(frame.streamId)}`,
    );
  }
  if (!frame.gridEpoch) rejectCellGridChunk("invalid-full", "cell snapshot grid_epoch is empty");
  if (!isTerminalGeometry({ cols: frame.cols, rows: frame.rows })) {
    rejectCellGridChunk(
      "invalid-geometry",
      `cell snapshot geometry ${frame.cols}x${frame.rows} is outside `
        + `1..${TERMINAL_MAX_COLS}x1..${TERMINAL_MAX_ROWS}`,
    );
  }
  if (frame.seq < 1n) {
    rejectCellGridChunk("invalid-sequence", `cell snapshot seq must be positive, got ${frame.seq}`);
  }
  if (frame.scrollbackAppend.length !== 0) {
    rejectCellGridChunk("invalid-full", "authoritative cell snapshot parts cannot carry scrollback append");
  }
  if (frame.sbBase < 0n || frame.sbBase > frame.scrollbackTotal) {
    rejectCellGridChunk("invalid-full", "authoritative cell snapshot sb_base is outside its history");
  }
}

export interface CellGridLinkMapping {
  key: string;
  uri: string;
}

export function addSnapshotRows(
  frame: PbCellGridFrame,
  rows: readonly PbCellRow[],
  seenRows: Set<number>,
  links: Map<string, CellGridLinkMapping>,
  startingSpans: number,
  internLinks: boolean,
): number {
  let spans = startingSpans;
  for (const row of rows) {
    if (!Number.isInteger(row.index) || row.index < 0 || row.index >= frame.rows) {
      rejectCellGridChunk(
        "row-index",
        `cell snapshot viewport row ${row.index} is outside 0..${frame.rows - 1}`,
      );
    }
    if (seenRows.has(row.index)) {
      rejectCellGridChunk("duplicate-row", `cell snapshot viewport row ${row.index} occurs more than once`);
    }
    seenRows.add(row.index);
    assertCellRowSpans(row, frame.cols);
    spans += row.spans.length;
    if (spans > CELL_GRID_SNAPSHOT_MAX_SPANS) {
      rejectCellGridChunk(
        "span-limit",
        `cell snapshot has more than ${CELL_GRID_SNAPSHOT_MAX_SPANS} spans`,
      );
    }
    for (const span of row.spans) {
      if (span.linkKey === undefined && span.linkUri === undefined) continue;
      // assertCellRowSpans has already rejected every half mapping.
      const key = span.linkKey!;
      const uri = span.linkUri!;
      const prior = links.get(key);
      if (prior !== undefined) {
        if (prior.uri !== uri) {
          rejectCellGridChunk(
            "link-conflict",
            `cell snapshot link_key ${JSON.stringify(key)} maps to conflicting URIs`,
          );
        }
        if (internLinks) {
          span.linkKey = prior.key;
          span.linkUri = prior.uri;
        }
        continue;
      }
      if (links.size >= CELL_GRID_SNAPSHOT_MAX_LINK_MAPPINGS) {
        rejectCellGridChunk(
          "link-limit",
          `cell snapshot has more than ${CELL_GRID_SNAPSHOT_MAX_LINK_MAPPINGS} link mappings`,
        );
      }
      links.set(key, { key, uri });
    }
  }
  return spans;
}
export function addSnapshotHistoryRows(
  frame: PbCellGridFrame,
  rows: readonly PbCellRow[],
  nextIndex: bigint,
  links: Map<string, CellGridLinkMapping>,
  startingSpans: number,
  internLinks: boolean,
): { spans: number; nextIndex: bigint } {
  let spans = startingSpans;
  let next = nextIndex;
  for (const row of rows) {
    if (!Number.isInteger(row.index) || BigInt(row.index) !== next || BigInt(row.index) >= frame.scrollbackTotal) {
      rejectCellGridChunk(
        "row-index",
        `cell snapshot history row ${row.index} does not continue at ${next}`,
      );
    }
    assertCellRowSpans(row, 0);
    spans += row.spans.length;
    if (spans > CELL_GRID_SNAPSHOT_MAX_SPANS) {
      rejectCellGridChunk(
        "span-limit",
        `cell snapshot has more than ${CELL_GRID_SNAPSHOT_MAX_SPANS} spans`,
      );
    }
    for (const span of row.spans) {
      if (span.linkKey === undefined && span.linkUri === undefined) continue;
      const key = span.linkKey!;
      const uri = span.linkUri!;
      const prior = links.get(key);
      if (prior !== undefined) {
        if (prior.uri !== uri) {
          rejectCellGridChunk(
            "link-conflict",
            `cell snapshot link_key ${JSON.stringify(key)} maps to conflicting URIs`,
          );
        }
        if (internLinks) {
          span.linkKey = prior.key;
          span.linkUri = prior.uri;
        }
        continue;
      }
      if (links.size >= CELL_GRID_SNAPSHOT_MAX_LINK_MAPPINGS) {
        rejectCellGridChunk(
          "link-limit",
          `cell snapshot has more than ${CELL_GRID_SNAPSHOT_MAX_LINK_MAPPINGS} link mappings`,
        );
      }
      links.set(key, { key, uri });
    }
    next++;
  }
  return { spans, nextIndex: next };
}


/** Validate one complete authoritative viewport snapshot. */
export function assertCellGridSnapshot(frame: PbCellGridFrame): CellGridSnapshotStats {
  assertSnapshotScalars(frame);
  if (frame.rows > CELL_GRID_SNAPSHOT_MAX_ROWS) {
    rejectCellGridChunk(
      "invalid-geometry",
      `cell snapshot has more than ${CELL_GRID_SNAPSHOT_MAX_ROWS} rows`,
    );
  }
  const seenRows = new Set<number>();
  const links = new Map<string, CellGridLinkMapping>();
  let spans = addSnapshotRows(frame, frame.viewportRows, seenRows, links, 0, false);
  const history = addSnapshotHistoryRows(frame, frame.scrollbackRows, frame.sbBase, links, spans, false);
  spans = history.spans;
  if (history.nextIndex !== frame.scrollbackTotal) {
    rejectCellGridChunk("missing-row", "cell snapshot history does not reach scrollback_total");
  }
  if (seenRows.size !== frame.rows) {
    rejectCellGridChunk(
      "missing-row",
      `cell snapshot has ${seenRows.size} of ${frame.rows} required viewport rows`,
    );
  }
  return { spans, linkMappings: links.size };
}

export function hasSameSnapshotMetadata(a: PbCellGridFrame, b: PbCellGridFrame): boolean {
  return a.sessionId === b.sessionId
    && a.streamId === b.streamId
    && a.gridEpoch === b.gridEpoch
    && a.cols === b.cols
    && a.rows === b.rows
    && a.cursorRow === b.cursorRow
    && a.cursorCol === b.cursorCol
    && a.cursorVisible === b.cursorVisible
    && a.altScreen === b.altScreen
    && a.full === b.full
    && a.scrollbackTotal === b.scrollbackTotal
    && a.seq === b.seq
    && a.sbBase === b.sbBase
    && a.baseSeq === b.baseSeq
    && a.cursorKeysApp === b.cursorKeysApp
    && a.bracketedPaste === b.bracketedPaste
    && a.mouseTracking === b.mouseTracking
    && a.mouseSgr === b.mouseSgr
    && a.focusEvents === b.focusEvents
    && a.ptyOutMs === b.ptyOutMs
    && a.workerEmitMs === b.workerEmitMs
    && a.coordRecvMs === b.coordRecvMs
    && a.coordFanoutMs === b.coordFanoutMs;
}

export function createCellGridFramePart(
  frame: PbCellGridFrame,
  rows: readonly PbCellRow[],
  scrollbackRows: readonly PbCellRow[] = [],
): PbCellGridFrame {
  return create(PbCellGridFrameSchema, {
    sessionId: frame.sessionId,
    cols: frame.cols,
    rows: frame.rows,
    cursorRow: frame.cursorRow,
    cursorCol: frame.cursorCol,
    cursorVisible: frame.cursorVisible,
    altScreen: frame.altScreen,
    full: frame.full,
    viewportRows: rows.map((row) => clone(PbCellRowSchema, row)),
    scrollbackRows: scrollbackRows.map((row) => clone(PbCellRowSchema, row)),
    scrollbackAppend: [],
    scrollbackTotal: frame.scrollbackTotal,
    seq: frame.seq,
    sbBase: frame.sbBase,
    cursorKeysApp: frame.cursorKeysApp,
    bracketedPaste: frame.bracketedPaste,
    ptyOutMs: frame.ptyOutMs,
    workerEmitMs: frame.workerEmitMs,
    coordRecvMs: frame.coordRecvMs,
    coordFanoutMs: frame.coordFanoutMs,
    gridEpoch: frame.gridEpoch,
    mouseTracking: frame.mouseTracking,
    mouseSgr: frame.mouseSgr,
    focusEvents: frame.focusEvents,
    streamId: frame.streamId,
    baseSeq: frame.baseSeq,
  });
}
