// cell-phase-1 — cell-grid core unit tests (R11).
// Covers: rowToSpans RLE + right-trim, gridToCellFrame reading a mock
// TerminalCore (viewport + oldest-first scrollback + cursor + alt + tail),
// readScrollbackRangeCells. applyDelta round-trip vs the real encoder
// (gridDeltaFrame) lives in cell-realcore.test.ts.

import { describe, test, expect } from "bun:test";
import type { TerminalCore, CellData, CursorState } from "@wterm/core";
import {
  rowToSpans, gridToCellFrame, readScrollbackRangeCells,
  DEFAULT_COLOR, CELL_BOLD,
  spanIsAtomic, rowColumns, spansText, textOffsetToColumn, textRangeToColumns, columnText,
} from "../src/cell/index.ts";

// ── mock core ──────────────────────────────────────────────────────
function cell(ch: string, fg = DEFAULT_COLOR, bg = DEFAULT_COLOR, flags = 0, fgRgb?: number): CellData {
  return { char: ch ? ch.codePointAt(0)! : 0, fg, bg, flags, fgRgb, bgRgb: undefined };
}
/** A double-width glyph as 0.3.4 stores it: width-2 LEAD + width-0 CONTINUATION. */
function wide(ch: string, fg = DEFAULT_COLOR, bg = DEFAULT_COLOR): CellData[] {
  return [
    { char: ch.codePointAt(0)!, fg, bg, flags: 0, width: 2, fgRgb: undefined, bgRgb: undefined },
    { char: 0, fg, bg, flags: 0, width: 0, fgRgb: undefined, bgRgb: undefined },
  ];
}
function rowOf(s: string, cols: number): CellData[] {
  const out: CellData[] = [];
  for (let i = 0; i < cols; i++) out.push(cell(s[i] ?? " "));
  return out;
}

// Mock scrollback is stored OLDEST-FIRST; the core API is NEWEST-FIRST by
// offset, so we convert here exactly as a real core does.
class MockCore {
  constructor(
    public grid: CellData[][],      // viewport rows × cols
    public sb: CellData[][] = [],   // scrollback, oldest-first
    public cursor: CursorState = { row: 0, col: 0, visible: true },
    public alt = false,
  ) {}
  getCols() { return this.grid[0]?.length ?? 0; }
  getRows() { return this.grid.length; }
  getCell(row: number, col: number) { return this.grid[row][col]; }
  getCursor() { return this.cursor; }
  usingAltScreen() { return this.alt; }
  cursorApp = false;
  bracketed = false;
  cursorKeysApp() { return this.cursorApp; }
  bracketedPaste() { return this.bracketed; }
  getScrollbackCount() { return this.sb.length; }
  getScrollbackCell(offset: number, col: number) {
    const line = this.sb[this.sb.length - 1 - offset];
    return line[col] ?? cell(" ");
  }
  getScrollbackLineLen(offset: number) { return this.sb[this.sb.length - 1 - offset].length; }
}
const asCore = (m: MockCore) => m as unknown as TerminalCore;

