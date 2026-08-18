// Pure cell→DOM row rendering: the xterm-palette→CSS mapping, one span's inline
// style, one row's element (optionally with find-match highlights split out),
// and the allocation-free row hash the viewport diff compares.
//
// Split out of cellRenderer.ts (400-line cap). No state, no scroll, no frames —
// CellGridRenderer composes these and owns everything stateful. Unit-tested in
// apps/web/tests/cellRenderer.test.ts.

import type { CellRow, CellSpan } from "@roost/shared/cell";
import {
  CELL_BOLD, CELL_DIM, CELL_ITALIC, CELL_UNDERLINE, CELL_BLINK,
  CELL_REVERSE, CELL_INVISIBLE, CELL_STRIKE, DEFAULT_COLOR,
  rowColumns, spanIsAtomic, spansText,
} from "@roost/shared/cell";

// xterm 256-palette → CSS. 0..15 map to the themed --term-color-N vars;
// 16..231 are the 6×6×6 cube; 232..255 are the 24-step grayscale ramp.
export function ansi256ToCss(n: number): string {
  if (n < 16) return `var(--term-color-${n})`;
  if (n >= 232) { const v = 8 + (n - 232) * 10; return `rgb(${v},${v},${v})`; }
  const c = n - 16;
  const steps = [0, 95, 135, 175, 215, 255];
  const r = steps[Math.floor(c / 36) % 6];
  const g = steps[Math.floor(c / 6) % 6];
  const b = steps[c % 6];
  return `rgb(${r},${g},${b})`;
}

function colorCss(color: number, rgb: number | undefined, isFg: boolean): string {
  if (rgb !== undefined) {
    return `#${((rgb >>> 0) & 0xffffff).toString(16).padStart(6, "0")}`;
  }
  if (color === DEFAULT_COLOR) return isFg ? "var(--term-fg)" : "var(--term-bg)";
  return ansi256ToCss(color);
}

/** An atomic span is ONE cell whose glyph advance the font decides — a wide CJK
 *  ideograph, an emoji, an astral codepoint. Pinning its box to its declared
 *  column count is what makes a painted row exactly `cols` wide: without it a
 *  2-column emoji that the font advances 1.6ch drags every later column left.
 *  A coalesced narrow run needs no box; its ch advance is already exact. */
function occupancyCss(s: CellSpan): string {
  return spanIsAtomic(s) ? `display:inline-block;width:${s.columns}ch` : "";
}

/** Inline CSS for one span. reverse swaps fg/bg; invisible hides text.
 *  Pure — unit-tested in cellRenderer.test.ts. */
export function spanStyle(s: CellSpan): string {
  const reverse = (s.flags & CELL_REVERSE) !== 0;
  let fg = colorCss(s.fg, s.fgRgb, true);
  let bg = colorCss(s.bg, s.bgRgb, false);
  if (reverse) { const t = fg; fg = bg; bg = t; }
  const parts: string[] = [`color:${fg}`];
  // Only emit background when non-default (or reversed) — keeps the DOM lean
  // and lets the container --term-bg show through for blank cells.
  if (s.bg !== DEFAULT_COLOR || s.bgRgb !== undefined || reverse) parts.push(`background:${bg}`);
  const deco = spanDecorationStyle(s);
  if (deco) parts.push(deco);
  return parts.join(";");
}

/** Everything in spanStyle EXCEPT colour — including the column-occupancy box,
 *  so a highlighted wide glyph keeps its width. A find-match span uses this so
 *  the .cell-find-hit class actually owns its background and text colour: an
 *  inline `color:`/`background:` beats any class rule, so emitting the run's own
 *  colours on a highlighted span would leave matches un-highlighted. */
export function spanDecorationStyle(s: CellSpan): string {
  const parts: string[] = [];
  const box = occupancyCss(s);
  if (box) parts.push(box);
  if (s.flags & CELL_BOLD) parts.push("font-weight:bold");
  if (s.flags & CELL_DIM) parts.push("opacity:0.6");
  if (s.flags & CELL_ITALIC) parts.push("font-style:italic");
  const deco: string[] = [];
  if (s.flags & CELL_UNDERLINE) deco.push("underline");
  if (s.flags & CELL_STRIKE) deco.push("line-through");
  if (deco.length) parts.push(`text-decoration:${deco.join(" ")}`);
  if (s.flags & CELL_INVISIBLE) parts.push("visibility:hidden");
  if (s.flags & CELL_BLINK) parts.push("animation:cell-blink 1s step-end infinite");
  return parts.join(";");
}

/** Class on every clickable terminal link. Anchors painted here from
 *  core-authored OSC 8 cell data and anchors the linkifier wraps around regex /
 *  file-path matches (components/terminal-links.ts) share it, so one CSS rule
 *  and one arm / hover / click path serve both kinds. */
export const TERMINAL_LINK_CLASS = "wterm-link";

