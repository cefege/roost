// Binds pane-local links, selection, focus, file drop, and mouse forwarding.
// These listeners share the mounted renderer and input controller but never own
// their lifetime. Global listeners exist only while the pane is visibly active,
// preserving terminal input and scroll behavior across warm parked sessions.

import {
  createEffect,
  createMemo,
  onCleanup,
  untrack,
  type Accessor,
} from "solid-js";
import type { MouseTracking } from "@roost/protocol/cell";
import { attachTerminalLinks } from "../../renderer/terminal-links.ts";
import { attachTerminalMouseForwarding } from "../../renderer/terminalMouseForwarding.ts";
import { sendUserTerminalInput } from "../../lib/userTerminalInput.ts";
import { copyOnSelect } from "../../store/prefs/copyOnSelectPref.ts";
import { isPageVisible, pageVisible } from "../../browser/pageVisible.ts";
import { isTouchDevice } from "../../browser/windowSizeClass.ts";
import { directionalInputActive } from "../../lib/directionalInput.ts";
import { activeComposeSessionId } from "./TerminalComposeButton.tsx";
import { sessionTitle } from "../../lib/sessionTitle.ts";
import type { CellTerminalProps } from "./cell-terminal-types.ts";
import type { CellTerminalRuntime } from "./cell-terminal-runtime.ts";
import type { CellTerminalInput } from "./cell-terminal-input.ts";
import type { CellTerminalPresentation } from "./cell-terminal-presentation.ts";
import type { CellTerminalViewport } from "./cell-terminal-viewport.ts";

interface CellTerminalInteractionSignals {
  mouseTracking: Accessor<MouseTracking>;
  linkActivationArmed: Accessor<boolean>;
}

export interface CellTerminalInteractions {
  dispose(): void;
}

export function _terminalFocusAllowed(
  viewport: Pick<CellTerminalViewport, "viewActive">,
  focused: boolean,
  inputReady: boolean,
): boolean {
  return inputReady && viewport.viewActive() && focused && isPageVisible();
}

export function mountCellTerminalInteractions(
  props: CellTerminalProps,
  runtime: CellTerminalRuntime,
  input: CellTerminalInput,
  presentation: CellTerminalPresentation,
  viewport: CellTerminalViewport,
  pending: Accessor<boolean>,
  navigate: (href: string) => void,
  signals: CellTerminalInteractionSignals,
): CellTerminalInteractions {
  const display = runtime.display();
  if (!display || !runtime.inputController) {
    throw new Error("terminal interactions mounted before input controller");
  }

  const foregroundWorkActive = createMemo(
    () => viewport.viewActive() && pageVisible(),
  );
  runtime.linkAttachment = attachTerminalLinks(display, {
    resolveFile: input.resolveFile,
    onOpenFile: navigate,
    githubOwnerRepo: () => props.session.git_remote ?? undefined,
    initialActive: foregroundWorkActive(),
    linkActivationArmed: signals.linkActivationArmed,
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
      if (!pending() && !isTouchDevice() && activeComposeSessionId() === null) {
        queueMicrotask(() => {
          if (
            _terminalFocusAllowed(viewport, props.focused === true, !pending())
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
    if (!_terminalFocusAllowed(viewport, props.focused === true, !pending())) return;
    runtime.renderer?.finishLiveSelectionRelease();
    if (event.button !== 0) return;
    gestureStartedOnDisplay = true;
    if (isNavFallthrough()) return;
    const target = event.target as HTMLElement | null;
    if (target?.closest("button, input, textarea, a")) return;
    queueMicrotask(() => {
      if (_terminalFocusAllowed(viewport, props.focused === true, !pending())) {
        runtime.inputController?.forceFocus();
      }
    });
  };
  const onDisplayClick = (event: MouseEvent): void => {
    if (!_terminalFocusAllowed(viewport, props.focused === true, !pending())) return;
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
    linkActivationArmed: signals.linkActivationArmed,
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
  // The selected optimistic placeholder exists before coord can accept its
  // session ID. It must not advertise a keyboard owner until spawn confirms.
  createEffect(() => {
    const mayOwnFocus = !pending()
      && viewport.viewActive()
      && props.focused === true
      && pageVisible();
    if (!mayOwnFocus) {
      input.setCtrlArmed(false);
      input.setLinkActivationArmed(false);
      runtime.linkAttachment?.releaseInteraction();
      const controller = runtime.inputController;
      if (controller?.ownsTarget(display.ownerDocument.activeElement)) {
        controller.textarea.blur();
      }
      return;
    }
    // TV mode leaves focus where the remote put it: the off-screen PTY textarea
    // would otherwise own it for the pane's whole life, so the D-pad could never
    // reach the .wterm scroll box or the on-screen key pad.
    if (!isTouchDevice() && !directionalInputActive() && activeComposeSessionId() === null) {
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
    // The window mouseup removed above is the ONLY clearer of an in-flight
    // forwarded press, so the debt is settled on this transition: otherwise the
    // application keeps a button it never sees released and the next mousemove
    // after re-attach reports a drag the PTY never saw begin.
    mouseForwarding.completeHeldDrag();
  };
  createEffect(() => {
    const foregroundWorkEnabled = !disposed && foregroundWorkActive();
    runtime.linkAttachment?.setActive(foregroundWorkEnabled);
    if (!foregroundWorkEnabled) {
      detachGlobalListeners();
      return;
    }
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
    // RENDERER_HOLD_SELECTION is armed EDGE-ONLY, by `selectionchange`, and
    // these listeners are absent for the whole of a withdraw. The hold must
    // therefore be re-derived from the LEVEL — the live document selection —
    // when they come back, or a selection dropped inside that gap pins a hold
    // no selection justifies and the pane never paints again. Untracked: the
    // presentation refresh it runs reads signals this gate must not follow.
    untrack(() => presentation.syncNativeSelectionHold());
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
