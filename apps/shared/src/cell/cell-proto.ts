// CellGridFrame ↔ proto adapters (R11). Worker fills a CellGridFrame
// (cell types), converts to PbCellGridFrame for the wire; coord stamps
// session_id and fans out; SPA converts back. Keeps the in-app cell types
// free of @bufbuild/protobuf so the renderer + pure logic stay wire-agnostic.

import { create } from "@bufbuild/protobuf";
import {
  PbCellGridFrameSchema, PbCellRowSchema, PbCellSpanSchema,
  type PbCellGridFrame, type PbCellRow, type PbCellSpan,
} from "../gen/roost/v1/cell_pb.ts";
import type { CellGridFrame, CellRow, CellSpan } from "./types.ts";

function _spanToProto(s: CellSpan): PbCellSpan {
  return create(PbCellSpanSchema, {
    text: s.text, fg: s.fg, bg: s.bg, flags: s.flags,
    fgRgb: s.fgRgb, bgRgb: s.bgRgb,
  });
}
export function cellRowToProto(r: CellRow): PbCellRow {
  return create(PbCellRowSchema, { index: r.index, spans: r.spans.map(_spanToProto) });
}

export function cellFrameToProto(f: CellGridFrame, sessionId: string): PbCellGridFrame {
  return create(PbCellGridFrameSchema, {
    sessionId,
    gridEpoch: f.gridEpoch,
    cols: f.cols, rows: f.rows,
    cursorRow: f.cursorRow, cursorCol: f.cursorCol, cursorVisible: f.cursorVisible,
    altScreen: f.altScreen, cursorKeysApp: f.cursorKeysApp, bracketedPaste: f.bracketedPaste, full: f.full,
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
 * be reused safely. */
export function cellRowFromProto(r: PbCellRow): CellRow {
  return r;
}

export function protoToCellFrame(p: PbCellGridFrame): CellGridFrame {
  return {
    gridEpoch: p.gridEpoch,
    cols: p.cols, rows: p.rows,
    cursorRow: p.cursorRow, cursorCol: p.cursorCol, cursorVisible: p.cursorVisible,
    altScreen: p.altScreen, cursorKeysApp: p.cursorKeysApp, bracketedPaste: p.bracketedPaste, full: p.full,
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
  const cloneRows = (rows: readonly PbCellRow[]): CellRow[] => rows.map((row) => ({
    index: row.index,
    spans: row.spans.map((span) => ({
      text: span.text,
      fg: span.fg,
      bg: span.bg,
      flags: span.flags,
      fgRgb: span.fgRgb,
      bgRgb: span.bgRgb,
    })),
  }));
  return {
    gridEpoch: p.gridEpoch,
    cols: p.cols, rows: p.rows,
    cursorRow: p.cursorRow, cursorCol: p.cursorCol, cursorVisible: p.cursorVisible,
    altScreen: p.altScreen, cursorKeysApp: p.cursorKeysApp, bracketedPaste: p.bracketedPaste, full: p.full,
    viewportRows: cloneRows(p.viewportRows),
    scrollbackRows: cloneRows(p.scrollbackRows),
    scrollbackAppend: cloneRows(p.scrollbackAppend),
    scrollbackTotal: Number(p.scrollbackTotal),
    sbBase: Number(p.sbBase),
    seq: Number(p.seq),
  };
}