/** Marks an anchor as PAINTED from cell link data, and carries that link's run
 *  identity (CellSpan.linkKey). The linkifier reads it to tell painted anchors
 *  from its own, and to learn exactly which columns already carry a producer
 *  link — it never matches link text. Soft-wrapped halves of one link land in
 *  different rows, so in different anchors; this attribute re-identifies them. */
export const LINK_KEY_ATTR = "data-link-key";

/** The row's true GRID OCCUPANCY, stamped because nothing else can recover it
 *  from the painted DOM: textContent length counts UTF-16 code units, and a
 *  column is neither (a CJK ideograph is 2 columns / 1 unit, a ZWJ emoji
 *  cluster 2 columns / 11 units). Soft-wrap grouping asks "did this row FILL
 *  the grid?" — a column question. Answering it with code units silently drops
 *  wrapped links on CJK rows and fabricates joins after emoji rows. */
export const ROW_COLUMNS_ATTR = "data-grid-columns";

/** Set on a row that painted at least one anchor. The linkifier's prepass runs
 *  over every held row on a full scan, and virtually none of them carry a
 *  producer link — so this O(1) attribute read replaces a per-row subtree query
 *  for the anchors. Absent means "no painted link on this row", always. */
export const ROW_HAS_LINKS_ATTR = "data-row-links";

/** Producer-controlled URIs are painted verbatim into `href`, so the schemes a
 *  click could EXECUTE are refused: an application printing OSC 8 must not be
 *  able to run script in the pane. A refused URI loses its LINK and keeps its
 *  TEXT — the same trade MAX_LINK_URI_BYTES makes on the wire. Every other
 *  scheme stays allowed deliberately (ssh:, vscode:, file: … are real terminal
 *  link targets; a file: URI additionally loses to a resolvable path, see
 *  terminal-links.detect.ts). Leading whitespace and C0 controls are skipped
 *  because href parsing skips them too. */
const UNSAFE_LINK_SCHEME = /^[\s\u0000-\u001f]*(?:javascript|data|vbscript):/i;

function _linkAnchor(doc: Document, uri: string, key: string): HTMLElement {
  const a = doc.createElement("a");
  a.className = TERMINAL_LINK_CLASS;
  // setAttribute, not the href/target/rel properties: `getAttribute("href")`
  // must return the producer URI verbatim for the linkifier's overlap test.
  a.setAttribute("href", uri);
  a.setAttribute(LINK_KEY_ATTR, key);
  a.setAttribute("target", "_blank");
  a.setAttribute("rel", "noopener noreferrer");
  a.setAttribute("tabindex", "-1");
  a.setAttribute("data-hint", uri);
  return a;
}

/** A find match inside one row: `len` COLUMNS starting at grid column `col`.
 *  Columns are not character offsets — see CellSpan.columns; the worker's search
 *  RPC converts its text offsets with textRangeToColumns before sending. */
export interface FindHit { col: number; len: number }

/** Paint one span into `parent` and return the grid column after it.
 *  `hits` is undefined when the row carries no highlight at all. */
function _paintSpan(
  s: CellSpan,
  parent: HTMLElement,
  doc: Document,
  hits: readonly FindHit[] | undefined,
  activeCol: number | undefined,
  col: number,
): number {
  const style = spanStyle(s);
  // Split the run at every hit boundary inside it. A match becomes its own
  // span that keeps the run's DECORATION (bold/italic/underline) but hands
  // colour to the .cell-find-hit class — an inline colour would beat the class
  // and leave matches on styled output looking unhighlighted. Highlighting by
  // span split rather than an absolutely-positioned overlay keeps `.wterm`'s
  // overflow/scrollbar-gutter rules (load-bearing, docs/FAILURE-INDEX.md) untouched.
  //
  // An ATOMIC span is one cell (wide glyph, astral codepoint, cluster): it is
  // all-or-nothing, because slicing it by column would cut a surrogate pair or
  // strip a combining mark. Only a coalesced narrow run splits, and there
  // column offsets and code-unit offsets coincide by construction.
  if (hits === undefined || spanIsAtomic(s)) {
    const span = doc.createElement("span");
    const hit = hits?.find((h) => col < h.col + h.len && h.col < col + s.columns);
    if (hit) {
      span.className = hit.col === activeCol
        ? "cell-find-hit cell-find-hit-active"
        : "cell-find-hit";
    }
    const spanCss = hit ? spanDecorationStyle(s) : style;
    if (spanCss) span.setAttribute("style", spanCss);
    span.textContent = s.text;
    parent.appendChild(span);
    return col + s.columns;
  }
  const hitStyle = spanDecorationStyle(s);
  let at = 0;
  while (at < s.columns) {
    const abs = col + at;
    const hit = hits.find((h) => abs >= h.col && abs < h.col + h.len);
    let end: number;
    if (hit) {
      end = Math.min(s.columns, hit.col + hit.len - col);
    } else {
      end = s.columns;
      for (const h of hits) {
        const rel = h.col - col;
        if (rel > at && rel < end) end = rel;
      }
    }
    const span = doc.createElement("span");
    if (hit) {
      span.className = hit.col === activeCol
        ? "cell-find-hit cell-find-hit-active"
        : "cell-find-hit";
    }
    const spanCss = hit ? hitStyle : style;
    if (spanCss) span.setAttribute("style", spanCss);
    span.textContent = s.text.slice(at, end);
    parent.appendChild(span);
    at = end;
  }
  return col + s.columns;
}

