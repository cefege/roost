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
});
