// Cell-delta batch folding keeps one bounded browser patch exact.
// These fixtures exercise sequence validation, final coordinates, history order,
// and row-shell ownership without involving a terminal core or renderer.

import { describe, expect, test } from "bun:test";
import {
  DEFAULT_COLOR,
  cloneCellGridFrame,
  foldCellDeltaBatch,
  type CellDeltaBatch,
  type CellGridFrame,
  type CellRow,
} from "../src/cell/index.ts";

const STREAM_ID = "00000000-0000-4000-8000-000000000001";

function cellRow(index: number, text: string): CellRow {
  return {
    index,
    spans: text.length === 0 ? [] : [{
      text,
      columns: text.length,
      fg: DEFAULT_COLOR,
      bg: DEFAULT_COLOR,
      flags: 0,
      fgRgb: undefined,
      bgRgb: undefined,
    }],
  };
}

function fullFrame(text: readonly string[], seq = 1): CellGridFrame {
  return {
    streamId: STREAM_ID,
    gridEpoch: "batch-grid:0",
    cols: 20,
    rows: text.length,
    cursorRow: 0,
    cursorCol: 0,
    cursorVisible: true,
    altScreen: false,
    cursorKeysApp: false,
    bracketedPaste: false,
    mouseTracking: 0,
    mouseSgr: false,
    focusEvents: false,
    full: true,
    viewportRows: text.map((value, index) => cellRow(index, value)),
    scrollbackRows: [],
    scrollbackAppend: [],
    scrollbackTotal: 0,
    sbBase: 0,
    baseSeq: 0,
    seq,
  };
}

function nextDelta(
  base: CellGridFrame,
  viewportRows: CellRow[],
  scrollbackAppend: CellRow[] = [],
): CellGridFrame {
  return {
    ...base,
    full: false,
    viewportRows,
    scrollbackRows: [],
    scrollbackAppend,
    scrollbackTotal: base.scrollbackTotal + scrollbackAppend.length,
    sbBase: 0,
    baseSeq: base.seq,
    seq: base.seq + 1,
  };
}

function nextState(base: CellGridFrame, delta: CellGridFrame): CellGridFrame {
  return { ...base, seq: delta.seq, scrollbackTotal: delta.scrollbackTotal };
}

function rowText(rows: readonly CellRow[]): string[] {
  return rows.map((row) => row.spans.map((span) => span.text).join(""));
}

function folded(base: CellGridFrame, deltas: readonly CellGridFrame[]): CellDeltaBatch {
  const batch = foldCellDeltaBatch(base, deltas);
  expect(batch).not.toBeNull();
  return batch!;
}

