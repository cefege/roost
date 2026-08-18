// CellGridFrame ↔ proto adapters (R11). Worker fills a CellGridFrame
// (cell types), converts to PbCellGridFrame for the wire; coord stamps
// session_id and fans out; SPA converts back. Keeps the in-app cell types
// free of @bufbuild/protobuf so the renderer + pure logic stay wire-agnostic.

import { create } from "@bufbuild/protobuf";
import {
  PbCellGridFrameSchema, PbCellRowSchema, PbCellSpanSchema,
  type PbCellGridFrame, type PbCellRow, type PbCellSpan,
} from "../gen/roost/v1/cell_pb.ts";
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

export function cellFrameToProto(f: CellGridFrame, sessionId: string): PbCellGridFrame {
  return create(PbCellGridFrameSchema, {
    sessionId,
    gridEpoch: f.gridEpoch,
    cols: f.cols, rows: f.rows,
    cursorRow: f.cursorRow, cursorCol: f.cursorCol, cursorVisible: f.cursorVisible,
    altScreen: f.altScreen, cursorKeysApp: f.cursorKeysApp, bracketedPaste: f.bracketedPaste, full: f.full,
    mouseTracking: f.mouseTracking, mouseSgr: f.mouseSgr, focusEvents: f.focusEvents,
    viewportRows: f.viewportRows.map(cellRowToProto),
    scrollbackRows: f.scrollbackRows.map(cellRowToProto),
    scrollbackAppend: f.scrollbackAppend.map(cellRowToProto),
    scrollbackTotal: BigInt(f.scrollbackTotal),
    sbBase: BigInt(f.sbBase),
    seq: BigInt(f.seq),
  });
}

/** Inbound protobuf rows are transferred to the cell model without rebuilding
 * row/span objects. The sync dispatcher owns the decoded message and hands that
 * ownership to exactly one renderer, so the structurally compatible arrays can
 * be reused safely. Occupancy is validated here too — a history page splices
 * into the same painted window as a frame's rows, so it owes the same contract.
 * A retained line keeps its write-time width, so no grid bound applies. */
export function cellRowFromProto(r: PbCellRow): CellRow {
  assertCellRowSpans(r, 0);
  return r;
}

export function protoToCellFrame(p: PbCellGridFrame): CellGridFrame {
  _assertFrameSpans(p);
  return {
    gridEpoch: p.gridEpoch,
    cols: p.cols, rows: p.rows,
    cursorRow: p.cursorRow, cursorCol: p.cursorCol, cursorVisible: p.cursorVisible,
    altScreen: p.altScreen, cursorKeysApp: p.cursorKeysApp, bracketedPaste: p.bracketedPaste, full: p.full,
    mouseTracking: asMouseTracking(p.mouseTracking), mouseSgr: p.mouseSgr, focusEvents: p.focusEvents,
    viewportRows: p.viewportRows,
    scrollbackRows: p.scrollbackRows,
    scrollbackAppend: p.scrollbackAppend,
    scrollbackTotal: Number(p.scrollbackTotal),
    sbBase: Number(p.sbBase),
    seq: Number(p.seq),
  };
}

/** Explicit deep-copy adapter for tests or exceptional callers that must retain
 * a frame independently of the decoded protobuf message. Production rendering
 * uses protoToCellFrame's ownership transfer instead. */
export function deepCloneCellFrameFromProto(p: PbCellGridFrame): CellGridFrame {
  _assertFrameSpans(p);
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
    gridEpoch: p.gridEpoch,
    cols: p.cols, rows: p.rows,
    cursorRow: p.cursorRow, cursorCol: p.cursorCol, cursorVisible: p.cursorVisible,
    altScreen: p.altScreen, cursorKeysApp: p.cursorKeysApp, bracketedPaste: p.bracketedPaste, full: p.full,
    mouseTracking: asMouseTracking(p.mouseTracking), mouseSgr: p.mouseSgr, focusEvents: p.focusEvents,
    viewportRows: cloneRows(p.viewportRows),
    scrollbackRows: cloneRows(p.scrollbackRows),
    scrollbackAppend: cloneRows(p.scrollbackAppend),
    scrollbackTotal: Number(p.scrollbackTotal),
    sbBase: Number(p.sbBase),
    seq: Number(p.seq),
  };
}
