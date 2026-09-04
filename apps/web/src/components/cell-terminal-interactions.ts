// Binds pane-local links, selection, focus, file drop, and mouse forwarding.
// These listeners share the mounted renderer and input controller but never own
// their lifetime. Global listeners exist only while the pane is visibly active,
// preserving terminal input and scroll behavior across warm parked sessions.

import {
  createEffect,
  onCleanup,
  type Accessor,
} from "solid-js";
import type { MouseTracking } from "@roost/shared/cell";
import { attachTerminalLinks } from "./terminal-links.ts";
import { attachTerminalMouseForwarding } from "../lib/terminalMouseForwarding.ts";
import { sendUserTerminalInput } from "../lib/userTerminalInput.ts";
import { copyOnSelect } from "../lib/copyOnSelectPref.ts";
import { isPageVisible, pageVisible } from "../lib/pageVisible.ts";
import { isTouchDevice } from "../lib/windowSizeClass.ts";
import { activeComposeSessionId } from "./TerminalComposeButton.tsx";
import { sessionTitle } from "../lib/sessionTitle.ts";
import type { CellTerminalProps } from "./cell-terminal-types.ts";
import type { CellTerminalRuntime } from "./cell-terminal-runtime.ts";
import type { CellTerminalInput } from "./cell-terminal-input.ts";
import type { CellTerminalPresentation } from "./cell-terminal-presentation.ts";
import type { CellTerminalViewport } from "./cell-terminal-viewport.ts";

interface CellTerminalInteractionSignals {
  mouseTracking: Accessor<MouseTracking>;
}

export interface CellTerminalInteractions {
  dispose(): void;
}

export function _terminalFocusAllowed(
  viewport: Pick<CellTerminalViewport, "viewActive">,
  focused: boolean,
): boolean {
  return viewport.viewActive() && focused && isPageVisible();
}

