// Grid-to-ANSI serializer for @wterm/core. Plays the role
// `@xterm/addon-serialize` plays for xterm.js: walks scrollback +
// visible buffer + cursor, emits an ANSI string that, when written
// to a fresh wterm instance of the same cols/rows, reproduces the
// captured grid state.
//
// Why we own this: lets us keep ONE VT parser end-to-end (wterm
// browser + wterm headless worker). xterm-headless was a placeholder
// for the serialize-addon capability; this file replaces it.
//
// Algorithm cribbed from xtermjs/xterm.js@master:addon-serialize/src/
// SerializeAddon.ts:
// - Iterate scrollback rows first, then visible buffer rows.
// - Track lastStyle (fg, bg, flags); emit SGR delta only on change.
// - Right-trim trailing default cells per row to keep output small.
// - Emit \r\n at row end (we don't track isWrapped — wterm-core
//   doesn't expose it; an extra \r\n at a wrap point reflows, but
//   on the same cols it's a no-op).
// - Park cursor with absolute CUP at end.
// - Wrap in ESC[?1049h … if usingAltScreen().
//
// Cell shape (verified vercel-labs/wterm:src/cell.zig):
//   { char: u32, fg: u16, bg: u16, flags: u8 }
//   fg/bg: 0..15 ANSI; 16..255 256-palette; 256 = default
//   flags: bold=0x01 dim=0x02 italic=0x04 underline=0x08
//          blink=0x10 reverse=0x20 invisible=0x40 strike=0x80
// (The exported CellData interface also has fgRgb?/bgRgb? for
// alternative cores — we emit them as true-color SGR
// when present.)

import type { TerminalCore, CellData } from "@wterm/core";

const DEFAULT_COLOR = 256;

const F_BOLD       = 0x01;
const F_DIM        = 0x02;
const F_ITALIC     = 0x04;
const F_UNDERLINE  = 0x08;
const F_BLINK      = 0x10;
const F_REVERSE    = 0x20;
const F_INVISIBLE  = 0x40;
const F_STRIKE     = 0x80;

// Frozen module-level constant used as padding for short scrollback
// rows. _serializeRow does not mutate CellData, so sharing one ref
// across thousands of pad slots is safe and avoids per-cell object
// allocation in the serialization hot path.
const BLANK_CELL: CellData = Object.freeze({
  char: 0x20, fg: DEFAULT_COLOR, bg: DEFAULT_COLOR, flags: 0,
}) as CellData;

type Style = {
  fg: number;
  bg: number;
  fgRgb: number | undefined;
  bgRgb: number | undefined;
  flags: number;
};

const DEFAULT_STYLE: Style = {
  fg: DEFAULT_COLOR,
  bg: DEFAULT_COLOR,
  fgRgb: undefined,
  bgRgb: undefined,
  flags: 0,
};

function _styleOf(cell: CellData): Style {
  return {
    fg: cell.fg,
    bg: cell.bg,
    fgRgb: cell.fgRgb,
    bgRgb: cell.bgRgb,
    flags: cell.flags,
  };
}

function _stylesEqual(a: Style, b: Style): boolean {
  return a.fg === b.fg && a.bg === b.bg && a.flags === b.flags
    && a.fgRgb === b.fgRgb && a.bgRgb === b.bgRgb;
}

function _isDefaultStyle(s: Style): boolean {
  return s.flags === 0
    && s.fg === DEFAULT_COLOR && s.bg === DEFAULT_COLOR
    && s.fgRgb === undefined && s.bgRgb === undefined;
}

