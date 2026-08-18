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
// no span, so a blank row = empty spans[]). A frame is either FULL (whole
// viewport, no historical rows) or a DELTA (changed viewport rows plus newly
// appended scrollback rows).
//
// COLUMN OCCUPANCY. `CellSpan.columns` — not `text.length` — is the terminal
// width of a span, because the core's cell model is neither one column per
// code unit nor one column per code point:
//   * A double-width glyph is a width-2 LEAD cell followed by a width-0
//     CONTINUATION cell. The continuation is never emitted as its own
//     space-bearing span; it is folded into the lead's `columns`.
//   * An astral codepoint ("🐙") is one column-or-two of grid but two code
//     units; a grapheme cluster (CellData.chars) is one cell of many.
// So a span is either a coalesced narrow RUN — every cell one column and one
// code unit, hence `columns === text.length` and grid column i == text[i] — or
// an ATOMIC single cell that must never be sliced (spanIsAtomic below).

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

// ── OSC 8 hyperlinks ───────────────────────────────────────────────────────
// Links are CORE-AUTHORED: the emulator resolves each cell's OSC 8 link index
// to a URI and a run key, and that per-cell identity is the whole mechanism.
// Nothing parses the byte stream for links, and nothing matches link TEXT.

/** Wire bound on one span's link URI, in UTF-8 BYTES. An application can put a
 *  megabyte inside a single OSC 8 payload; the core stores it, and every frame
 *  touching those cells would then carry it on every span. Over-cap URIs lose
 *  the LINK and keep the TEXT — truncating a URI would silently retarget the
 *  click at a different destination, which is worse than no link at all. */
export const MAX_LINK_URI_BYTES = 2048;

/** True when `uri` fits MAX_LINK_URI_BYTES once UTF-8 encoded. Exact and
 *  allocation-free: this runs on the per-cell span-encoding path, so encoding
 *  every URI to measure it is not affordable. UTF-8 is never shorter than the
 *  UTF-16 code-unit count and never longer than 3 bytes per unit (a surrogate
 *  PAIR is 2 units → 4 bytes), which settles almost every call in one compare. */
export function linkUriWithinCap(uri: string): boolean {
  if (uri.length > MAX_LINK_URI_BYTES) return false;
  if (uri.length * 3 <= MAX_LINK_URI_BYTES) return true;
  let bytes = 0;
  for (let i = 0; i < uri.length; i++) {
    const unit = uri.charCodeAt(i);
    if (unit < 0x80) bytes += 1;
    else if (unit < 0x800) bytes += 2;
    else if (
      unit >= 0xd800 && unit <= 0xdbff && i + 1 < uri.length
      && (uri.charCodeAt(i + 1) & 0xfc00) === 0xdc00
    ) { bytes += 4; i++; }
    else bytes += 3;  // BMP 3-byter, or a lone surrogate the decoder replaced
    if (bytes > MAX_LINK_URI_BYTES) return false;
  }
  return true;
}

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
  /** REQUIRED terminal columns this span occupies (>= 1). See the column
   *  occupancy note above: equals text.length only for a coalesced narrow run. */
  columns: number;
  /** Core-authored OSC 8 URI for every cell of this span, or undefined when the
   *  run carries no hyperlink. This is Roost's ONLY hyperlink source — nothing
   *  re-derives links from bytes and nothing text-matches them. */
  linkUri?: string;
  /** Opaque per-core RUN identity for the OSC 8 link (@wterm/core
   *  CellData.linkKey). Present exactly when `linkUri` is. Two separate OSC 8
   *  emissions can share one URI and must stay two independently clickable
   *  spans, so this — not `linkUri` — is the coalescing and grouping key. It is
   *  meaningful only inside one core instance: never persist or compare it
   *  across a core rebuild. */
  linkKey?: string;
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

/** Mouse reporting mode the foreground application requested, exactly as
 *  @wterm/core reports it: 0 = none, 1000 = press/release, 1002 = press/release
 *  plus motion while held. The core folds legacy mode 9 and any-motion 1003
 *  into these three, so no other value is representable. */
export type MouseTracking = 0 | 1000 | 1002;

/** Narrow a wire integer to a MouseTracking. An unknown value is "no tracking
 *  requested": the browser then keeps native selection and scroll, which is the
 *  safe reading of a mode neither side agreed on. */
export function asMouseTracking(raw: number): MouseTracking {
  return raw === 1000 || raw === 1002 ? raw : 0;
}