export function mountCellTerminalInteractions(
  props: CellTerminalProps,
  runtime: CellTerminalRuntime,
  input: CellTerminalInput,
  presentation: CellTerminalPresentation,
  viewport: CellTerminalViewport,
  navigate: (href: string) => void,
  signals: CellTerminalInteractionSignals,
): CellTerminalInteractions {
  const display = runtime.display();
  if (!display || !runtime.inputController) {
    throw new Error("terminal interactions mounted before input controller");
  }

  runtime.linkAttachment = attachTerminalLinks(display, {
    resolveFile: input.resolveFile,
    onOpenFile: navigate,
    githubOwnerRepo: () => props.session.git_remote ?? undefined,
    onArmedHoverChange: (active) => {
      presentation.notifyBackfill(runtime.renderer?.setArmedHold(active));
    },
  });

  const NAV_FALLTHROUGH_MS = 700;
  let lastActivatedAt = 0;
  let previouslyFocused = false;
  let gestureStartedOnDisplay = false;
  createEffect(() => {
    const focused = props.focused === true;
    if (focused && !previouslyFocused) {
      lastActivatedAt = Date.now();
      if (!isTouchDevice() && activeComposeSessionId() === null) {
        queueMicrotask(() => {
          if (
            _terminalFocusAllowed(viewport, props.focused === true)
            && activeComposeSessionId() === null
          ) runtime.inputController?.forceFocus();
        });
      }
    }
    previouslyFocused = focused;
  });
  const isNavFallthrough = (): boolean =>
    isTouchDevice() && Date.now() - lastActivatedAt < NAV_FALLTHROUGH_MS;
  const onDisplayDown = (event: MouseEvent): void => {
    if (!_terminalFocusAllowed(viewport, props.focused === true)) return;
    runtime.renderer?.finishLiveSelectionRelease();
    if (event.button !== 0) return;
    gestureStartedOnDisplay = true;
    if (isNavFallthrough()) return;
    const target = event.target as HTMLElement | null;
    if (target?.closest("button, input, textarea, a")) return;
    queueMicrotask(() => {
      if (_terminalFocusAllowed(viewport, props.focused === true)) {
        runtime.inputController?.forceFocus();
      }
    });
  };
  const onDisplayClick = (event: MouseEvent): void => {
    if (!_terminalFocusAllowed(viewport, props.focused === true)) return;
    const startedHere = gestureStartedOnDisplay;
    gestureStartedOnDisplay = false;
    if (event.button !== 0 || (!startedHere && isNavFallthrough())) return;
    const target = event.target as HTMLElement | null;
    if (target?.closest("button, input, textarea, a")) return;
    const selection = display.ownerDocument.getSelection();
    if (selection && !selection.isCollapsed) return;
    runtime.inputController?.forceFocus();
  };
  display.addEventListener("mousedown", onDisplayDown);
  display.addEventListener("click", onDisplayClick);

  const onSelectionChange = (): void => {
    presentation.syncNativeSelectionHold();
  };
  const onSelectionSettled = (): void => {
    if (!copyOnSelect()) return;
    const selection = display.ownerDocument.getSelection();
    if (!selection || selection.isCollapsed) return;
    if (!selection.anchorNode || !display.contains(selection.anchorNode)) return;
    void input.copySelectionToClipboard();
  };

  const mouseForwarding = attachTerminalMouseForwarding({
    display,
    mouseTracking: signals.mouseTracking,
    sendBytes: (bytes) =>
      sendUserTerminalInput(runtime.sessionId, bytes, runtime.view?.viewId),
    getRenderer: () => runtime.renderer,
    getMouseSgr: () => runtime.frameMouseSgr,
    getCellW: () => runtime.cellWidth,
    getCellH: () => runtime.cellHeight,
    measureCell: viewport.measureCell,
  });

  const dragHasFiles = (event: DragEvent): boolean =>
    event.dataTransfer?.types.includes("Files") ?? false;
  const onDragOver = (event: DragEvent): void => {
    if (!props.focused || !isPageVisible() || !dragHasFiles(event)) return;
    event.preventDefault();
  };
  const onDrop = (event: DragEvent): void => {
    if (!props.focused || !isPageVisible() || !dragHasFiles(event)) return;
    event.preventDefault();
    input.enqueueFileItems(event.dataTransfer?.items);
  };
  createEffect(() => {
    const mayOwnFocus = viewport.viewActive()
      && props.focused === true
      && pageVisible();
    if (!mayOwnFocus) {
      input.setCtrlArmed(false);
      const controller = runtime.inputController;
      if (controller?.ownsTarget(display.ownerDocument.activeElement)) {
        controller.textarea.blur();
      }
      return;
    }
    if (!isTouchDevice() && activeComposeSessionId() === null) {
      runtime.inputController?.forceFocus();
    }
  });

  let disposed = false;
  let globalListenersAttached = false;
  const detachGlobalListeners = (): void => {
    if (!globalListenersAttached) return;
    globalListenersAttached = false;
    document.removeEventListener("selectionchange", onSelectionChange);
    window.removeEventListener("pointerup", onSelectionSettled);
    window.removeEventListener("keyup", onSelectionSettled);
    window.removeEventListener("mousemove", mouseForwarding.onWindowMouseMove);
    window.removeEventListener("mouseup", mouseForwarding.onWindowMouseUp);
    document.removeEventListener("dragenter", onDragOver);
    document.removeEventListener("dragover", onDragOver);
    document.removeEventListener("drop", onDrop);
  };
  createEffect(() => {
    if (disposed || !viewport.viewActive() || !pageVisible()) return;
    globalListenersAttached = true;
    document.addEventListener("selectionchange", onSelectionChange);
    window.addEventListener("pointerup", onSelectionSettled);
    window.addEventListener("keyup", onSelectionSettled);
    window.addEventListener("mousemove", mouseForwarding.onWindowMouseMove);
    window.addEventListener("mouseup", mouseForwarding.onWindowMouseUp);
    document.addEventListener("dragenter", onDragOver);
    document.addEventListener("dragover", onDragOver);
    document.addEventListener("drop", onDrop);
    onCleanup(detachGlobalListeners);
  });
  mouseForwarding.bindWheelAndTouchMove();
  createEffect(() => {
    const title = sessionTitle(props.session);
    runtime.renderer?.setAccessibleLabel(`Terminal — ${title}`);
    runtime.inputController?.setAccessibleLabel(`Terminal input — ${title}`);
  });

  const dispose = (): void => {
    if (disposed) return;
    disposed = true;
    detachGlobalListeners();
    runtime.linkAttachment?.dispose();
    runtime.linkAttachment = null;
    display.removeEventListener("mousedown", onDisplayDown);
    display.removeEventListener("click", onDisplayClick);
    mouseForwarding.dispose();
  };
  return { dispose };
}
