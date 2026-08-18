// @roost/shared/cell — cell-grid wire (R11). Worker fills frames
// via gridToCellFrame/gridDeltaFrame; SPA reconstructs via applyDelta.
// Import via "@roost/shared/cell".

export * from "./types.ts";
export {
  rowToSpans, gridToCellFrame, readScrollbackRangeCells,
  viewportRowSpans, scrollbackOffsetSpans,
} from "./grid-to-cells.ts";
export { applyDelta, deltaViewportShift } from "./diff-grid.ts";
export {
  cellGridEpoch, initCellEmitState, nextCellFrame, scrollbackOrigin, type CellEmitState,
} from "./emitter.ts";