export interface CellGridFrame {
  /** Opaque identity for the worker-side grid numbering epoch. */
  gridEpoch: string;
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
  /** Mouse reporting the FOREGROUND APPLICATION requested (DECSET 1000/1002),
   *  read off the core. 0 = never requested, so the browser keeps native
   *  selection and scroll; the core folds modes 9 and 1003 to 0. */
  mouseTracking: MouseTracking;
  /** DECSET 1006; picks the SPA's mouse-report encoding (SGR-1006 vs X10). */
  mouseSgr: boolean;
  /** DECSET 1004; the SPA reports real textarea focus/blur as CSI I / CSI O. */
  focusEvents: boolean;
  /** true = full snapshot (viewportRows = all rows, scrollbackRows empty).
   *  false = delta (viewportRows = changed only, scrollbackAppend = lines
   *  pushed since the prior frame). */
  full: boolean;
  viewportRows: CellRow[];
  /** Full frames carry no retained history. sbBase still represents the full
   *  scrollback depth so history remains available through explicit paging. */
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

// Authoritative FULL frames are viewport-only. Retained scrollback depth still
// rides in scrollbackTotal/sbBase and is fetched only on explicit scroll/find.
export const SB_SNAPSHOT_HISTORY_ROWS = 0;

// ── Column geometry ────────────────────────────────────────────────────
// The single implementation of "which grid column is this text at", shared by
// the worker's find RPC (offset → column) and the SPA's paint, hit splitting,
// and predictive echo (column → text). Nothing may re-derive columns from
// text.length; a wide glyph and an astral codepoint both break that identity.

/** True when the span is ONE grid cell that must be painted and highlighted
 *  whole: a wide lead, an astral codepoint, or a grapheme cluster. False for a
 *  coalesced narrow run, whose grid column i is exactly text[i].
 *
 *  A run only ever holds one-code-unit narrow cells (rowToSpans), so a
 *  surrogate code unit proves a single astral glyph even where its column count
 *  happens to equal its code-unit count ("🐙": 2 columns, 2 code units). */
export function spanIsAtomic(span: CellSpan): boolean {
  if (span.columns !== span.text.length) return true;
  for (let i = 0; i < span.text.length; i++) {
    const unit = span.text.charCodeAt(i);
    if (unit >= 0xd800 && unit <= 0xdfff) return true;
  }
  return false;
}

/** Terminal columns the spans occupy. Right-trimmed rows report less than the
 *  grid width; never more (assertCellRowSpans enforces it on decode). */
export function rowColumns(spans: readonly CellSpan[]): number {
  let columns = 0;
  for (const span of spans) columns += span.columns;
  return columns;
}

/** Row text exactly as painted — continuation columns contribute nothing, so
 *  "中文" is two characters over four columns. */
export function spansText(spans: readonly CellSpan[]): string {
  let text = "";
  for (const span of spans) text += span.text;
  return text;
}

/** Grid column of code-unit `offset` in spansText(spans). An offset inside an
 *  atomic span resolves to that span's FIRST column; past the end it resolves to
 *  the row's column count. */
export function textOffsetToColumn(spans: readonly CellSpan[], offset: number): number {
  let at = 0;
  let col = 0;
  for (const span of spans) {
    const next = at + span.text.length;
    if (offset < next) return spanIsAtomic(span) ? col : col + (offset - at);
    at = next;
    col += span.columns;
  }
  return col;
}

/** Exclusive-end companion: the column one past the last column covered by a
 *  range ending at code-unit `offset`. An atomic span is covered whole, so a
 *  match can never end halfway through a grapheme. */
export function textOffsetToColumnEnd(spans: readonly CellSpan[], offset: number): number {
  let at = 0;
  let col = 0;
  for (const span of spans) {
    if (offset <= at) return col;
    const next = at + span.text.length;
    if (offset < next) return spanIsAtomic(span) ? col + span.columns : col + (offset - at);
    at = next;
    col += span.columns;
  }
  return col;
}

/** Column range of a [offset, offset+length) code-unit range of
 *  spansText(spans) — the conversion every match/highlight crosses. */
export function textRangeToColumns(
  spans: readonly CellSpan[], offset: number, length: number,
): { col: number; columns: number } {
  const col = textOffsetToColumn(spans, offset);
  if (length <= 0) return { col, columns: 0 };
  return { col, columns: Math.max(textOffsetToColumnEnd(spans, offset + length) - col, 1) };
}

/** Text painted at grid column `col`: the whole glyph for an atomic span (both
 *  columns of a wide one), one character of a run, "" past the row's end. */
export function columnText(spans: readonly CellSpan[], col: number): string {
  if (col < 0) return "";
  let at = 0;
  for (const span of spans) {
    const next = at + span.columns;
    if (col < next) return spanIsAtomic(span) ? span.text : (span.text[col - at] ?? "");
    at = next;
  }
  return "";
}

/** Decode-side contract check: every span must claim at least one column and
 *  carry text, and a viewport row may not claim more columns than the grid has.
 *  `maxColumns <= 0` skips the width bound — a retained scrollback line keeps
 *  its original write-time width, which can exceed the current grid.
 *
 *  Link identity is checked here too, because a span is what gets CLICKED: a
 *  URI over the wire cap, an empty one, or a key with no URI to open would each
 *  reach the user as a broken or unbounded link rather than as a decode error. */
export function assertCellRowSpans(row: CellRow, maxColumns: number): void {
  let columns = 0;
  for (const span of row.spans) {
    if (!Number.isInteger(span.columns) || span.columns < 1) {
      throw new Error(
        `cell row ${row.index}: span ${JSON.stringify(span.text)} claims ${span.columns} columns`,
      );
    }
    if (span.text.length === 0) {
      throw new Error(`cell row ${row.index}: span at column ${columns} carries no text`);
    }
    // `linkKey` is the run identity a renderer groups and coalesces by; a URI
    // without it cannot be grouped, and a key without a URI links nowhere.
    if (span.linkUri !== undefined) {
      if (span.linkUri.length === 0) {
        throw new Error(`cell row ${row.index}: span at column ${columns} carries an empty link_uri`);
      }
      if (!linkUriWithinCap(span.linkUri)) {
        throw new Error(
          `cell row ${row.index}: span at column ${columns} carries a link_uri over ${MAX_LINK_URI_BYTES} bytes`,
        );
      }
      if (span.linkKey === undefined || span.linkKey.length === 0) {
        throw new Error(`cell row ${row.index}: span at column ${columns} carries link_uri with no link_key`);
      }
    } else if (span.linkKey !== undefined) {
      throw new Error(`cell row ${row.index}: span at column ${columns} carries link_key with no link_uri`);
    }
    columns += span.columns;
  }
  if (maxColumns > 0 && columns > maxColumns) {
    throw new Error(
      `cell row ${row.index}: spans occupy ${columns} columns of a ${maxColumns}-column grid`,
    );
  }
}
