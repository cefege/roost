// Shared coordinator terminal-input control facade.
export { cancelTerminalControlGeneration, terminalViewerIdentity } from "./terminal-control-lane.ts";
export type {
  TerminalControlGeneration,
  TerminalViewerIdentity,
} from "./terminal-control-lane.ts";
export { nextCompatibilityInputSeq, processInputControl } from "./input-control.ts";
export type { InputControlCommand, InputControlResult } from "./input-control.ts";