describe("foldCellDeltaBatch", () => {
  test("folds one sparse delta into an owned canonical successor", () => {
    const base = fullFrame(["zero", "one", "two"]);
    const incoming = nextDelta(base, [cellRow(1, "ONE")]);

    const batch = folded(base, [incoming]);

    expect(batch.frame).not.toBe(base);
    expect(batch.frame).toMatchObject({ full: true, baseSeq: 0, seq: 2 });
    expect(rowText(batch.frame.viewportRows)).toEqual(["zero", "ONE", "two"]);
    expect(batch.dirtyRows.map((row) => row.index)).toEqual([1]);
    expect(rowText(batch.dirtyRows)).toEqual(["ONE"]);
    expect(batch.dirtyRows[0]).toBe(batch.frame.viewportRows[1]);
    expect(batch.frame.viewportRows[1]).not.toBe(incoming.viewportRows[0]);
    expect(batch.frame.viewportRows[1]!.spans).toBe(incoming.viewportRows[0]!.spans);
    expect(rowText(base.viewportRows)).toEqual(["zero", "one", "two"]);
  });

  test("combines sparse rows in final order and keeps the latest metadata", () => {
    const base = fullFrame(["a", "b", "c"]);
    const first = nextDelta(base, [cellRow(0, "A")]);
    const second = nextDelta(nextState(base, first), [cellRow(2, "C")]);
    second.cursorCol = 7;

    const batch = folded(base, [first, second]);

    expect(batch.frame).toMatchObject({ full: true, baseSeq: 0, seq: 3, cursorCol: 7 });
    expect(rowText(batch.frame.viewportRows)).toEqual(["A", "b", "C"]);
    expect(batch.dirtyRows.map((row) => row.index)).toEqual([0, 2]);
    expect(rowText(batch.dirtyRows)).toEqual(["A", "C"]);
    expect(batch.viewportShift).toBe(0);
    expect(batch.frame.scrollbackRows).toEqual([]);
    expect(batch.frame.scrollbackAppend).toEqual([]);
  });

  test("translates dirty rows across proven shifts and preserves history order", () => {
    const base = fullFrame(["a", "b", "c", "d"]);
    const first = nextDelta(
      base,
      [cellRow(1, "C!"), cellRow(3, "e")],
      [cellRow(0, "a")],
    );
    const second = nextDelta(
      nextState(base, first),
      [cellRow(2, "E!"), cellRow(3, "f")],
      [cellRow(1, "b")],
    );

    const batch = folded(base, [first, second]);

    expect(batch.viewportShift).toBe(2);
    expect(batch.dirtyRows.map((row) => row.index)).toEqual([0, 2, 3]);
    expect(rowText(batch.dirtyRows)).toEqual(["C!", "E!", "f"]);
    expect(rowText(batch.frame.viewportRows)).toEqual(["C!", "d", "E!", "f"]);
    expect(batch.frame.scrollbackTotal).toBe(2);
    expect(rowText(batch.frame.scrollbackRows)).toEqual(["a", "b"]);
    expect(batch.frame.scrollbackAppend).toEqual([]);
    expect(batch.scrollbackAppend.map((row) => row.index)).toEqual([0, 1]);
    expect(rowText(batch.scrollbackAppend)).toEqual(["a", "b"]);
  });

  test("marks the complete final viewport when shifts discard every original row", () => {
    const base = fullFrame(["a", "b"]);
    const first = nextDelta(base, [cellRow(1, "c")], [cellRow(0, "a")]);
    const second = nextDelta(nextState(base, first), [cellRow(1, "d")], [cellRow(1, "b")]);

    const batch = folded(base, [first, second]);

    expect(batch.viewportShift).toBe(2);
    expect(batch.dirtyRows.map((row) => row.index)).toEqual([0, 1]);
    expect(rowText(batch.dirtyRows)).toEqual(["c", "d"]);
    expect(rowText(batch.frame.viewportRows)).toEqual(["c", "d"]);
  });

  test("rejects empty, full, malformed, and noncontiguous chains without mutation", () => {
    const base = fullFrame(["a", "b", "c"]);
    const first = nextDelta(base, [cellRow(1, "B")]);
    const duplicate = nextDelta(nextState(base, first), [cellRow(0, "x"), cellRow(0, "y")]);
    const sequenceGap = { ...nextDelta(base, [cellRow(2, "late")]), baseSeq: 2, seq: 3 };
    const historyGap = nextDelta(base, [], [cellRow(1, "gap")]);
    const replacement = fullFrame(["x", "y", "z"], 2);
    const baseBefore = cloneCellGridFrame(base);
    const firstBefore = cloneCellGridFrame(first);
    const duplicateBefore = cloneCellGridFrame(duplicate);
    const sequenceGapBefore = cloneCellGridFrame(sequenceGap);
    const historyGapBefore = cloneCellGridFrame(historyGap);
    const replacementBefore = cloneCellGridFrame(replacement);
    const baseRows = base.viewportRows;
    const firstRows = first.viewportRows;
    const duplicateRows = duplicate.viewportRows;
    const sequenceGapRows = sequenceGap.viewportRows;
    const historyGapAppend = historyGap.scrollbackAppend;
    const replacementRows = replacement.viewportRows;

    expect(foldCellDeltaBatch(base, [])).toBeNull();
    expect(foldCellDeltaBatch(base, [replacement])).toBeNull();
    expect(foldCellDeltaBatch(base, [first, duplicate])).toBeNull();
    expect(foldCellDeltaBatch(base, [sequenceGap])).toBeNull();
    expect(foldCellDeltaBatch(base, [historyGap])).toBeNull();

    expect(base).toEqual(baseBefore);
    expect(first).toEqual(firstBefore);
    expect(duplicate).toEqual(duplicateBefore);
    expect(sequenceGap).toEqual(sequenceGapBefore);
    expect(historyGap).toEqual(historyGapBefore);
    expect(replacement).toEqual(replacementBefore);
    expect(base.viewportRows).toBe(baseRows);
    expect(first.viewportRows).toBe(firstRows);
    expect(duplicate.viewportRows).toBe(duplicateRows);
    expect(sequenceGap.viewportRows).toBe(sequenceGapRows);
    expect(historyGap.scrollbackAppend).toBe(historyGapAppend);
    expect(replacement.viewportRows).toBe(replacementRows);
  });

  test("owns base and delta row coordinates while sharing immutable spans", () => {
    const base = fullFrame(["a", "b", "c"]);
    const first = nextDelta(base, [cellRow(1, "B")]);
    const second = nextDelta(
      nextState(base, first),
      [cellRow(2, "d")],
      [cellRow(0, "a")],
    );
    const baseBefore = cloneCellGridFrame(base);

    const batch = folded(base, [first, second]);

    expect(rowText(batch.dirtyRows)).toEqual(["B", "d"]);
    expect(batch.dirtyRows.map((row) => row.index)).toEqual([0, 2]);
    expect(base).toEqual(baseBefore);
    expect(first.viewportRows[0]!.index).toBe(1);
    expect(batch.frame.viewportRows[0]).not.toBe(first.viewportRows[0]);
    expect(batch.frame.viewportRows[0]!.spans).toBe(first.viewportRows[0]!.spans);
    expect(batch.frame.viewportRows[1]).not.toBe(base.viewportRows[2]);
    expect(batch.frame.viewportRows[1]!.spans).toBe(base.viewportRows[2]!.spans);
    expect(batch.scrollbackAppend[0]).not.toBe(second.scrollbackAppend[0]);
    expect(batch.scrollbackAppend[0]!.spans).toBe(second.scrollbackAppend[0]!.spans);
  });
});
