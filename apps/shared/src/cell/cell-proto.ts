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
    cols: f.cols, rows: f.rows,
    cursorRow: f.cursorRow, cursorCol: f.cursorCol, cursorVisible: f.cursorVisible,
    altScreen: f.altScreen, full: f.full,
    viewportRows: f.viewportRows.map(cellRowToProto),
    scrollbackRows: f.scrollbackRows.map(cellRowToProto),
    scrollbackAppend: f.scrollbackAppend.map(cellRowToProto),
    scrollbackTotal: BigInt(f.scrollbackTotal),
    sbBase: BigInt(f.sbBase),
    seq: BigInt(f.seq),
  });
}

function _spanFromProto(s: PbCellSpan): CellSpan {
  return { text: s.text, fg: s.fg, bg: s.bg, flags: s.flags, fgRgb: s.fgRgb, bgRgb: s.bgRgb };
}
export function cellRowFromProto(r: PbCellRow): CellRow {
  return { index: r.index, spans: r.spans.map(_spanFromProto) };
}

export function protoToCellFrame(p: PbCellGridFrame): CellGridFrame {
  return {
    cols: p.cols, rows: p.rows,
    cursorRow: p.cursorRow, cursorCol: p.cursorCol, cursorVisible: p.cursorVisible,
    altScreen: p.altScreen, full: p.full,
    viewportRows: p.viewportRows.map(cellRowFromProto),
    scrollbackRows: p.scrollbackRows.map(cellRowFromProto),
    scrollbackAppend: p.scrollbackAppend.map(cellRowFromProto),
    scrollbackTotal: Number(p.scrollbackTotal),
    sbBase: Number(p.sbBase),
    seq: Number(p.seq),
  };
}