export function renderRow(row: CellRow, doc: Document, hits?: readonly FindHit[], activeCol?: number): HTMLElement {
  const el = doc.createElement("div");
  el.className = "cell-row";
  el.setAttribute(ROW_COLUMNS_ATTR, String(rowColumns(row.spans)));
  if (row.spans.length === 0) {
    el.appendChild(doc.createTextNode(" ")); // keep blank rows tall
    return el;
  }
  const marked = hits !== undefined && hits.length > 0;
  let col = 0;
  // One anchor per maximal run of spans sharing a linkKey — the core's OSC 8
  // run identity, which is exactly what "the same logical link" means. Every
  // piece of the run is appended INSIDE that anchor, including the sub-spans a
  // find hit splits a span into, so a highlighted link stays one clickable
  // element and both halves of a split span keep their link.
  let anchor: HTMLElement | null = null;
  let anchorKey = "";
  for (const s of row.spans) {
    const uri = s.linkUri;
    if (uri === undefined || UNSAFE_LINK_SCHEME.test(uri)) {
      anchor = null;
      anchorKey = "";
    } else {
      // linkKey is present whenever linkUri is (assertCellRowSpans enforces it);
      // the URI is a total fallback so a malformed frame still paints one link.
      const key = s.linkKey ?? uri;
      if (anchor === null || key !== anchorKey) {
        anchor = _linkAnchor(doc, uri, key);
        el.setAttribute(ROW_HAS_LINKS_ATTR, "1");
        el.appendChild(anchor);
        anchorKey = key;
      }
    }
    col = _paintSpan(s, anchor ?? el, doc, marked ? hits : undefined, activeCol, col);
  }
  return el;
}

/** Visual identity of a row, as a 32-bit FNV-1a hash of every attribute
 *  renderRow paints: span structure, text codepoints, colors and flags. Rows
 *  with equal hashes paint identically, so the viewport diff skips them
 *  (renderViewport). Allocation-free on purpose — the string version built one
 *  string per viewport row per frame AND called spanStyle a second time for
 *  every row that changed. Span count, per-span text length and per-span column
 *  occupancy are folded in so a different span split, or the same text at a
 *  different width, can never collide.
 *  Find hits are folded in too, so a highlight change repaints exactly the rows
 *  whose highlighting actually changed — the diff needs no separate signal.
 *  LINK identity is folded in as the run KEY plus the URI's length, not the URI
 *  itself: linkKey is present exactly when linkUri is, and inside one grid epoch
 *  it maps 1:1 onto a URI (each distinct URI takes its own core hyperlink-table
 *  slot, and a core rebuild arrives as a NEW epoch, which repaints every row
 *  unconditionally). Hashing a 2 KB URI per span per row per frame would cost
 *  far more than the collision it rules out. */
export function rowHash(row: CellRow, hits?: readonly FindHit[], activeCol?: number): number {
  let h = Math.imul(2_166_136_261 ^ row.spans.length, 16_777_619);
  for (const sp of row.spans) {
    const t = sp.text;
    h = Math.imul(h ^ t.length, 16_777_619);
    h = Math.imul(h ^ sp.columns, 16_777_619);
    for (let i = 0; i < t.length; i++) h = Math.imul(h ^ t.charCodeAt(i), 16_777_619);
    h = Math.imul(h ^ sp.fg, 16_777_619);
    h = Math.imul(h ^ sp.bg, 16_777_619);
    h = Math.imul(h ^ sp.flags, 16_777_619);
    h = Math.imul(h ^ (sp.fgRgb ?? -1), 16_777_619);
    h = Math.imul(h ^ (sp.bgRgb ?? -1), 16_777_619);
    const key = sp.linkKey;
    h = Math.imul(h ^ (key === undefined ? 0 : key.length + 1), 16_777_619);
    if (key !== undefined) {
      for (let i = 0; i < key.length; i++) h = Math.imul(h ^ key.charCodeAt(i), 16_777_619);
      h = Math.imul(h ^ (sp.linkUri?.length ?? 0), 16_777_619);
    }
  }
  if (hits) {
    for (const hit of hits) {
      h = Math.imul(h ^ hit.col, 16_777_619);
      h = Math.imul(h ^ hit.len, 16_777_619);
    }
    h = Math.imul(h ^ (activeCol ?? -1), 16_777_619);
  }
  return h >>> 0;
}
