// CellGridFrame ↔ proto adapters (R11). Worker fills a CellGridFrame
// (cell types), converts to PbCellGridFrame for the wire; coord stamps
// session_id and fans out; SPA converts back. Keeps the in-app cell types
// free of @bufbuild/protobuf so the renderer + pure logic stay wire-agnostic.

import { create } from "@bufbuild/protobuf";
import {
  PbCellGridFrameSchema, PbCellRowSchema, PbCellSpanSchema,
  type PbCellGridFrame, type PbCellRow, type PbCellSpan,
} from "../gen/roost/v1/cell_pb.ts";
import { assertTerminalGeometry, isTerminalUuid } from "../viewport.ts";
import {
  asMouseTracking, assertCellRowSpans,
  type CellGridFrame, type CellRow, type CellSpan,
} from "./types.ts";

function _spanToProto(s: CellSpan): PbCellSpan {
  return create(PbCellSpanSchema, {
    text: s.text, fg: s.fg, bg: s.bg, flags: s.flags,
    fgRgb: s.fgRgb, bgRgb: s.bgRgb, columns: s.columns,
    linkUri: s.linkUri, linkKey: s.linkKey,
  });
}
export function cellRowToProto(r: CellRow): PbCellRow {
  return create(PbCellRowSchema, { index: r.index, spans: r.spans.map(_spanToProto) });
}

/** Column occupancy is load-bearing for every downstream column computation, so
 *  a frame that does not carry it is rejected at the boundary rather than
 *  painted one column short per wide glyph. Viewport rows are bounded by the
 *  frame's width; a retained scrollback line keeps its write-time width. */
function _assertFrameSpans(p: PbCellGridFrame): void {
  for (const row of p.viewportRows) assertCellRowSpans(row, p.cols);
  for (const row of p.scrollbackRows) assertCellRowSpans(row, 0);
  for (const row of p.scrollbackAppend) assertCellRowSpans(row, 0);
}
const MAX_SAFE_UINT64 = BigInt(Number.MAX_SAFE_INTEGER);

function _safeUint64(value: bigint, field: string): number {
  if (value < 0n || value > MAX_SAFE_UINT64) {
    throw new Error(`${field} ${value} is outside JavaScript's safe unsigned integer range`);
  }
  return Number(value);
}

function _uint64(value: number, field: string): bigint {
  if (!Number.isSafeInteger(value) || value < 0) {
    throw new Error(`${field} ${String(value)} is not a safe unsigned integer`);
  }
  return BigInt(value);
}

function _assertFrameIdentity(
  streamId: string,
  gridEpoch: string,
  full: boolean,
  baseSeq: bigint,
  seq: bigint,
): void {
  if (!isTerminalUuid(streamId)) {
    throw new Error(`cell stream_id is not a UUID: ${JSON.stringify(streamId)}`);
  }
  if (gridEpoch.length === 0) throw new Error("cell grid_epoch is empty");
  if (seq < 1n || seq > MAX_SAFE_UINT64 || baseSeq > MAX_SAFE_UINT64) {
    throw new Error(`cell sequence ${baseSeq}->${seq} is outside the safe positive range`);
  }
  if (full) {
    if (baseSeq !== 0n) throw new Error(`full cell frame has nonzero base_seq ${baseSeq}`);
    return;
  }
  if (seq !== baseSeq + 1n) {
    throw new Error(`cell delta seq ${seq} does not follow base_seq ${baseSeq}`);
  }
}

function _assertFrameSequence(p: PbCellGridFrame): void {
  _assertFrameIdentity(p.streamId, p.gridEpoch, p.full, p.baseSeq, p.seq);
}
function _assertFrameStructure(p: PbCellGridFrame): void {
  assertTerminalGeometry({ cols: p.cols, rows: p.rows });
  const seen = new Set<number>();
  for (const row of p.viewportRows) {
    if (!Number.isInteger(row.index) || row.index < 0 || row.index >= p.rows) {
      throw new Error(`cell viewport row ${row.index} is outside 0..${p.rows - 1}`);
    }
    if (seen.has(row.index)) {
      throw new Error(`cell viewport row ${row.index} occurs more than once`);
    }
    seen.add(row.index);
  }
  if (!p.full) {
    if (p.scrollbackRows.length !== 0) {
      throw new Error("cell delta cannot carry scrollback_rows");
    }
    return;
  }
  if (seen.size !== p.rows) {
    throw new Error(`full cell frame has ${seen.size} of ${p.rows} required viewport rows`);
  }
  if (p.scrollbackAppend.length !== 0) {
    throw new Error("full cell frame cannot carry scrollback_append");
  }
  const sbBase = _safeUint64(p.sbBase, "sb_base");
  const scrollbackTotal = _safeUint64(p.scrollbackTotal, "scrollback_total");
  if (sbBase > scrollbackTotal || p.scrollbackRows.length !== scrollbackTotal - sbBase) {
    throw new Error(
      `full cell frame history does not cover [${sbBase}, ${scrollbackTotal}) exactly`,
    );
  }
  for (let offset = 0; offset < p.scrollbackRows.length; offset += 1) {
    if (p.scrollbackRows[offset]!.index !== sbBase + offset) {
      throw new Error(`full cell frame history row ${sbBase + offset} is missing or out of order`);
    }
  }
}


