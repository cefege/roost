// Holds imperative resources shared by one terminal pane's controller modules.
// CellTerminal creates exactly one runtime per mounted session; renderer, view,
// input, link, and prediction lifetimes are then explicit rather than global.
// Reactive state remains in the owning hooks and is never hidden in this object.

import type { CellGridRenderer } from "../lib/cellRenderer.ts";
import type { ScrollbackBackfill } from "../lib/scrollbackBackfill.ts";
import type { PredictiveEcho } from "../lib/predictiveEcho.ts";
import type { TerminalInputController } from "../lib/terminalInputController.ts";
import type { TerminalLinkAttachment } from "./terminal-links.ts";
import type { TerminalViewHandle } from "../store/terminal-stream.ts";

export interface CellTerminalRuntime {
  readonly sessionId: string;
  readonly display: () => HTMLDivElement | undefined;
  renderer: CellGridRenderer | null;
  linkAttachment: TerminalLinkAttachment | null;
  backfill: ScrollbackBackfill | null;
  predictor: PredictiveEcho | null;
  inputController: TerminalInputController | null;
  view: TerminalViewHandle | null;
  frameCursorKeysApplication: boolean;
  frameBracketedPaste: boolean;
  frameMouseSgr: boolean;
  frameFocusEvents: boolean;
  revealStartedAt: number;
  cellWidth: number;
  cellHeight: number;
  unmounted: boolean;
}

export function createCellTerminalRuntime(
  sessionId: string,
  display: () => HTMLDivElement | undefined,
): CellTerminalRuntime {
  return {
    sessionId,
    display,
    renderer: null,
    linkAttachment: null,
    backfill: null,
    predictor: null,
    inputController: null,
    view: null,
    frameCursorKeysApplication: false,
    frameBracketedPaste: false,
    frameMouseSgr: false,
    frameFocusEvents: false,
    revealStartedAt: 0,
    cellWidth: 0,
    cellHeight: 0,
    unmounted: false,
  };
}
