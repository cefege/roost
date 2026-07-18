// Cell reader — turns a @wterm/core grid into a CellGridFrame (R11).
// Runs on the worker (the single emulator owner). Pure given the core:
// reads via getCell / getScrollbackCell, never mutates. rowToSpans is the
// run-length encoder (group consecutive equal-style cells, right-trim
// trailing default spaces) and is the inverse of the renderer's paint.
//
// Indexing note (verified in wterm-serialize.ts): wterm-core stores
// scrollback NEWEST-FIRST — offset 0 = line just above the viewport,
// offset count-1 = oldest. We expose scrollback OLDEST-FIRST (index 0 =
// oldest retained) so the client paints top→bottom and splices appends.

import type { TerminalCore, CellData } from "@wterm/core";
import { DEFAULT_COLOR, type CellRow, type CellSpan, type CellGridFrame } from "./types.ts";

function _sameStyle(a: CellData, b: CellSpan): boolean {
  return a.fg === b.fg && a.bg === b.bg && a.flags === b.flags
    && a.fgRgb === b.fgRgb && a.bgRgb === b.bgRgb;
}

function _isBlankDefault(c: CellData): boolean {
  return (c.char === 0 || c.char === 0x20)
    && c.flags === 0
    && c.fg === DEFAULT_COLOR && c.bg === DEFAULT_COLOR
    && c.fgRgb === undefined && c.bgRgb === undefined;
}

function _char(cp: number): string {
  if (cp === 0) return " ";
  try { return String.fromCodePoint(cp); } catch { return " "; }
}

/** Run-length encode one row of cells into right-trimmed style spans.
 *  Trailing default-style blanks are dropped (empty row → []). */
export function rowToSpans(cells: CellData[]): CellSpan[] {
  // Right-trim trailing default-style blanks.
  let end = cells.length;
  while (end > 0 && _isBlankDefault(cells[end - 1])) end--;

  const spans: CellSpan[] = [];
  let cur: CellSpan | null = null;
  for (let col = 0; col < end; col++) {
    const c = cells[col];
    if (cur && _sameStyle(c, cur)) {
      cur.text += _char(c.char);
    } else {
      cur = { text: _char(c.char), fg: c.fg, bg: c.bg, flags: c.flags, fgRgb: c.fgRgb, bgRgb: c.bgRgb };
      spans.push(cur);
    }
  }
  return spans;
}

function _viewportRow(core: TerminalCore, row: number, cols: number): CellRow {
  const cells: CellData[] = new Array(cols);
  for (let col = 0; col < cols; col++) cells[col] = core.getCell(row, col);
  return { index: row, spans: rowToSpans(cells) };
}

/** Read one scrollback line by OLDEST-FIRST absolute index. `total` is the
 *  current getScrollbackCount() (so we can convert to the core's
 *  newest-first offset). */
function _scrollbackRow(core: TerminalCore, absIndex: number, total: number): CellRow {
  const offset = total - 1 - absIndex; // oldest-first → newest-first offset
  const len = core.getScrollbackLineLen(offset);
  const cells: CellData[] = new Array(len);
  for (let col = 0; col < len; col++) cells[col] = core.getScrollbackCell(offset, col);
  return { index: absIndex, spans: rowToSpans(cells) };
}

/** Full snapshot: whole viewport + scrollback + cursor. `tailRows` caps the
 *  scrollback to the newest N lines (sbBase = total - N); unset = complete
 *  history with sbBase 0. */
export function gridToCellFrame(core: TerminalCore, seq: number, tailRows?: number): CellGridFrame {
  const cols = core.getCols();
  const rows = core.getRows();
  const total = core.getScrollbackCount();
  const cursor = core.getCursor();

  const viewportRows: CellRow[] = new Array(rows);
  for (let row = 0; row < rows; row++) viewportRows[row] = _viewportRow(core, row, cols);

  const sbBase = tailRows === undefined ? 0 : Math.max(0, total - tailRows);
  const scrollbackRows: CellRow[] = new Array(total - sbBase);
  for (let i = sbBase; i < total; i++) scrollbackRows[i - sbBase] = _scrollbackRow(core, i, total);

  return {
    cols, rows,
    cursorRow: cursor.row, cursorCol: cursor.col, cursorVisible: cursor.visible,
    altScreen: core.usingAltScreen(),
    full: true,
    viewportRows,
    scrollbackRows,
    scrollbackAppend: [],
    scrollbackTotal: total,
    sbBase,
    seq,
  };
}

/** Delta frame built directly from the core's dirty-row tracking — the
 *  worker's hot path. Changed viewport rows come from isDirtyRow (caller
 *  MUST call core.clearDirty() AFTER this returns), scrollback append from
 *  the count delta, cursor always. Caller decides full-vs-delta: send a
 *  FULL frame (gridToCellFrame) on attach, resize, alt toggle, or when
 *  getScrollbackCount() < prevScrollbackTotal (reset/eviction). */
export function gridDeltaFrame(core: TerminalCore, prevScrollbackTotal: number, seq: number): CellGridFrame {
  const cols = core.getCols();
  const rows = core.getRows();
  const total = core.getScrollbackCount();
  const cursor = core.getCursor();

  const viewportRows: CellRow[] = [];
  for (let row = 0; row < rows; row++) {
    if (core.isDirtyRow(row)) viewportRows.push(_viewportRow(core, row, cols));
  }

  return {
    cols, rows,
    cursorRow: cursor.row, cursorCol: cursor.col, cursorVisible: cursor.visible,
    altScreen: core.usingAltScreen(),
    full: false,
    viewportRows,
    scrollbackRows: [],
    scrollbackAppend: readScrollbackRangeCells(core, Math.max(prevScrollbackTotal, 0), total),
    scrollbackTotal: total,
    sbBase: 0,
    seq,
  };
}

/** Read scrollback lines [startAbs, endAbs) by OLDEST-FIRST absolute index,
 *  clamped to [0, getScrollbackCount()]. Empty/inverted range → []. Feeds
 *  gridDeltaFrame's append (lines beyond the prior total; clamps when the
 *  ring evicted past it — caller sends a full frame then) and the
 *  get-scrollback-cells backfill RPC (lazy history on attach). */
export function readScrollbackRangeCells(core: TerminalCore, startAbs: number, endAbs: number): CellRow[] {
  const total = core.getScrollbackCount();
  const start = Math.min(Math.max(startAbs, 0), total);
  const end = Math.min(Math.max(endAbs, 0), total);
  if (end <= start) return [];
  const out: CellRow[] = new Array(end - start);
  for (let abs = start; abs < end; abs++) out[abs - start] = _scrollbackRow(core, abs, total);
  return out;
}
