// Cell reader — turns a @wterm/core grid into a CellGridFrame (R11).
// Runs on the worker (the single emulator owner). Pure given the core:
// reads via getCell / getScrollbackCell, never mutates. rowToSpans is the
// run-length encoder (group consecutive equal-style cells, right-trim
// trailing default spaces) and is the inverse of the renderer's paint.
//
// Indexing note (verified in wterm-serialize.ts): wterm-core stores
// scrollback NEWEST-FIRST — offset 0 = line just above the viewport,
// offset count-1 = oldest. We expose scrollback OLDEST-FIRST so the client
// paints top→bottom and splices appends.
//
// The exposed index is MONOTONIC, not "N-th oldest retained". The ring is
// bounded (10k lines), so once it saturates getScrollbackCount() pins and the
// retained-index origin slides by one on every eviction: absolute row 0 named
// a different line every line pushed, appends computed from the count delta
// went empty, and every held index on every viewer silently re-aliased.
// `sbDropped` — lines the ring has evicted since the core was created — is the
// origin: monotonic index = sbDropped + (N-th oldest retained). Callers thread
// it in; emitter.ts owns the counter (see scrollbackShift).

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

/** Read one scrollback line by MONOTONIC absolute index. `total` is the
 *  current getScrollbackCount() and `sbDropped` the ring's eviction origin,
 *  so the retained position is monoIndex - sbDropped. */
function _scrollbackRow(core: TerminalCore, monoIndex: number, total: number, sbDropped: number): CellRow {
  const offset = total - 1 - (monoIndex - sbDropped); // oldest-first → newest-first offset
  const len = core.getScrollbackLineLen(offset);
  const cells: CellData[] = new Array(len);
  for (let col = 0; col < len; col++) cells[col] = core.getScrollbackCell(offset, col);
  return { index: monoIndex, spans: rowToSpans(cells) };
}

// Lines sampled from the newest end of the ring to identify it.
const SB_SIG_ROWS = 6;
// A full retained row is 80 cells on the standard terminal. Hashing its
// codepoints is intentional: wterm pads every short row with spaces, so a
// physical-tail sample sees only padding for `CELLLINE-123` and aliases every
// row. A false match silently loses history; this cold, saturation-only probe
// instead pays a few hundred WASM reads to preserve identity.
// Max lines we will scan for the previous tail before giving up and forcing a
// full frame. A saturating writer CAN exceed this inside one 16ms coalesce
// window; a full frame then is correct (and no dearer than a delta whose
// append spans the same rows), just less efficient.
export const SB_SHIFT_SCAN_MAX = 256;

/** Identity probe for the newest `SB_SIG_ROWS` retained lines, taken `atOffset`
 *  lines back from the newest. It samples every cell, but encodes only a
 *  compact rolling hash per line. "" when the ring is too shallow. */
export function scrollbackTailSig(core: TerminalCore, atOffset = 0): string {
  const total = core.getScrollbackCount();
  if (total < atOffset + SB_SIG_ROWS) return "";
  let sig = "";
  for (let i = 0; i < SB_SIG_ROWS; i++) {
    const off = atOffset + i;
    const len = core.getScrollbackLineLen(off);
    let hash = 2_166_136_261;
    for (let col = 0; col < len; col++) {
      hash = Math.imul(hash ^ core.getScrollbackCell(off, col).char, 16_777_619);
    }
    sig += `${len}:${hash >>> 0};`;
  }
  return sig;
}

/** Lines evicted since the emit that produced `prevSig`, or null when the
 *  previous tail is no longer within SB_SHIFT_SCAN_MAX (caller must reframe —
 *  an honest full frame beats a silently wrong delta).
 *
 *  Exact sampled-row matches are overwhelmingly likely to identify one shift;
 *  a 32-bit rolling hash collision is treated as a reframe risk only if the
 *  same signature appears at more than one offset. */