function _emitSgrDelta(prev: Style, next: Style): string {
  if (_stylesEqual(prev, next)) return "";
  if (_isDefaultStyle(next)) return "\x1b[0m";

  const seq: number[] = [];

  // Foreground.
  if (prev.fg !== next.fg || prev.fgRgb !== next.fgRgb) {
    if (next.fgRgb !== undefined) {
      seq.push(38, 2, (next.fgRgb >>> 16) & 0xff, (next.fgRgb >>> 8) & 0xff, next.fgRgb & 0xff);
    } else if (next.fg === DEFAULT_COLOR) {
      seq.push(39);
    } else if (next.fg >= 16) {
      seq.push(38, 5, next.fg);
    } else {
      // 0..7 → 30..37; 8..15 → 90..97 (bright).
      seq.push(next.fg & 8 ? 90 + (next.fg & 7) : 30 + (next.fg & 7));
    }
  }

  // Background.
  if (prev.bg !== next.bg || prev.bgRgb !== next.bgRgb) {
    if (next.bgRgb !== undefined) {
      seq.push(48, 2, (next.bgRgb >>> 16) & 0xff, (next.bgRgb >>> 8) & 0xff, next.bgRgb & 0xff);
    } else if (next.bg === DEFAULT_COLOR) {
      seq.push(49);
    } else if (next.bg >= 16) {
      seq.push(48, 5, next.bg);
    } else {
      seq.push(next.bg & 8 ? 100 + (next.bg & 7) : 40 + (next.bg & 7));
    }
  }

  // Flag deltas.
  const fp = prev.flags;
  const fn = next.flags;
  const changed = fp ^ fn;
  if (changed & F_REVERSE)   seq.push(fn & F_REVERSE   ? 7 : 27);
  // Bold/dim share SGR-22 off — emitting 22 to turn off ONE wipes the
  // OTHER too. Coalesce: if either is turning off, emit 22 then re-emit
  // whichever one(s) should stay on.
  const boldChanged = (changed & F_BOLD) !== 0;
  const dimChanged  = (changed & F_DIM)  !== 0;
  if (boldChanged || dimChanged) {
    const nextBold = (fn & F_BOLD) !== 0;
    const nextDim  = (fn & F_DIM)  !== 0;
    const prevBold = (fp & F_BOLD) !== 0;
    const prevDim  = (fp & F_DIM)  !== 0;
    const turningOff = (prevBold && !nextBold) || (prevDim && !nextDim);
    if (turningOff) {
      seq.push(22);
      if (nextBold) seq.push(1);
      if (nextDim)  seq.push(2);
    } else {
      if (boldChanged && nextBold) seq.push(1);
      if (dimChanged  && nextDim)  seq.push(2);
    }
  }
  if (changed & F_UNDERLINE) seq.push(fn & F_UNDERLINE ? 4 : 24);
  if (changed & F_BLINK)     seq.push(fn & F_BLINK     ? 5 : 25);
  if (changed & F_INVISIBLE) seq.push(fn & F_INVISIBLE ? 8 : 28);
  if (changed & F_ITALIC)    seq.push(fn & F_ITALIC    ? 3 : 23);
  if (changed & F_STRIKE)    seq.push(fn & F_STRIKE    ? 9 : 29);

  if (seq.length === 0) return "";
  return `\x1b[${seq.join(";")}m`;
}

function _codepointToChar(cp: number): string {
  // Empty / null cell. Caller decides whether to emit space or skip.
  if (cp === 0) return " ";
  return String.fromCodePoint(cp);
}

function _serializeRow(
  cells: CellData[],
  outStyle: Style,
): { text: string; style: Style } {
  // Right-trim: find the rightmost cell that isn't (space + default style).
  let endCol = cells.length;
  while (endCol > 0) {
    const c = cells[endCol - 1];
    const isBlank = (c.char === 0 || c.char === 0x20)
      && c.flags === 0
      && c.fg === DEFAULT_COLOR && c.bg === DEFAULT_COLOR
      && c.fgRgb === undefined && c.bgRgb === undefined;
    if (!isBlank) break;
    endCol--;
  }

  let out = "";
  let style = outStyle;
  for (let col = 0; col < endCol; col++) {
    const cell = cells[col];
    const cellStyle = _styleOf(cell);
    const sgr = _emitSgrDelta(style, cellStyle);
    if (sgr) {
      out += sgr;
      style = cellStyle;
    }
    out += _codepointToChar(cell.char);
  }
  return { text: out, style };
}

