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
// `sbDropped` — lines the ring has evicted — is the origin: monotonic index =
// sbDropped + (N-th oldest retained). Callers thread it in; the core reports it
// (emitter.ts::scrollbackOrigin), so this file never has to guess.
//
// HYPERLINKS. This file is the ONLY producer of OSC 8 link identity on the
// wire. The core resolves each cell's link index to a URI and a run key
// (CellData.linkUri/linkKey) and rowToSpans copies both onto the span, for
// viewport AND scrollback rows alike — so retained history and the backfill RPC
// carry links with no separate message, and nothing anywhere re-parses the byte
// stream or matches link text to find them.

import type { TerminalCore, CellData } from "@wterm/core";
import { diag } from "../diag.ts";
import {
  DEFAULT_COLOR, MAX_LINK_URI_BYTES, linkUriWithinCap,
  type CellRow, type CellSpan, type CellGridFrame,
} from "./types.ts";

/** Can cell `a` extend the open run `b`? Style equality is not enough: OSC 8
 *  identity is per-cell, and two adjacent runs can share a style (even a URI)
 *  while being two separately clickable links. Merging those would ship one
 *  span carrying one link and silently lose the other. */
function _sameRun(a: CellData, b: CellSpan, linkKey: string | undefined): boolean {
  return a.fg === b.fg && a.bg === b.bg && a.flags === b.flags
    && a.fgRgb === b.fgRgb && a.bgRgb === b.bgRgb
    && linkKey === b.linkKey;
}

/** Does this URI fit the wire cap? One-entry memo keyed by BOTH key and URI: a
 *  link covers a RUN of cells and usually repeats down consecutive rows, so the
 *  exact byte count is paid once per distinct link rather than once per cell,
 *  and a key reused by another core instance can never hit on a stale URI. */
let _memoKey: string | undefined;
let _memoUri: string | undefined;
let _memoOk = false;
function _linkOk(key: string, uri: string): boolean {
  if (key === _memoKey && uri === _memoUri) return _memoOk;
  const ok = linkUriWithinCap(uri);
  _memoKey = key;
  _memoUri = uri;
  _memoOk = ok;
  return ok;
}

/** The link a cell contributes to its span, or undefined when it has none, when
 *  the core gave a URI with no run key, or when the URI blew the wire cap — the
 *  last of which drops the LINK and keeps the text, because a truncated URI
 *  would point somewhere else entirely. */
function _linkKeyOf(c: CellData): string | undefined {
  const uri = c.linkUri;
  const key = c.linkKey;
  if (uri === undefined || key === undefined || uri.length === 0 || key.length === 0) return undefined;
  return _linkOk(key, uri) ? key : undefined;
}

/** Trimmable padding. A LINKED cell never qualifies however blank it looks: its
 *  columns are clickable, so trimming them would shrink the hyperlink. */
function _isBlankDefault(c: CellData): boolean {
  return (c.char === 0 || c.char === 0x20)
    && c.flags === 0
    && c.fg === DEFAULT_COLOR && c.bg === DEFAULT_COLOR
    && c.fgRgb === undefined && c.bgRgb === undefined
    && c.linkUri === undefined;
}

function _char(cp: number): string {
  if (cp === 0) return " ";
  try { return String.fromCodePoint(cp); } catch { return " "; }
}

/** Display width of a cell. 1 = narrow, 2 = wide LEAD, 0 = wide CONTINUATION.
 *  A core that omits it only has narrow cells. */
function _width(c: CellData): number {
  return c.width ?? 1;
}

/** Run-length encode one row of cells into right-trimmed style spans.
 *  Trailing default-style blanks are dropped (empty row → []).
 *
 *  A wide glyph's width-0 continuation column is FOLDED into its lead span's
 *  `columns` — never emitted as its own space-bearing cell, which would paint
 *  "中  文" for a grid holding "中文" and shift every column right of it. An
 *  orphan continuation (no wide lead before it, e.g. a lead overwritten by a
 *  narrow char on a core that leaves the tail behind) paints as one blank
 *  column so the row still spans exactly its cells.
 *
 *  OSC 8 identity rides along: a contiguous same-style run of ONE link is one
 *  span carrying `linkUri`/`linkKey`, and a link boundary breaks the run even
 *  when the style is identical on both sides. Both the viewport and the
 *  scrollback readers below go through here, so retained history carries links
 *  with no separate message. */
