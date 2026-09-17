// Cell-history interval tests keep coverage arithmetic independent of the DOM.
// The renderer owns row state; these cases cover only sorted absolute indexes.
// Gap paging depends on these half-open boundaries remaining exact.

import { describe, expect, test } from "bun:test";
import {
  cellHistoryInsertionIndex,
  hasCellHistoryRange,
  hasContiguousCellHistoryRows,
  hasSortedCellHistoryRows,
  missingCellHistoryRange,
  missingCellHistoryRangeAtScroll,
  missingCellHistoryRanges,
} from "../src/lib/cellHistoryRanges.ts";

describe("cell history ranges", () => {
  test("finds sorted head, interior, and tail gaps", () => {
    const rows = [2, 3, 6, 7].map((index) => ({ index }));
    expect(hasSortedCellHistoryRows(rows, 10)).toBe(true);
    expect(missingCellHistoryRange(rows, 10, 0)).toEqual({ start: 0, end: 2 });
    expect(missingCellHistoryRange(rows, 10, 4)).toEqual({ start: 4, end: 6 });
    expect(missingCellHistoryRange(rows, 10, 8)).toEqual({ start: 8, end: 10 });
    expect(missingCellHistoryRanges(rows, 10, 1, 9)).toEqual([
      { start: 1, end: 2 },
      { start: 4, end: 6 },
      { start: 8, end: 9 },
    ]);
  });

  test("requires exact nonempty coverage and contiguous insertions", () => {
    const rows = [2, 3, 6, 7].map((index) => ({ index }));
    expect(hasCellHistoryRange(rows, 10, 2, 4)).toBe(true);
    expect(hasCellHistoryRange(rows, 10, 2, 5)).toBe(false);
    expect(hasCellHistoryRange(rows, 10, 6, 8)).toBe(true);
    expect(hasCellHistoryRange(rows, 10, 10, 10)).toBe(false);
    expect(hasContiguousCellHistoryRows([{ index: 6 }, { index: 7 }], 6, 8)).toBe(true);
    expect(hasContiguousCellHistoryRows([{ index: 6 }, { index: 8 }], 6, 8)).toBe(false);
    expect(cellHistoryInsertionIndex(rows, 6)).toBe(2);
  });

  test("read-ahead widens the scroll window upward, nearest missing gap first", () => {
    const view = { scrollTop: 600, spacerTop: 0, clientHeight: 30, rowHeight: 10 };
    const painted = Array.from({ length: 50 }, (_, index) => ({ index: index + 50 }));
    // Viewport [60, 63) is painted: only read-ahead can see the gap above it.
    expect(missingCellHistoryRangeAtScroll(painted, 100, view)).toBeNull();
    expect(missingCellHistoryRangeAtScroll(painted, 100, { ...view, aheadRows: -20 })).toBeNull();
    expect(missingCellHistoryRangeAtScroll(painted, 100, { ...view, aheadRows: 20 }))
      .toEqual({ start: 0, end: 50, focusRow: 40, visibleEnd: 50 });

    // A gap reaching the viewport keeps visibleEnd inside the viewport, so the
    // page answering it still covers the rows the reader is staring at.
    const straddle = [
      ...Array.from({ length: 30 }, (_, index) => ({ index })),
      ...Array.from({ length: 40 }, (_, index) => ({ index: index + 60 })),
    ];
    const atGap = { ...view, scrollTop: 550 };
    expect(missingCellHistoryRangeAtScroll(straddle, 100, atGap))
      .toEqual({ start: 30, end: 60, focusRow: 55, visibleEnd: 58 });
    expect(missingCellHistoryRangeAtScroll(straddle, 100, { ...atGap, aheadRows: 20 }))
      .toEqual({ start: 30, end: 60, focusRow: 35, visibleEnd: 58 });

    // Disjoint gaps inside the widened window: the nearest one wins.
    const disjoint = [
      ...Array.from({ length: 10 }, (_, index) => ({ index })),
      ...Array.from({ length: 10 }, (_, index) => ({ index: index + 20 })),
      ...Array.from({ length: 60 }, (_, index) => ({ index: index + 40 })),
    ];
    expect(missingCellHistoryRangeAtScroll(disjoint, 100, { ...view, scrollTop: 500, aheadRows: 40 }))
      .toEqual({ start: 30, end: 40, focusRow: 30, visibleEnd: 40 });
  });
});
