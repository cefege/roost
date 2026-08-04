// Cell-grid wire types — the pre-rendered terminal payload (R11).
// The worker owns the ONE emulator (wterm-core) and ships PRE-RENDERED
// styled CELLS; the SPA paints them and NEVER re-parses VT or reflows.
// This file is the cell source of truth; proto frames (worker_transport /
// sync) mirror it, the worker fills it via grid-to-cells.ts, the SPA
// renders it. Style fields mirror @wterm/core CellData exactly (see
// wterm-serialize.ts cell-shape note) so a span round-trips byte-for-style.
//
// A "span" is a run of consecutive cells sharing one style. A "row" is an
// ordered list of spans (right-trimmed: trailing default-style spaces emit
// no span, so a blank row = empty spans[]). A "frame" is either FULL (whole
// viewport + whole scrollback, sent on attach/resize) or a DELTA (only the
// changed viewport rows + newly-appended scrollback + cursor).

export const DEFAULT_COLOR = 256;

// Style flag bits — identical layout to @wterm/core CellData.flags and to
// wterm-serialize.ts (F_*). The renderer maps these to CSS.
export const CELL_BOLD = 0x01;
export const CELL_DIM = 0x02;
export const CELL_ITALIC = 0x04;
export const CELL_UNDERLINE = 0x08;
export const CELL_BLINK = 0x10;
export const CELL_REVERSE = 0x20;
export const CELL_INVISIBLE = 0x40;
export const CELL_STRIKE = 0x80;

export interface CellSpan {
  /** Concatenated codepoints of the run (NUL cells render as a space). */
  text: string;
  /** 0..15 ANSI, 16..255 palette, 256 = default. */
  fg: number;
  bg: number;
  /** CELL_* bitfield. */
  flags: number;
  /** Resolved 24-bit fg (0xRRGGBB) when the core provides true color; else undefined. */
  fgRgb?: number;
  bgRgb?: number;
}

export interface CellRow {
  /**
   * Row coordinate. For viewport rows: 0..rows-1 within the current grid.
   * For scrollback rows: the absolute scrollback line number (0 = oldest
   * retained line), used to splice deltas onto the client's held frame.
   */
  index: number;
  spans: CellSpan[];
}

export interface CellGridFrame {
  cols: number;
  rows: number;
  cursorRow: number;
  cursorCol: number;
  cursorVisible: boolean;
  altScreen: boolean;
  /** DECCKM app-cursor-keys mode; drives the SPA keystroke encoder (arrows). */
  cursorKeysApp: boolean;
  /** DECSET 2004 bracketed-paste mode; drives the SPA paste wrapping. */
  bracketedPaste: boolean;
  /** true = full snapshot (viewportRows = all rows, scrollbackRows = all
   *  retained lines). false = delta (viewportRows = changed only,
   *  scrollbackAppend = lines pushed since the prior frame). */
  full: boolean;
  viewportRows: CellRow[];
  /** Full frames only: retained scrollback lines, oldest → newest. May be
   *  only a TAIL of history: rows [sbBase, sbBase+scrollbackRows.length) of
   *  [0, scrollbackTotal) — the rest is pulled lazily per-viewer via the
   *  get-scrollback-cells RPC. */
  scrollbackRows: CellRow[];
  /** Delta frames only: scrollback lines appended since the prior frame,
   *  oldest → newest, index = absolute line number. */
  scrollbackAppend: CellRow[];
  /** Total retained scrollback line count at frame time. */
  scrollbackTotal: number;
  /** Absolute index of scrollbackRows[0]. 0 = complete from the oldest
   *  retained line. Delta frames: always 0 (scrollbackRows empty; appends
   *  carry their own absolute index). */
  sbBase: number;
  /** Per-channel monotonic frame seq; lets the client detect gaps/resets. */
  seq: number;
}

// Scrollback tail carried in FULL frames (attach/reframe). One SB_BLOCK
// (cellRenderer.ts) — covers scrollbackText(250) keyterm reads and gives
// immediate scroll-up context; deeper history arrives via the per-viewer
// get-scrollback-cells backfill.
// This is the ONLY tail size: a full frame is constant-size (~40 KiB) no
// matter how far a viewer fell behind or how deep the session is, so a reveal
// never waits on history. A viewer whose held window falls below the frame's
// sbBase collapses to this tail (cellRenderer apply()'s windowUnreachable path,
// pinned to the literal bottom) and is refilled behind the reader by
// scrollbackBackfill.ts's parallel drain.
export const SB_SNAPSHOT_TAIL_ROWS = 250;
