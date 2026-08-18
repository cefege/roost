// Terminal input/viewport control, shared by the unary compatibility RPCs and
// Sync v2. This is the stable public facade — the implementation lives in four
// siblings:
//   - terminal-control-lane.ts:  viewer identity, the per-viewer/session lane +
//                                generation cancel registry, session routing
//   - viewport-control.ts:       processViewportControl
//   - input-control.ts:          processInputControl + the input audit queue
//   - barrier-repair-replay.ts:  requestBarrierRepairFullFrame
// External callers import from THIS file; the exports below re-expose the
// siblings so no import path changes.
//
// This file owns no session lifecycle despite its name: sessions are opened and
// closed by handlers-sessions.ts and handler-session-spawn.ts.

export { cancelTerminalControlGeneration, terminalViewerIdentity } from "./terminal-control-lane.ts";
export type { TerminalControlGeneration, TerminalViewerIdentity } from "./terminal-control-lane.ts";
export { processViewportControl } from "./viewport-control.ts";
export type { ViewportControlCommand, ViewportControlResult } from "./viewport-control.ts";
export { nextCompatibilityInputSeq, processInputControl } from "./input-control.ts";
export type { InputControlCommand, InputControlResult } from "./input-control.ts";
export { requestBarrierRepairFullFrame } from "./barrier-repair-replay.ts";
export type { BarrierRepairReplay, BarrierRepairRoute } from "./barrier-repair-replay.ts";
