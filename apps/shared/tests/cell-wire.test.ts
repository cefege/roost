// cell-phase-2 — cell emitter decision + proto round-trip (R11).
// nextCellFrame: full-on-first/reframe, delta-from-dirty otherwise, seq
// advance, scrollback append/shrink handling. cellFrameToProto/protoToCellFrame
// round-trip incl. optional rgb presence.

import { describe, test, expect } from "bun:test";
import type { TerminalCore, CellData, CursorState } from "@wterm/core";
import {
  nextCellFrame, initCellEmitState,
  DEFAULT_COLOR, CELL_BOLD,
  type CellGridFrame,
} from "../src/cell/index.ts";
import { cellFrameToProto, protoToCellFrame } from "../src/cell/cell-proto.ts";

// Configurable mock — set grid/scrollback/dirty between calls to drive the
// emitter decision without a real VT engine.
function cell(ch: string, fg = DEFAULT_COLOR, bg = DEFAULT_COLOR, flags = 0): CellData {
  return { char: ch ? ch.codePointAt(0)! : 0, fg, bg, flags, fgRgb: undefined, bgRgb: undefined };
}
function row(s: string, cols: number): CellData[] {
  return Array.from({ length: cols }, (_, i) => cell(s[i] ?? " "));
}
class MockCore {
  grid: CellData[][] = [row("", 5), row("", 5)];
  sb: CellData[][] = [];
  cursor: CursorState = { row: 0, col: 0, visible: true };
  alt = false;
  dirty = new Set<number>();
  getCols() { return this.grid[0]?.length ?? 0; }
  getRows() { return this.grid.length; }
  getCell(r: number, c: number) { return this.grid[r][c]; }
  getCursor() { return this.cursor; }
  usingAltScreen() { return this.alt; }
  getScrollbackCount() { return this.sb.length; }
  getScrollbackCell(off: number, c: number) { return this.sb[this.sb.length - 1 - off][c] ?? cell(" "); }
  getScrollbackLineLen(off: number) { return this.sb[this.sb.length - 1 - off].length; }
  isDirtyRow(r: number) { return this.dirty.has(r); }
  clearDirty() { this.dirty.clear(); }
}
const asCore = (m: MockCore) => m as unknown as TerminalCore;

describe("nextCellFrame", () => {
  test("first emit is a full frame; seq advances", () => {
    const core = new MockCore();
    core.grid = [row("hello", 5), row("world", 5)];
    let st = initCellEmitState();
    const r = nextCellFrame(asCore(core), st, false);
    expect(r.frame.full).toBe(true);
    expect(r.frame.seq).toBe(1);
    expect(r.frame.viewportRows.length).toBe(2);
    expect(r.state.sentFull).toBe(true);
    expect(r.state.cols).toBe(5);
  });

  test("subsequent emit is a delta carrying only dirty rows", () => {
    const core = new MockCore();
    core.grid = [row("aaa", 5), row("bbb", 5)];
    const full = nextCellFrame(asCore(core), initCellEmitState(), false);
    core.clearDirty();
    core.grid = [row("aaa", 5), row("ZZZ", 5)];
    core.dirty = new Set([1]);
    const d = nextCellFrame(asCore(core), full.state, false);
    expect(d.frame.full).toBe(false);
    expect(d.frame.viewportRows.map((x) => x.index)).toEqual([1]);
    expect(d.frame.viewportRows[0].spans[0].text).toBe("ZZZ");
    expect(d.frame.seq).toBe(2);
  });

  test("dimension change forces a full reframe", () => {
    const core = new MockCore();
    const full = nextCellFrame(asCore(core), initCellEmitState(), false);
    core.grid = [row("", 8), row("", 8)]; // cols 5 → 8
    const r = nextCellFrame(asCore(core), full.state, false);
    expect(r.frame.full).toBe(true);
  });

  test("scrollback append rides a delta; shrink forces a full", () => {
    const core = new MockCore();
    const full = nextCellFrame(asCore(core), initCellEmitState(), false);
    core.sb = [row("h1", 5), row("h2", 5)];
    const d = nextCellFrame(asCore(core), full.state, false);
    expect(d.frame.full).toBe(false);
    expect(d.frame.scrollbackAppend.map((x) => x.index)).toEqual([0, 1]);
    // shrink (reset/eviction) → full
    core.sb = [];
    const r = nextCellFrame(asCore(core), d.state, false);
    expect(r.frame.full).toBe(true);
  });

  test("force=true reframes even when nothing changed", () => {
    const core = new MockCore();
    const full = nextCellFrame(asCore(core), initCellEmitState(), false);
    const forced = nextCellFrame(asCore(core), full.state, true);
    expect(forced.frame.full).toBe(true);
    expect(forced.frame.seq).toBe(2);
  });

  test("tailRows caps a full frame's scrollback; deltas ignore it", () => {
    const core = new MockCore();
    core.sb = [row("h1", 5), row("h2", 5), row("h3", 5)];
    const full = nextCellFrame(asCore(core), initCellEmitState(), true, 2);
    expect(full.frame.full).toBe(true);
    expect(full.frame.sbBase).toBe(1);
    expect(full.frame.scrollbackTotal).toBe(3);
    expect(full.frame.scrollbackRows.map((x) => x.index)).toEqual([1, 2]);
    // delta after the tail frame appends from the REAL total, base untouched
    core.sb = [...core.sb, row("h4", 5)];
    const d = nextCellFrame(asCore(core), full.state, false, 2);
    expect(d.frame.full).toBe(false);
    expect(d.frame.scrollbackAppend.map((x) => x.index)).toEqual([3]);
    expect(d.frame.sbBase).toBe(0);
  });
});

describe("cell-proto round-trip", () => {
  function makeFrame(): CellGridFrame {
    return {
      cols: 6, rows: 2, cursorRow: 1, cursorCol: 3, cursorVisible: false, altScreen: true,
      full: true,
      viewportRows: [
        { index: 0, spans: [{ text: "hi", fg: 1, bg: DEFAULT_COLOR, flags: CELL_BOLD, fgRgb: 0xff8800, bgRgb: undefined }] },
        { index: 1, spans: [] },
      ],
      scrollbackRows: [{ index: 0, spans: [{ text: "old", fg: DEFAULT_COLOR, bg: DEFAULT_COLOR, flags: 0, fgRgb: undefined, bgRgb: undefined }] }],
      scrollbackAppend: [],
      scrollbackTotal: 1,
      sbBase: 0,
      seq: 42,
    };
  }

  test("toProto → fromProto preserves the frame; session_id stamped separately", () => {
    const f = makeFrame();
    const pb = cellFrameToProto(f, "sess-123");
    expect(pb.sessionId).toBe("sess-123");
    expect(protoToCellFrame(pb)).toEqual(f);
  });

  test("nonzero sbBase survives the proto round-trip (tail frame)", () => {
    const f = makeFrame();
    f.sbBase = 9750;
    f.scrollbackTotal = 10_000;
    expect(protoToCellFrame(cellFrameToProto(f, "s")).sbBase).toBe(9750);
  });

  test("absent rgb stays absent through the wire (optional presence)", () => {
    const f = makeFrame();
    f.viewportRows[0].spans[0].fgRgb = undefined;
    const back = protoToCellFrame(cellFrameToProto(f, "s"));
    expect(back.viewportRows[0].spans[0].fgRgb).toBeUndefined();
  });
});
