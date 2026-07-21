// applyDelta(base, delta) → reconstructed FULL frame (R11). The producing
// side is gridDeltaFrame (emitter.ts, core.isDirtyRow path) — its reframe
// rule (cols/rows change, alt-screen toggle, scrollback shrink → full frame)
// is what makes the delta safely applicable here. Round-trip against the
// real encoder is covered by tests/cell-realcore.test.ts.

import type { CellRow, CellGridFrame } from "./types.ts";

/** Reconstruct a full frame by applying a delta onto the client's held
 *  frame. A full delta replaces wholesale. */
export function applyDelta(base: CellGridFrame, delta: CellGridFrame): CellGridFrame {
  if (delta.full) return delta;

  // Override changed viewport rows by index; keep the rest from base.
  const byIndex = new Map<number, CellRow>();
  for (const r of base.viewportRows) byIndex.set(r.index, r);
  for (const r of delta.viewportRows) byIndex.set(r.index, r);
  const viewportRows: CellRow[] = [];
  for (let row = 0; row < delta.rows; row++) {
    viewportRows.push(byIndex.get(row) ?? { index: row, spans: [] });
  }

  // Splice appended scrollback onto the held history.
  const scrollbackRows = delta.scrollbackAppend.length
    ? base.scrollbackRows.concat(delta.scrollbackAppend)
    : base.scrollbackRows;

  return {
    cols: delta.cols, rows: delta.rows,
    cursorRow: delta.cursorRow, cursorCol: delta.cursorCol, cursorVisible: delta.cursorVisible,
    altScreen: delta.altScreen, cursorKeysApp: delta.cursorKeysApp, bracketedPaste: delta.bracketedPaste,
    full: true,
    viewportRows,
    scrollbackRows,
    scrollbackAppend: [],
    scrollbackTotal: delta.scrollbackTotal,
    sbBase: base.sbBase, // deltas never move the held window's base
    seq: delta.seq,
  };
}