export function scrollbackShift(core: TerminalCore, prevSig: string): number | null {
  if (prevSig === "") return 0;
  const firstLen = Number(prevSig.slice(0, prevSig.indexOf(":")));
  const total = core.getScrollbackCount();
  let found: number | null = null;
  for (let d = 0; d <= SB_SHIFT_SCAN_MAX; d++) {
    if (d + SB_SIG_ROWS > total) break;
    if (core.getScrollbackLineLen(d) !== firstLen) continue;
    if (scrollbackTailSig(core, d) !== prevSig) continue;
    if (found !== null) return null;
    found = d;
  }
  return found;
}

/** Full snapshot: whole viewport + scrollback + cursor. `tailRows` caps the
 *  scrollback to the newest N lines; unset = complete retained history.
 *  Indices are monotonic — `sbDropped` is the ring's eviction origin. */
export function gridToCellFrame(core: TerminalCore, seq: number, tailRows?: number, sbDropped = 0): CellGridFrame {
  const cols = core.getCols();
  const rows = core.getRows();
  const total = core.getScrollbackCount();
  const cursor = core.getCursor();

  const viewportRows: CellRow[] = new Array(rows);
  for (let row = 0; row < rows; row++) viewportRows[row] = _viewportRow(core, row, cols);

  const monoTotal = sbDropped + total;
  const sbBase = tailRows === undefined ? sbDropped : Math.max(sbDropped, monoTotal - tailRows);
  const scrollbackRows: CellRow[] = new Array(monoTotal - sbBase);
  for (let i = sbBase; i < monoTotal; i++) scrollbackRows[i - sbBase] = _scrollbackRow(core, i, total, sbDropped);

  return {
    cols, rows,
    cursorRow: cursor.row, cursorCol: cursor.col, cursorVisible: cursor.visible,
    altScreen: core.usingAltScreen(),
    cursorKeysApp: core.cursorKeysApp(),
    bracketedPaste: core.bracketedPaste(),
    full: true,
    viewportRows,
    scrollbackRows,
    scrollbackAppend: [],
    scrollbackTotal: monoTotal,
    sbBase,
    seq,
  };
}

/** Delta frame built directly from the core's dirty-row tracking — the
 *  worker's hot path. Changed viewport rows come from isDirtyRow (caller
 *  MUST call core.clearDirty() AFTER this returns), scrollback append from
 *  the monotonic-total delta, cursor always. Caller decides full-vs-delta:
 *  send a FULL frame (gridToCellFrame) on attach, resize, alt toggle, or a
 *  monotonic-total rewind (reset). */
export function gridDeltaFrame(core: TerminalCore, prevMonoTotal: number, seq: number, sbDropped = 0): CellGridFrame {
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
    cursorKeysApp: core.cursorKeysApp(),
    bracketedPaste: core.bracketedPaste(),
    full: false,
    viewportRows,
    scrollbackRows: [],
    scrollbackAppend: readScrollbackRangeCells(core, Math.max(prevMonoTotal, 0), sbDropped + total, sbDropped),
    scrollbackTotal: sbDropped + total,
    sbBase: 0,
    seq,
  };
}

/** Read scrollback lines [startMono, endMono) by MONOTONIC absolute index,
 *  clamped to the retained window [sbDropped, sbDropped + count]. Empty or
 *  inverted range → []. Feeds gridDeltaFrame's append (lines beyond the prior
 *  total; clamps when the ring evicted past it) and the get-scrollback-cells
 *  backfill RPC (lazy history on attach). */
export function readScrollbackRangeCells(core: TerminalCore, startMono: number, endMono: number, sbDropped = 0): CellRow[] {
  const total = core.getScrollbackCount();
  const lo = sbDropped, hi = sbDropped + total;
  const start = Math.min(Math.max(startMono, lo), hi);
  const end = Math.min(Math.max(endMono, lo), hi);
  if (end <= start) return [];
  const out: CellRow[] = new Array(end - start);
  for (let abs = start; abs < end; abs++) out[abs - start] = _scrollbackRow(core, abs, total, sbDropped);
  return out;
}
