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
} from "../src/cell/index.ts";

// ── mock core ──────────────────────────────────────────────────────
function cell(ch: string, fg = DEFAULT_COLOR, bg = DEFAULT_COLOR, flags = 0, fgRgb?: number): CellData {
  return { char: ch ? ch.codePointAt(0)! : 0, fg, bg, flags, fgRgb, bgRgb: undefined };
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
      { text: "hi", fg: DEFAULT_COLOR, bg: DEFAULT_COLOR, flags: 0, fgRgb: undefined, bgRgb: undefined },
      { text: "!", fg: 1, bg: DEFAULT_COLOR, flags: 0, fgRgb: undefined, bgRgb: undefined },
    ]);
  });

  test("right-trims trailing default-style blanks → empty row is []", () => {
    expect(rowToSpans(rowOf("", 10))).toEqual([]);
    const spans = rowToSpans(rowOf("ab", 10));
    expect(spans).toEqual([{ text: "ab", fg: DEFAULT_COLOR, bg: DEFAULT_COLOR, flags: 0, fgRgb: undefined, bgRgb: undefined }]);
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
    expect(rowToSpans(cells)).toEqual([{ text: "a b", fg: DEFAULT_COLOR, bg: DEFAULT_COLOR, flags: 0, fgRgb: undefined, bgRgb: undefined }]);
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
    const f = gridToCellFrame(asCore(core), 7);
    expect(f.full).toBe(true);
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
    const f = gridToCellFrame(asCore(core), 1, 2);
    expect(f.sbBase).toBe(1);
    expect(f.scrollbackTotal).toBe(3);
    expect(f.scrollbackRows.map((r) => [r.index, r.spans[0]?.text])).toEqual([[1, "mid"], [2, "new"]]);
    // tail deeper than history → complete frame, sbBase 0
    const g = gridToCellFrame(asCore(core), 2, 250);
    expect(g.sbBase).toBe(0);
    expect(g.scrollbackRows.length).toBe(3);
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