export function rowToSpans(cells: CellData[]): CellSpan[] {
  // Right-trim trailing default-style blanks. A width-0 cell backed by a wide
  // lead is that glyph's SECOND COLUMN, not padding: trimming it would shrink
  // the lead's occupancy and un-align the row's tail.
  let end = cells.length;
  while (end > 0) {
    const c = cells[end - 1];
    if (!_isBlankDefault(c)) break;
    if (_width(c) === 0) {
      let lead = end - 2;
      while (lead >= 0 && _width(cells[lead]) === 0) lead--;
      if (lead >= 0 && _width(cells[lead]) >= 2) break;
    }
    end--;
  }

  const spans: CellSpan[] = [];
  let run: CellSpan | null = null;
  let col = 0;
  // At most ONE diagnostic per row, whatever the cells do: a row alternating
  // two over-cap links would otherwise log per cell, per frame, forever.
  let droppedLinks = 0;
  let droppedHead = "";
  let droppedUnits = 0;
  while (col < end) {
    const c = cells[col];
    // Resolved once per cell and reused by both the run test and the span it
    // may open: undefined when the cell has no link or its URI blew the cap.
    const linkKey = _linkKeyOf(c);
    if (c.linkUri !== undefined && linkKey === undefined) {
      droppedLinks++;
      if (droppedHead === "") {
        droppedHead = c.linkUri.slice(0, 64);
        droppedUnits = c.linkUri.length;
      }
    }
    const linkUri = linkKey === undefined ? undefined : c.linkUri;
    // Coalesce only cells that are exactly one column AND one UTF-16 code unit,
    // so a run keeps column i == text[i]. An astral codepoint or a cluster is
    // one cell of several code units and stays atomic.
    if (_width(c) === 1 && c.chars === undefined && c.char <= 0xffff) {
      if (run && _sameRun(c, run, linkKey)) {
        run.text += _char(c.char);
        run.columns++;
      } else {
        run = {
          text: _char(c.char), columns: 1,
          fg: c.fg, bg: c.bg, flags: c.flags, fgRgb: c.fgRgb, bgRgb: c.bgRgb,
          linkUri, linkKey,
        };
        spans.push(run);
      }
      col++;
      continue;
    }
    // Atomic cell: a wide lead, an astral codepoint, a cluster, or an orphan
    // continuation. None of them may merge with a neighbour.
    run = null;
    const width = _width(c);
    let columns = 1;
    if (width >= 2) {
      // The lead owns every continuation column that follows it. A lead in the
      // last column of a truncated row has none and occupies exactly one.
      while (col + columns < cells.length && _width(cells[col + columns]) === 0) columns++;
    }
    spans.push({
      text: width === 0 ? " " : (c.chars ?? _char(c.char)), columns,
      fg: c.fg, bg: c.bg, flags: c.flags, fgRgb: c.fgRgb, bgRgb: c.bgRgb,
      linkUri, linkKey,
    });
    col += columns;
  }
  if (droppedLinks > 0) {
    // Bounded on purpose: the whole fault is an unbounded URI, so only its
    // length and a short prefix are ever recorded.
    diag("cell.link_dropped", {
      cells: droppedLinks,
      cap_bytes: MAX_LINK_URI_BYTES,
      uri_units: droppedUnits,
      uri_head: droppedHead,
    });
  }
  return spans;
}

/** One viewport row's cells as spans. Exported because find-in-scrollback maps
 *  its text offsets into the SAME column space the wire carries, and deriving
 *  that from a second, private cell reader is how the two drift apart. */
export function viewportRowSpans(core: TerminalCore, row: number, cols: number): CellSpan[] {
  const cells: CellData[] = new Array(cols);
  for (let col = 0; col < cols; col++) cells[col] = core.getCell(row, col);
  return rowToSpans(cells);
}