// ── rowToSpans ─────────────────────────────────────────────────────
describe("rowToSpans", () => {
  test("groups consecutive equal-style cells into one span", () => {
    const cells = [cell("h"), cell("i"), cell("!", 1)];
    const spans = rowToSpans(cells);
    expect(spans).toEqual([
      { text: "hi", columns: 2, fg: DEFAULT_COLOR, bg: DEFAULT_COLOR, flags: 0, fgRgb: undefined, bgRgb: undefined },
      { text: "!", columns: 1, fg: 1, bg: DEFAULT_COLOR, flags: 0, fgRgb: undefined, bgRgb: undefined },
    ]);
  });

  test("right-trims trailing default-style blanks → empty row is []", () => {
    expect(rowToSpans(rowOf("", 10))).toEqual([]);
    const spans = rowToSpans(rowOf("ab", 10));
    expect(spans).toEqual([{ text: "ab", columns: 2, fg: DEFAULT_COLOR, bg: DEFAULT_COLOR, flags: 0, fgRgb: undefined, bgRgb: undefined }]);
  });

  test("a styled trailing space is NOT trimmed (only default-style blanks)", () => {
    const cells = [cell("x"), cell(" ", DEFAULT_COLOR, 2)]; // bg=2 space
    const spans = rowToSpans(cells);
    expect(spans.length).toBe(2);
    expect(spans[1]).toMatchObject({ text: " ", bg: 2 });
  });

  test("flags + rgb participate in span boundaries", () => {
    const cells = [cell("a", 1, DEFAULT_COLOR, CELL_BOLD), cell("b", 1, DEFAULT_COLOR, CELL_BOLD), cell("c", 1, DEFAULT_COLOR, CELL_BOLD, 0xff0000)];
    const spans = rowToSpans(cells);
    expect(spans.map((s) => s.text)).toEqual(["ab", "c"]);
    expect(spans[1].fgRgb).toBe(0xff0000);
  });

  test("NUL cell renders as a space within a span", () => {
    const cells = [cell("a"), { char: 0, fg: DEFAULT_COLOR, bg: DEFAULT_COLOR, flags: 0 }, cell("b")];
    expect(rowToSpans(cells)).toEqual([{ text: "a b", columns: 3, fg: DEFAULT_COLOR, bg: DEFAULT_COLOR, flags: 0, fgRgb: undefined, bgRgb: undefined }]);
  });

  test("a wide glyph is one atomic 2-column span; its continuation emits nothing", () => {
    // "中文a" as the core stores it: lead+continuation, lead+continuation, narrow.
    const cells = [...wide("中"), ...wide("文"), cell("a"), ...rowOf("", 5)];
    const spans = rowToSpans(cells);
    expect(spans.map((s) => [s.text, s.columns])).toEqual([["中", 2], ["文", 2], ["a", 1]]);
    // The row paints "中文" — never "中 文" — across exactly 5 columns.
    expect(spansText(spans)).toBe("中文a");
    expect(rowColumns(spans)).toBe(5);
    expect(spans.map(spanIsAtomic)).toEqual([true, true, false]);
  });

  test("a wide lead with no continuation cell claims exactly its one column", () => {
    // Truncated row edge (a stored scrollback line that ends on the lead). The
    // row's occupancy must equal the cells read, so the lead claims 1, not 2.
    const spans = rowToSpans([cell("a"), {
      char: "中".codePointAt(0)!, fg: DEFAULT_COLOR, bg: DEFAULT_COLOR, flags: 0, width: 2,
    }]);
    expect(spans.map((s) => [s.text, s.columns])).toEqual([["a", 1], ["中", 1]]);
    expect(rowColumns(spans)).toBe(2);
  });

  test("a wide glyph's continuation survives the right-trim that eats padding", () => {
    // A default-style width-0 cell looks blank; trimming it would shrink the
    // lead to one column and un-align every column after it.
    const spans = rowToSpans([...wide("中"), ...rowOf("", 6)]);
    expect(spans).toEqual([
      { text: "中", columns: 2, fg: DEFAULT_COLOR, bg: DEFAULT_COLOR, flags: 0, fgRgb: undefined, bgRgb: undefined },
    ]);
  });

  test("an orphan continuation paints one blank column", () => {
    // No wide lead before it (a lead overwritten narrow, a clipped row edge).
    const spans = rowToSpans([
      cell("a"),
      { char: 0, fg: DEFAULT_COLOR, bg: 3, flags: 0, width: 0 },
      cell("b"),
    ]);
    expect(spans.map((s) => [s.text, s.columns])).toEqual([["a", 1], [" ", 1], ["b", 1]]);
    expect(rowColumns(spans)).toBe(3);
  });

  test("a wide glyph overwritten by a narrow char is one narrow column", () => {
    // What 0.3.4 leaves behind: both cells become plain width-1 (verified
    // against the real core in cell-wide-occupancy.test.ts).
    const spans = rowToSpans([cell("A"), cell(" "), ...wide("文")]);
    expect(spans.map((s) => [s.text, s.columns])).toEqual([["A ", 2], ["文", 2]]);
    expect(columnText(spans, 0)).toBe("A");
    expect(columnText(spans, 2)).toBe("文");
    expect(columnText(spans, 3)).toBe("文"); // continuation column paints the lead
  });

  test("astral codepoints and clusters stay atomic, never coalesced", () => {
    const octopus: CellData = {
      char: 0x1f419, fg: DEFAULT_COLOR, bg: DEFAULT_COLOR, flags: 0, width: 2,
    };
    const flag: CellData = { // regional indicator: astral but ONE column
      char: 0x1f1fa, fg: DEFAULT_COLOR, bg: DEFAULT_COLOR, flags: 0, width: 1,
    };
    const cluster: CellData = {
      char: 0x1f44b, chars: "👋🏽", fg: DEFAULT_COLOR, bg: DEFAULT_COLOR, flags: 0, width: 2,
    };
    const spans = rowToSpans([
      octopus, { char: 0, fg: DEFAULT_COLOR, bg: DEFAULT_COLOR, flags: 0, width: 0 },
      flag,
      cluster, { char: 0, fg: DEFAULT_COLOR, bg: DEFAULT_COLOR, flags: 0, width: 0 },
    ]);
    expect(spans.map((s) => [s.text, s.columns])).toEqual([["🐙", 2], ["🇺", 1], ["👋🏽", 2]]);
    // 🐙 has as many code units as columns — atomicity must not be inferred
    // from that identity, or a hit boundary would slice the surrogate pair.
    expect(spans.map(spanIsAtomic)).toEqual([true, true, true]);
    expect(rowColumns(spans)).toBe(5);
  });
});