export function cellFrameToProto(f: CellGridFrame, sessionId: string): PbCellGridFrame {
  const scrollbackTotal = _uint64(f.scrollbackTotal, "scrollback_total");
  const sbBase = _uint64(f.sbBase, "sb_base");
  const baseSeq = _uint64(f.baseSeq, "base_seq");
  const seq = _uint64(f.seq, "seq");
  _assertFrameIdentity(f.streamId, f.gridEpoch, f.full, baseSeq, seq);
  const proto = create(PbCellGridFrameSchema, {
    sessionId,
    streamId: f.streamId,
    gridEpoch: f.gridEpoch,
    cols: f.cols, rows: f.rows,
    cursorRow: f.cursorRow, cursorCol: f.cursorCol, cursorVisible: f.cursorVisible,
    altScreen: f.altScreen, cursorKeysApp: f.cursorKeysApp, bracketedPaste: f.bracketedPaste, full: f.full,
    mouseTracking: f.mouseTracking, mouseSgr: f.mouseSgr, focusEvents: f.focusEvents,
    viewportRows: f.viewportRows.map(cellRowToProto),
    scrollbackRows: f.scrollbackRows.map(cellRowToProto),
    scrollbackAppend: f.scrollbackAppend.map(cellRowToProto),
    scrollbackTotal,
    sbBase,
    baseSeq,
    seq,
  });
  _assertFrameStructure(proto);
  return proto;
}

/** Inbound protobuf rows transfer into the first canonical replica without
 * rebuilding row/span objects. That replica owns the decoded arrays; fan-out
 * gives each mutable replica/renderer its own row shells via
 * cloneCellGridFrame while sharing immutable spans. Occupancy is validated here
 * too because history pages splice into the same painted coordinate space. A
 * retained line keeps its write-time width, so no current-grid bound applies. */
export function cellRowFromProto(r: PbCellRow): CellRow {
  assertCellRowSpans(r, 0);
  return r;
}

export function protoToCellFrame(p: PbCellGridFrame): CellGridFrame {
  _assertFrameSpans(p);
  _assertFrameSequence(p);
  _assertFrameStructure(p);
  return {
    streamId: p.streamId,
    gridEpoch: p.gridEpoch,
    cols: p.cols, rows: p.rows,
    cursorRow: p.cursorRow, cursorCol: p.cursorCol, cursorVisible: p.cursorVisible,
    altScreen: p.altScreen, cursorKeysApp: p.cursorKeysApp, bracketedPaste: p.bracketedPaste, full: p.full,
    mouseTracking: asMouseTracking(p.mouseTracking), mouseSgr: p.mouseSgr, focusEvents: p.focusEvents,
    viewportRows: p.viewportRows,
    scrollbackRows: p.scrollbackRows,
    scrollbackAppend: p.scrollbackAppend,
    scrollbackTotal: _safeUint64(p.scrollbackTotal, "scrollback_total"),
    sbBase: _safeUint64(p.sbBase, "sb_base"),
    baseSeq: _safeUint64(p.baseSeq, "base_seq"),
    seq: _safeUint64(p.seq, "seq"),
  };
}

/** Explicit deep-copy adapter for tests or exceptional callers that must retain
 * a frame independently of the decoded protobuf message. Production rendering
 * uses protoToCellFrame's ownership transfer instead. */
export function deepCloneCellFrameFromProto(p: PbCellGridFrame): CellGridFrame {
  _assertFrameSpans(p);
  _assertFrameSequence(p);
  _assertFrameStructure(p);
  const cloneRows = (rows: readonly PbCellRow[]): CellRow[] => rows.map((row) => ({
    index: row.index,
    spans: row.spans.map((span) => ({
      text: span.text,
      fg: span.fg,
      bg: span.bg,
      flags: span.flags,
      fgRgb: span.fgRgb,
      bgRgb: span.bgRgb,
      columns: span.columns,
      linkUri: span.linkUri,
      linkKey: span.linkKey,
    })),
  }));
  return {
    streamId: p.streamId,
    gridEpoch: p.gridEpoch,
    cols: p.cols, rows: p.rows,
    cursorRow: p.cursorRow, cursorCol: p.cursorCol, cursorVisible: p.cursorVisible,
    altScreen: p.altScreen, cursorKeysApp: p.cursorKeysApp, bracketedPaste: p.bracketedPaste, full: p.full,
    mouseTracking: asMouseTracking(p.mouseTracking), mouseSgr: p.mouseSgr, focusEvents: p.focusEvents,
    viewportRows: cloneRows(p.viewportRows),
    scrollbackRows: cloneRows(p.scrollbackRows),
    scrollbackAppend: cloneRows(p.scrollbackAppend),
    scrollbackTotal: _safeUint64(p.scrollbackTotal, "scrollback_total"),
    sbBase: _safeUint64(p.sbBase, "sb_base"),
    baseSeq: _safeUint64(p.baseSeq, "base_seq"),
    seq: _safeUint64(p.seq, "seq"),
  };
}
