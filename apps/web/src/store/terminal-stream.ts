// Browser terminal stream owner. One session replica survives renderer mount
// gaps; view handles own only socket-bound activity/geometry intent. Cohesive
// state, replica, view, chunk, and diagnostic concerns live in adjacent modules;
// this stable facade preserves every existing public import path.

export {
  dispatchTerminalCellChunk,
  dispatchTerminalCellFrame,
} from "./terminal-stream-replica.ts";
export {
  createTerminalView,
  dispatchTerminalViewState,
} from "./terminal-stream-view.ts";
export {
  _resetTerminalStreamForTest,
  cellFrameCount,
  cellFrameCountSize,
  cellFullFrameCount,
  cellGridEpoch,
  dropNextCellFrame,
  droppedCellFrameCount,
  lastFullFrameSbRows,
  pruneTerminalSession,
  terminalStreamDiagnosticSnapshot,
} from "./terminal-stream-diagnostics.ts";
export type {
  TerminalRendererDelivery,
  TerminalStreamDiagnosticSnapshot,
  TerminalViewHandle,
  TerminalViewHandleStatus,
} from "./terminal-stream-types.ts";