// ── column geometry ────────────────────────────────────────────────
describe("span column geometry", () => {
  // "ab中文cd": columns 0,1 narrow, 2-3 中, 4-5 文, 6,7 narrow.
  const spans = rowToSpans([cell("a"), cell("b"), ...wide("中"), ...wide("文"), cell("c"), cell("d")]);

  test("text offsets map to the columns the paint used", () => {
    expect(spansText(spans)).toBe("ab中文cd");
    expect(rowColumns(spans)).toBe(8);
    expect([0, 1, 2, 3, 4, 5].map((at) => textOffsetToColumn(spans, at)))
      .toEqual([0, 1, 2, 4, 6, 7]);
    expect(textOffsetToColumn(spans, 6)).toBe(8); // past the end → row width
  });

  test("a match covers whole glyphs, never half of one", () => {
    // "中文" is offsets 2..4 → columns 2..6 (four columns, not two).
    expect(textRangeToColumns(spans, 2, 2)).toEqual({ col: 2, columns: 4 });
    // "b中" → column 1 through the end of 中.
    expect(textRangeToColumns(spans, 1, 2)).toEqual({ col: 1, columns: 3 });
    // A narrow-only match keeps 1:1 columns.
    expect(textRangeToColumns(spans, 4, 2)).toEqual({ col: 6, columns: 2 });
    expect(textRangeToColumns(spans, 0, 0)).toEqual({ col: 0, columns: 0 });
  });

  test("column lookups read the glyph occupying that column", () => {
    expect([0, 1, 2, 3, 4, 5, 6, 7, 8].map((col) => columnText(spans, col)))
      .toEqual(["a", "b", "中", "中", "文", "文", "c", "d", ""]);
  });
});

// ── gridToCellFrame ────────────────────────────────────────────────
describe("gridToCellFrame", () => {
  test("reads viewport, scrollback (oldest-first), cursor, alt", () => {
    const core = new MockCore(
      [rowOf("live1", 5), rowOf("live2", 5)],
      [rowOf("old", 5), rowOf("mid", 5), rowOf("new", 5)], // oldest-first
      { row: 1, col: 3, visible: false },
      true,
    );
    const f = gridToCellFrame(asCore(core), 7, "test-grid:0", "00000000-0000-4000-8000-000000000001");
    expect(f.full).toBe(true);
    expect(f.gridEpoch).toBe("test-grid:0");
    expect(f.cols).toBe(5);
    expect(f.rows).toBe(2);
    expect(f.altScreen).toBe(true);
    expect(f.cursorRow).toBe(1);
    expect(f.cursorCol).toBe(3);
    expect(f.cursorVisible).toBe(false);
    expect(f.seq).toBe(7);
    expect(f.viewportRows.map((r) => r.spans[0]?.text)).toEqual(["live1", "live2"]);
    expect(f.scrollbackTotal).toBe(3);
    // oldest → newest with absolute indices 0,1,2
    expect(f.scrollbackRows.map((r) => [r.index, r.spans[0]?.text])).toEqual([[0, "old"], [1, "mid"], [2, "new"]]);
    expect(f.sbBase).toBe(0);
  });

  test("tailRows caps scrollback to the newest N lines with sbBase set", () => {
    const core = new MockCore([rowOf("v", 3)], [rowOf("old", 3), rowOf("mid", 3), rowOf("new", 3)]);
    const f = gridToCellFrame(asCore(core), 1, "test-grid:0", "00000000-0000-4000-8000-000000000001", 2);
    expect(f.sbBase).toBe(1);
    expect(f.scrollbackTotal).toBe(3);
    expect(f.scrollbackRows.map((r) => [r.index, r.spans[0]?.text])).toEqual([[1, "mid"], [2, "new"]]);
    // tail deeper than history → complete frame, sbBase 0
    const g = gridToCellFrame(asCore(core), 2, "test-grid:0", "00000000-0000-4000-8000-000000000001", 250);
    expect(g.sbBase).toBe(0);
    expect(g.scrollbackRows.length).toBe(3);

    const viewportOnly = gridToCellFrame(asCore(core), 3, "test-grid:0", "00000000-0000-4000-8000-000000000001", 0);
    expect(viewportOnly.scrollbackRows).toEqual([]);
    expect(viewportOnly.sbBase).toBe(viewportOnly.scrollbackTotal);
  });

  test("readScrollbackRangeCells serves [start, end) clamped to the grid", () => {
    const core = new MockCore([rowOf("v", 1)], [rowOf("a", 1), rowOf("b", 1), rowOf("c", 1)]);
    const mid = readScrollbackRangeCells(asCore(core), 1, 3);
    expect(mid.map((r) => [r.index, r.spans[0]?.text])).toEqual([[1, "b"], [2, "c"]]);
    // end beyond total clamps; inverted/empty ranges are []
    expect(readScrollbackRangeCells(asCore(core), 2, 99).map((r) => r.index)).toEqual([2]);
    expect(readScrollbackRangeCells(asCore(core), 3, 3)).toEqual([]);
    expect(readScrollbackRangeCells(asCore(core), 2, 1)).toEqual([]);
    expect(readScrollbackRangeCells(asCore(core), -5, 1).map((r) => r.index)).toEqual([0]);
  });
});

