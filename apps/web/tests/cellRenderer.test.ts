// cell-phase-3 — CellGridRenderer pure style mapping (R11). DOM painting itself is
// verified live via /roost-smoke (no jsdom in this repo, by design — the
// project verifies rendered DOM through humanchrome). Here we lock the
// span→CSS + 256-palette mapping that the paint depends on.

import { describe, test, expect } from "bun:test";
import { spanStyle, ansi256ToCss, rowHash } from "../src/lib/cellRow.ts";
import { DEFAULT_COLOR, CELL_BOLD, CELL_DIM, CELL_ITALIC, CELL_UNDERLINE, CELL_REVERSE, CELL_INVISIBLE, CELL_STRIKE } from "@roost/shared/cell";
import type { CellSpan, CellRow } from "@roost/shared/cell";

function span(over: Partial<CellSpan>): CellSpan {
  const text = over.text ?? "x";
  return { fg: DEFAULT_COLOR, bg: DEFAULT_COLOR, flags: 0, fgRgb: undefined, bgRgb: undefined, ...over, text, columns: over.columns ?? text.length };
}

describe("ansi256ToCss", () => {
  test("0..15 map to themed --term-color vars", () => {
    expect(ansi256ToCss(0)).toBe("var(--term-color-0)");
    expect(ansi256ToCss(15)).toBe("var(--term-color-15)");
  });
  test("6x6x6 cube corners", () => {
    expect(ansi256ToCss(16)).toBe("rgb(0,0,0)");
    expect(ansi256ToCss(231)).toBe("rgb(255,255,255)");
    expect(ansi256ToCss(196)).toBe("rgb(255,0,0)"); // 196 = 16 + 5*36
  });
  test("grayscale ramp 232..255", () => {
    expect(ansi256ToCss(232)).toBe("rgb(8,8,8)");
    expect(ansi256ToCss(255)).toBe("rgb(238,238,238)");
  });
});

describe("spanStyle", () => {
  test("default fg/bg → fg var, no background emitted", () => {
    const s = spanStyle(span({}));
    expect(s).toBe("color:var(--term-fg)");
  });
  test("palette fg", () => {
    expect(spanStyle(span({ fg: 3 }))).toContain("color:var(--term-color-3)");
  });
  test("true-color fg/bg → hex", () => {
    const s = spanStyle(span({ fgRgb: 0xff8800, bg: 2, bgRgb: 0x102030 }));
    expect(s).toContain("color:#ff8800");
    expect(s).toContain("background:#102030");
  });
  test("reverse swaps fg/bg and always emits background", () => {
    const s = spanStyle(span({ fg: 1, bg: DEFAULT_COLOR, flags: CELL_REVERSE }));
    // reversed: text painted in bg(default→--term-bg), background in fg(color-1)
    expect(s).toContain("color:var(--term-bg)");
    expect(s).toContain("background:var(--term-color-1)");
  });
  test("flags → CSS", () => {
    expect(spanStyle(span({ flags: CELL_BOLD }))).toContain("font-weight:bold");
    expect(spanStyle(span({ flags: CELL_DIM }))).toContain("opacity:0.6");
    expect(spanStyle(span({ flags: CELL_ITALIC }))).toContain("font-style:italic");
    expect(spanStyle(span({ flags: CELL_INVISIBLE }))).toContain("visibility:hidden");
  });
  test("underline + strike coalesce into one text-decoration", () => {
    const s = spanStyle(span({ flags: CELL_UNDERLINE | CELL_STRIKE }));
    expect(s).toContain("text-decoration:underline line-through");
  });
});

describe("rowHash", () => {
  const r = (spans: CellSpan[]): CellRow => ({ index: 0, spans });
  test("identical rows → identical hash", () => {
    expect(rowHash(r([span({ text: "ab" })]))).toBe(rowHash(r([span({ text: "ab" })])));
  });
  test("text change → hash differs", () => {
    expect(rowHash(r([span({ text: "ab" })]))).not.toBe(rowHash(r([span({ text: "ac" })])));
  });
  test("style-only change → hash differs", () => {
    expect(rowHash(r([span({ text: "ab" })]))).not.toBe(rowHash(r([span({ text: "ab", flags: CELL_BOLD })])));
  });
  test("span boundaries don't collide", () => {
    expect(rowHash(r([span({ text: "ab" })]))).not.toBe(rowHash(r([span({ text: "a" }), span({ text: "b" })])));
  });
  test("a true-color change the palette fields cannot see still differs", () => {
    expect(rowHash(r([span({ text: "ab", fgRgb: 0x112233 })])))
      .not.toBe(rowHash(r([span({ text: "ab", fgRgb: 0x112234 })])));
    expect(rowHash(r([span({ text: "ab", bgRgb: 0x112233 })])))
      .not.toBe(rowHash(r([span({ text: "ab" })])));
  });
  test("a reversed row differs from the same row unreversed", () => {
    expect(rowHash(r([span({ text: "ab" })])))
      .not.toBe(rowHash(r([span({ text: "ab", flags: CELL_REVERSE })])));
  });
});