/** Serialize the full grid state to ANSI. Writing the returned string
 *  into a fresh wterm instance of the same cols×rows reproduces the
 *  captured visible buffer + scrollback + cursor. */
export function serializeWTerm(core: TerminalCore): string {
  const cols = core.getCols();
  const rows = core.getRows();
  const sbCount = core.getScrollbackCount();
  const altScreen = core.usingAltScreen();

  let out = "";
  // Hard-reset SGR + clear screen + force DECAWM (autowrap) ON so a
  // residual SGR/mode state on the browser-side wterm doesn't bleed
  // into the replayed content. ESC[?7h matters because scrollback may
  // legitimately contain a literal `ESC[?7l` byte (TUI captures, vim
  // recordings cat-ed into the terminal) and serializing those rows
  // before any wider-than-cols line would otherwise silently clip
  // subsequent rows at the SPA's cols.
  out += "\x1b[0m\x1b[?7h\x1b[2J\x1b[H";

  let style = { ...DEFAULT_STYLE };

  // Scrollback FIRST, into main-screen. wterm's main-screen scrollback
  // is what the user expects to scroll up into. We must emit these rows
  // BEFORE switching into alt-screen, otherwise (for an active TUI like
  // claude) the scrollback rows would land in the alt-screen's viewport
  // and corrupt the TUI's display.
  for (let i = 0; i < sbCount; i++) {
    // wterm-core indexes scrollback NEWEST-FIRST: offset 0 = the line just
    // above the visible buffer, offset sbCount-1 = the oldest line. We must
    // emit OLDEST first (top of history) so the replayed scrollback is in
    // chronological order — iterating 0..sbCount emitted it REVERSED, which
    // surfaced as "every refresh/width-change fucks up the order" once the
    // worker served this grid for shells (deep scrollback). Verified: write
    // AAA..FFF on a 3-row grid → off0="DDD" (newest), offLast="AAA" (oldest).
    const off = sbCount - 1 - i;
    // Read the full stored line. Previous code did
    // `Math.min(getScrollbackLineLen(off), cols)` which silently TRUNCATED
    // historic rows whose stored width exceeded the SPA's current cols
    // (PTY ran at 200 cols for an hour → user refreshes browser at 80
    // cols → first 80 chars of every history row preserved, rest gone).
    // wterm-core retains scrollback at the original write-time width;
    // we emit it at that full width. The SPA's wterm will reflow on
    // write at its current cols — keeping the bytes is better than
    // dropping them.
    const lineLen = core.getScrollbackLineLen(off);
    const cells: CellData[] = new Array(lineLen);
    for (let col = 0; col < lineLen; col++) cells[col] = core.getScrollbackCell(off, col);
    // Pad to max(cols, lineLen) so right-trim semantics still apply,
    // but never shorter than the stored content.
    const padTo = Math.max(cols, lineLen);
    while (cells.length < padTo) cells.push(BLANK_CELL);
    const r = _serializeRow(cells, style);
    out += r.text;
    style = r.style;
    out += "\r\n";
  }

  if (altScreen) {
    // Now switch into alt-screen so the VISIBLE buffer (alt-screen
    // content for a TUI) lands in the right place. A future ESC[?1049l
    // from the running TUI puts us back in main-screen — restoring the
    // scrollback we just emitted above.
    out += "\x1b[?1049h\x1b[H";
  }

  // Visible buffer (top → bottom).
  for (let row = 0; row < rows; row++) {
    const cells: CellData[] = new Array(cols);
    for (let col = 0; col < cols; col++) cells[col] = core.getCell(row, col);
    const r = _serializeRow(cells, style);
    out += r.text;
    style = r.style;
    if (row < rows - 1) out += "\r\n";
  }

  // Reset SGR before cursor placement so the cursor is parked under
  // default attrs.
  if (!_isDefaultStyle(style)) out += "\x1b[0m";

  // Park cursor at its captured viewport position (1-based CUP).
  const cur = core.getCursor();
  out += `\x1b[${cur.row + 1};${cur.col + 1}H`;
  if (!cur.visible) out += "\x1b[?25l";

  return out;
}
