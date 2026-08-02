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

/** Everything in spanStyle EXCEPT colour. A find-match span uses this so the
 *  .cell-find-hit class actually owns its background and text colour: an inline
 *  `color:`/`background:` beats any class rule, so emitting the run's own colours
 *  on a highlighted span would leave matches on styled output un-highlighted. */
export function spanDecorationStyle(s: CellSpan): string {
  const parts: string[] = [];
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

/** A find match inside one row: `len` columns starting at column `col` of the
 *  row's plain text. Columns and character offsets coincide (one char per cell). */
export interface FindHit { col: number; len: number }

export function renderRow(row: CellRow, doc: Document, hits?: readonly FindHit[], activeCol?: number): HTMLElement {
  const el = doc.createElement("div");
  el.className = "cell-row";
  if (row.spans.length === 0) {
    el.appendChild(doc.createTextNode(" ")); // keep blank rows tall
    return el;
  }
  const marked = hits !== undefined && hits.length > 0;
  let col = 0;
  for (const s of row.spans) {
    const style = spanStyle(s);
    if (!marked) {
      const span = doc.createElement("span");
      span.setAttribute("style", style);
      span.textContent = s.text;
      el.appendChild(span);
      col += s.text.length;
      continue;
    }
    // Split the run at every hit boundary inside it. A match becomes its own
    // span that keeps the run's DECORATION (bold/italic/underline) but hands
    // colour to the .cell-find-hit class — an inline colour would beat the class
    // and leave matches on styled output looking unhighlighted. Highlighting by
    // span split rather than an absolutely-positioned overlay keeps `.wterm`'s
    // overflow/scrollbar-gutter rules (load-bearing, CLAUDE.md L11) untouched.
    const hitStyle = spanDecorationStyle(s);
    let at = 0;
    while (at < s.text.length) {
      const abs = col + at;
      const hit = hits.find((h) => abs >= h.col && abs < h.col + h.len);
      let end: number;
      if (hit) {
        end = Math.min(s.text.length, hit.col + hit.len - col);
      } else {
        end = s.text.length;
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
      el.appendChild(span);
      at = end;
    }
    col += s.text.length;
  }
  return el;
}

/** Concatenated text of a row's spans. */
export function rowText(row: CellRow): string {
  return row.spans.map((s) => s.text).join("");
}

/** Visual identity of a row, as a 32-bit FNV-1a hash of every attribute
 *  renderRow paints: span structure, text codepoints, colors and flags. Rows
 *  with equal hashes paint identically, so the viewport diff skips them
 *  (renderViewport). Allocation-free on purpose — the string version built one
 *  string per viewport row per frame AND called spanStyle a second time for
 *  every row that changed. Span count and per-span text length are folded in so
 *  a different span split can never collide with the same concatenated text.
 *  Find hits are folded in too, so a highlight change repaints exactly the rows
 *  whose highlighting actually changed — the diff needs no separate signal. */
export function rowHash(row: CellRow, hits?: readonly FindHit[], activeCol?: number): number {
  let h = Math.imul(2_166_136_261 ^ row.spans.length, 16_777_619);
  for (const sp of row.spans) {
    const t = sp.text;
    h = Math.imul(h ^ t.length, 16_777_619);
    for (let i = 0; i < t.length; i++) h = Math.imul(h ^ t.charCodeAt(i), 16_777_619);
    h = Math.imul(h ^ sp.fg, 16_777_619);
    h = Math.imul(h ^ sp.bg, 16_777_619);
    h = Math.imul(h ^ sp.flags, 16_777_619);
    h = Math.imul(h ^ (sp.fgRgb ?? -1), 16_777_619);
    h = Math.imul(h ^ (sp.bgRgb ?? -1), 16_777_619);
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
