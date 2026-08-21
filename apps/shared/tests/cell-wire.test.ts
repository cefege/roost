// cell-phase-2 — cell emitter decision + proto round-trip (R11).
// nextCellFrame: full-on-first/reframe, delta-from-dirty otherwise, seq
// advance, scrollback append/shrink handling. cellFrameToProto/protoToCellFrame
// round-trip incl. optional rgb presence.

import { describe, test, expect } from "bun:test";
import { create } from "@bufbuild/protobuf";
import type { TerminalCore, CellData, CursorState } from "@wterm/core";
import {
  nextCellFrame, initCellEmitState, cloneCellGridFrame, SB_SNAPSHOT_HISTORY_ROWS,
  DEFAULT_COLOR, CELL_BOLD,
  type CellGridFrame,
} from "../src/cell/index.ts";
import {
  cellFrameToProto,
  deepCloneCellFrameFromProto,
  protoToCellFrame,
} from "../src/cell/cell-proto.ts";
import { PbCellGridFrameSchema } from "../src/gen/roost/v1/cell_pb.ts";

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
  /** Oldest-first, exactly as the frame exposes it. */
  sb: CellData[][] = [];
  /** Lines this ring has evicted — what 0.3.4 reports and the emitter reads. */
  discarded = 0;
  cursor: CursorState = { row: 0, col: 0, visible: true };
  alt = false;
  dirty = new Set<number>();
  /** Roll the ring: drop the oldest `count` lines and count them as gone. */
  evict(count: number) { this.sb.splice(0, count); this.discarded += count; }
  getCols() { return this.grid[0]?.length ?? 0; }
  getRows() { return this.grid.length; }
  getCell(r: number, c: number) { return this.grid[r][c]; }
  getCursor() { return this.cursor; }
  usingAltScreen() { return this.alt; }
  cursorApp = false;
  bracketed = false;
  cursorKeysApp() { return this.cursorApp; }
  bracketedPaste() { return this.bracketed; }
  getScrollbackCount() { return this.sb.length; }
  getScrollbackDiscardedCount() { return this.discarded; }
  getScrollbackCell(off: number, c: number) { return this.sb[this.sb.length - 1 - off][c] ?? cell(" "); }
  getScrollbackLineLen(off: number) { return this.sb[this.sb.length - 1 - off].length; }
  isDirtyRow(r: number) { return this.dirty.has(r); }
  clearDirty() { this.dirty.clear(); }
}
const asCore = (m: MockCore) => m as unknown as TerminalCore;
const STREAM_ID = "00000000-0000-4000-8000-000000000001";

