// @roost/shared/cell — cell-grid wire (R11). Worker fills frames
// via gridToCellFrame/gridDeltaFrame; SPA reconstructs via applyDelta.
// Import via "@roost/shared/cell".

export * from "./types.ts";
export {
  rowToSpans, gridToCellFrame, gridDeltaFrame, readScrollbackRangeCells,
  viewportRowSpans, scrollbackOffsetSpans,
} from "./grid-to-cells.ts";
export { registerWtermRowReader, wtermRowReader } from "./wterm-row-reader.ts";
export type { WtermBorrowedRow, WtermRowReader } from "./wterm-row-reader.ts";
export {
  applyDelta, cloneCellGridFrame, deltaViewportShift, normalizeCellGridFrame,
} from "./diff-grid.ts";
export { foldCellDeltaBatch } from "./delta-batch.ts";
export type { CellDeltaBatch } from "./delta-batch.ts";
export * from "./frame-chunks.ts";
export {
  cellGridEpoch, initCellEmitState, LIVE_DELTA_SCROLLBACK_ROWS_CAP,
  nextCellFrame, scrollbackOrigin, type CellEmitState,
} from "./emitter.ts";
