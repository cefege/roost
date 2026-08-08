// @roost/shared/cell — cell-grid wire (R11). Worker fills frames
// via gridToCellFrame/gridDeltaFrame; SPA reconstructs via applyDelta.
// Import via "@roost/shared/cell".

export * from "./types.ts";
export { rowToSpans, gridToCellFrame, readScrollbackRangeCells } from "./grid-to-cells.ts";
export { applyDelta } from "./diff-grid.ts";
export { cellGridEpoch, initCellEmitState, nextCellFrame, type CellEmitState } from "./emitter.ts";