/** One retained line's cells as spans, addressed by the core's NEWEST-FIRST
 *  offset. wterm keeps a line at its write-time width, which can exceed the
 *  current grid — the stored length, never `cols`, bounds the read. */
export function scrollbackOffsetSpans(core: TerminalCore, offset: number): CellSpan[] {
  const len = core.getScrollbackLineLen(offset);
  const cells: CellData[] = new Array(len);
  for (let col = 0; col < len; col++) cells[col] = core.getScrollbackCell(offset, col);
  return rowToSpans(cells);
}

/** Read one scrollback line by MONOTONIC absolute index. `total` is the
 *  current getScrollbackCount() and `sbDropped` the ring's eviction origin,
 *  so the retained position is monoIndex - sbDropped. */
function _scrollbackRow(core: TerminalCore, monoIndex: number, total: number, sbDropped: number): CellRow {
  const offset = total - 1 - (monoIndex - sbDropped); // oldest-first → newest-first offset
  return { index: monoIndex, spans: scrollbackOffsetSpans(core, offset) };
}


/** Full snapshot: whole viewport plus an optional newest-N history slice.
 * `tailRows=0` is the authoritative viewport-only contract; unset remains
 * available to pure callers that need a complete grid. Indices are monotonic. */
export function gridToCellFrame(
  core: TerminalCore,
  seq: number,
  gridEpoch: string,
  tailRows?: number,
  sbDropped = 0,
): CellGridFrame {
  const cols = core.getCols();
  const rows = core.getRows();
  const total = core.getScrollbackCount();
  const cursor = core.getCursor();

  const viewportRows: CellRow[] = new Array(rows);
  for (let row = 0; row < rows; row++) viewportRows[row] = { index: row, spans: viewportRowSpans(core, row, cols) };

  const monoTotal = sbDropped + total;
  const sbBase = tailRows === undefined ? sbDropped : Math.max(sbDropped, monoTotal - tailRows);
  const scrollbackRows: CellRow[] = new Array(monoTotal - sbBase);
  for (let i = sbBase; i < monoTotal; i++) scrollbackRows[i - sbBase] = _scrollbackRow(core, i, total, sbDropped);

  return {
    gridEpoch,
    cols, rows,
    cursorRow: cursor.row, cursorCol: cursor.col, cursorVisible: cursor.visible,
    altScreen: core.usingAltScreen(),
    cursorKeysApp: core.cursorKeysApp(),
    bracketedPaste: core.bracketedPaste(),
    // Optional on the TerminalCore interface (Roost's patched WASM always
    // implements them; assertRoostWasmAbi proves it at worker boot), so read
    // them the same way session-emit.ts reads synchronizedOutput.
    mouseTracking: core.mouseTracking?.() ?? 0,
    mouseSgr: core.mouseSgr?.() ?? false,
    focusEvents: core.focusEvents?.() ?? false,
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
export function gridDeltaFrame(
  core: TerminalCore,
  prevMonoTotal: number,
  seq: number,
  gridEpoch: string,
  sbDropped = 0,
): CellGridFrame {
  const cols = core.getCols();
  const rows = core.getRows();
  const total = core.getScrollbackCount();
  const cursor = core.getCursor();

  const viewportRows: CellRow[] = [];
  for (let row = 0; row < rows; row++) {
    if (core.isDirtyRow(row)) viewportRows.push({ index: row, spans: viewportRowSpans(core, row, cols) });
  }

  return {
    gridEpoch,
    cols, rows,
    cursorRow: cursor.row, cursorCol: cursor.col, cursorVisible: cursor.visible,
    altScreen: core.usingAltScreen(),
    cursorKeysApp: core.cursorKeysApp(),
    bracketedPaste: core.bracketedPaste(),
    mouseTracking: core.mouseTracking?.() ?? 0,
    mouseSgr: core.mouseSgr?.() ?? false,
    focusEvents: core.focusEvents?.() ?? false,
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
