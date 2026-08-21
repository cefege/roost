// @roost/shared/cell — cell-grid wire (R11). Worker fills frames
// via gridToCellFrame/gridDeltaFrame; SPA reconstructs via applyDelta.
// Import via "@roost/shared/cell".

export * from "./types.ts";
export {
  rowToSpans, gridToCellFrame, gridDeltaFrame, readScrollbackRangeCells,
  viewportRowSpans, scrollbackOffsetSpans,
} from "./grid-to-cells.ts";
export { applyDelta, cloneCellGridFrame, deltaViewportShift } from "./diff-grid.ts";
export * from "./frame-chunks.ts";
export {
  cellGridEpoch, initCellEmitState, nextCellFrame, scrollbackOrigin, type CellEmitState,
} from "./emitter.ts";