describe("nextCellFrame", () => {
  test("first emit is a full frame; seq advances", () => {
    const core = new MockCore();
    core.grid = [row("hello", 5), row("world", 5)];
    let st = initCellEmitState("test-grid", STREAM_ID);
    const r = nextCellFrame(asCore(core), st, false);
    expect(r.frame.full).toBe(true);
    expect(r.frame.seq).toBe(1);
    expect(r.frame.baseSeq).toBe(0);
    expect(r.frame.streamId).toBe(STREAM_ID);
    expect(r.frame.gridEpoch).toBe("test-grid:0");
    expect(r.frame.viewportRows.length).toBe(2);
    expect(r.state.sentFull).toBe(true);
    expect(r.state.cols).toBe(5);
    // MockCore implements none of the OPTIONAL 0.3.4 mode accessors, exactly like
    // a core built against the older interface. The frame must still report "no
    // tracking" concretely: an undefined mouseTracking is nonzero to the
    // browser's `!== 0` gate, which would forward the mouse to an app that never
    // asked for it.
    expect(r.frame.mouseTracking).toBe(0);
    expect(r.frame.mouseSgr).toBe(false);
    expect(r.frame.focusEvents).toBe(false);
  });

  test("subsequent emit is a delta carrying only dirty rows", () => {
    const core = new MockCore();
    core.grid = [row("aaa", 5), row("bbb", 5)];
    const full = nextCellFrame(asCore(core), initCellEmitState("test-grid", STREAM_ID), false);
    core.clearDirty();
    core.grid = [row("aaa", 5), row("ZZZ", 5)];
    core.dirty = new Set([1]);
    const d = nextCellFrame(asCore(core), full.state, false);
    expect(d.frame.full).toBe(false);
    expect(d.frame.viewportRows.map((x) => x.index)).toEqual([1]);
    expect(d.frame.viewportRows[0].spans[0].text).toBe("ZZZ");
    expect(d.frame.seq).toBe(2);
    expect(d.frame.baseSeq).toBe(1);
    expect(d.frame.streamId).toBe(STREAM_ID);
  });

  test("a semantic dimension reframe advances the grid epoch", () => {
    const core = new MockCore();
    const full = nextCellFrame(asCore(core), initCellEmitState("test-grid", STREAM_ID), false);
    core.grid = [row("", 8), row("", 8)];
    const reframed = nextCellFrame(asCore(core), full.state, false);
    expect(reframed.frame.full).toBe(true);
    expect(reframed.frame.gridEpoch).toBe("test-grid:1");
  });

  test("scrollback append rides a delta; shrink forces a full", () => {
    const core = new MockCore();
    const full = nextCellFrame(asCore(core), initCellEmitState("test-grid", STREAM_ID), false);
    core.sb = [row("h1", 5), row("h2", 5)];
    const d = nextCellFrame(asCore(core), full.state, false);
    expect(d.frame.full).toBe(false);
    expect(d.frame.scrollbackAppend.map((x) => x.index)).toEqual([0, 1]);
    // shrink (reset/eviction) → full
    core.sb = [];
    const r = nextCellFrame(asCore(core), d.state, false);
    expect(r.frame.full).toBe(true);
  });

  test("a force-only claim snapshot keeps the grid epoch", () => {
    const core = new MockCore();
    const full = nextCellFrame(asCore(core), initCellEmitState("test-grid", STREAM_ID), false);
    const forced = nextCellFrame(asCore(core), full.state, true);
    expect(forced.frame.full).toBe(true);
    expect(forced.frame.seq).toBe(2);
    expect(forced.frame.gridEpoch).toBe(full.frame.gridEpoch);
  });

  test("authoritative full frames carry zero history but retain its depth", () => {
    const core = new MockCore();
    core.sb = [row("h1", 5), row("h2", 5), row("h3", 5)];
    const full = nextCellFrame(
      asCore(core),
      initCellEmitState("test-grid", STREAM_ID),
      true,
      SB_SNAPSHOT_HISTORY_ROWS,
    );
    expect(full.frame.full).toBe(true);
    expect(full.frame.scrollbackRows).toEqual([]);
    expect(full.frame.scrollbackTotal).toBe(3);
    expect(full.frame.sbBase).toBe(full.frame.scrollbackTotal);
    core.sb = [...core.sb, row("h4", 5)];
    const delta = nextCellFrame(asCore(core), full.state, false, SB_SNAPSHOT_HISTORY_ROWS);
    expect(delta.frame.scrollbackAppend.map((x) => x.index)).toEqual([3]);
  });

  test("a rolling ring keeps appending at monotonic indices, not retained ones", () => {
    // The re-alias bug the authoritative counter closes. At saturation
    // getScrollbackCount() PINS, so a delta computed from the count alone sees
    // no growth at all and every absolute index the client holds silently names
    // a different line. The origin has to come from the discard counter.
    const core = new MockCore();
    core.sb = [row("h0", 5), row("h1", 5), row("h2", 5), row("h3", 5), row("h4", 5)];
    const full = nextCellFrame(asCore(core), initCellEmitState("test-grid", STREAM_ID), false);
    expect(full.frame.scrollbackTotal).toBe(5);

    core.evict(2);
    core.sb.push(row("h5", 5), row("h6", 5));
    const d = nextCellFrame(asCore(core), full.state, false);
    // Retained is 5 either way; only the origin distinguishes them.
    expect(core.getScrollbackCount()).toBe(5);
    expect(d.frame.full).toBe(false);
    expect(d.state.sbDropped).toBe(2);
    expect(d.frame.scrollbackTotal).toBe(7);
    expect(d.frame.scrollbackAppend.map((x) => [x.index, x.spans[0]?.text]))
      .toEqual([[5, "h5"], [6, "h6"]]);
  });

  test("a ring that evicts past the client's history forces an honest reframe", () => {
    // A whole ring's worth of lines inside one coalesce window: the delta's
    // append can only start at the retained floor, so the rows between what the
    // client holds and that floor would never reach it and its history would
    // splice a hole. Those lines are gone from the ring either way.
    const core = new MockCore();
    core.sb = [row("h0", 5), row("h1", 5), row("h2", 5)];
    const full = nextCellFrame(asCore(core), initCellEmitState("test-grid", STREAM_ID), false);
    expect(full.state.lastSbTotal).toBe(3);

    core.evict(3);
    core.discarded += 5; // five more rolled through between emits
    core.sb.push(row("h8", 5), row("h9", 5));
    const r = nextCellFrame(asCore(core), full.state, false);
    expect(r.state.sbDropped).toBe(8);
    expect(r.frame.full).toBe(true);
    expect(r.frame.gridEpoch).toBe("test-grid:1");
    expect(r.frame.scrollbackTotal).toBe(10);
  });
});

describe("cell-proto round-trip", () => {
  function makeFrame(): CellGridFrame {
    return {
      streamId: STREAM_ID,
      gridEpoch: "test-grid:0",
      cols: 6, rows: 2, cursorRow: 1, cursorCol: 3, cursorVisible: false, altScreen: true,
      cursorKeysApp: true, bracketedPaste: false,
      // Non-default on purpose: a dropped field in any of the three conversions
      // would round-trip back as 0/false and fail the deep-equal below.
      mouseTracking: 1002, mouseSgr: true, focusEvents: true,
      full: true,
      viewportRows: [
        { index: 0, spans: [
          { text: "hi", columns: 2, fg: 1, bg: DEFAULT_COLOR, flags: CELL_BOLD, fgRgb: 0xff8800, bgRgb: undefined },
          { text: "中", columns: 2, fg: 1, bg: DEFAULT_COLOR, flags: 0, fgRgb: undefined, bgRgb: undefined },
        ] },
        { index: 1, spans: [] },
      ],
      scrollbackRows: [{ index: 0, spans: [{ text: "old", columns: 3, fg: DEFAULT_COLOR, bg: DEFAULT_COLOR, flags: 0, fgRgb: undefined, bgRgb: undefined }] }],
      scrollbackAppend: [],
      scrollbackTotal: 1,
      sbBase: 0,
      baseSeq: 0,
      seq: 42,
    };
  }

  test("toProto → deep clone preserves the frame; session_id stamped separately", () => {
    const f = makeFrame();
    const pb = cellFrameToProto(f, "sess-123");
    expect(pb.sessionId).toBe("sess-123");
    expect(deepCloneCellFrameFromProto(pb)).toEqual(f);
  });

  test("production conversion transfers decoded row and span arrays shallowly", () => {
    const pb = cellFrameToProto(makeFrame(), "s");
    const frame = protoToCellFrame(pb);
    expect(frame.viewportRows).toBe(pb.viewportRows);
    expect(frame.viewportRows[0]).toBe(pb.viewportRows[0]);
    expect(frame.viewportRows[0]!.spans).toBe(pb.viewportRows[0]!.spans);
    expect(frame.viewportRows[0]!.spans[0]).toBe(pb.viewportRows[0]!.spans[0]);

    const clone = deepCloneCellFrameFromProto(pb);
    expect(clone.viewportRows).not.toBe(pb.viewportRows);
    expect(clone.viewportRows[0]!.spans[0]).not.toBe(pb.viewportRows[0]!.spans[0]);
    const shallow = cloneCellGridFrame(frame);
    expect(shallow).not.toBe(frame);
    expect(shallow.viewportRows).not.toBe(frame.viewportRows);
    expect(shallow.viewportRows[0]).not.toBe(frame.viewportRows[0]);
    expect(shallow.viewportRows[0]!.spans).toBe(frame.viewportRows[0]!.spans);
  });


  test("nonzero sbBase and grid epoch survive the proto round-trip", () => {
    const f = makeFrame();
    f.sbBase = 10_000;
    f.scrollbackTotal = 10_000;
    f.scrollbackRows = [];
    const roundTrip = protoToCellFrame(cellFrameToProto(f, "s"));
    expect(roundTrip.sbBase).toBe(10_000);
    expect(roundTrip.gridEpoch).toBe("test-grid:0");
  });

  test("absent rgb stays absent through the wire (optional presence)", () => {
    const back = protoToCellFrame(cellFrameToProto(makeFrame(), "s"));
    expect(back.viewportRows[0].spans[1].fgRgb).toBeUndefined();
  });

  test("column occupancy rides the wire and is required on decode", () => {
    const pb = cellFrameToProto(makeFrame(), "s");
    expect(pb.viewportRows[0]!.spans.map((s) => [s.text, s.columns]))
      .toEqual([["hi", 2], ["中", 2]]);

    // A producer that ships no occupancy (proto3 default 0) is a bug that would
    // paint every wide glyph one column short — reject it at the boundary.
    const stale = cellFrameToProto(makeFrame(), "s");
    stale.viewportRows[0]!.spans[1]!.columns = 0;
    expect(() => protoToCellFrame(stale)).toThrow(/claims 0 columns/);

    // Occupancy wider than the grid means the row cannot be painted as sent.
    const overflow = cellFrameToProto(makeFrame(), "s");
    overflow.viewportRows[0]!.spans[0]!.columns = 40;
    expect(() => protoToCellFrame(overflow)).toThrow(/columns of a 6-column grid/);

    // A retained scrollback line keeps its write-time width, so it is NOT bound
    // by the current grid.
    const wideHistory = cellFrameToProto(makeFrame(), "s");
    wideHistory.scrollbackRows[0]!.spans[0]!.columns = 200;
    expect(() => protoToCellFrame(wideHistory)).not.toThrow();
  });

  test("rejects zero geometry and negative uint64 counters", () => {
    const zero = create(PbCellGridFrameSchema, {
      streamId: STREAM_ID,
      gridEpoch: "grid:0",
      cols: 0,
      rows: 0,
      full: true,
      seq: 1n,
    });
    expect(() => protoToCellFrame(zero)).toThrow(/terminal geometry/);

    const negative = cellFrameToProto(makeFrame(), "s");
    negative.scrollbackTotal = -1n;
    expect(() => protoToCellFrame(negative)).toThrow(/safe unsigned integer range/);
  });
});
