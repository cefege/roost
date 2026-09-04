// Binds viewport, page, resize, and document-focus lifecycle for one terminal.
// It converts visibility transitions into explicit active or inactive view intent
// and keeps reserved copy, paste, and find chords ahead of PTY key encoding.
// Renderer resources remain owned by the mounting controller.

import {
  createEffect,
  createMemo,
  on,
  type Accessor,
} from "solid-js";
import { diag } from "@roost/shared/diag";
import { termFontSize } from "../lib/terminalFontPref.ts";
import { arrangeEpoch, isResizeDragging } from "../lib/resizeDrag.ts";
import { isPageVisible } from "../lib/pageVisible.ts";
import { FOCUS_OWNERS } from "../lib/focusOwners.ts";
import { isAltGraphKey } from "../lib/terminalInput.ts";
import { isTouchDevice } from "../lib/windowSizeClass.ts";
import type { CellTerminalProps } from "./cell-terminal-types.ts";
import type { CellTerminalRuntime } from "./cell-terminal-runtime.ts";
import type { CellTerminalInput } from "./cell-terminal-input.ts";
import type { CellTerminalPresentation } from "./cell-terminal-presentation.ts";
import type { CellTerminalViewport } from "./cell-terminal-viewport.ts";

export interface CellTerminalLifecycle {
  dispose(): void;
}

export function mountCellTerminalLifecycle(
  props: CellTerminalProps,
  runtime: CellTerminalRuntime,
  input: CellTerminalInput,
  presentation: CellTerminalPresentation,
  viewport: CellTerminalViewport,
  pending: Accessor<boolean>,
): CellTerminalLifecycle {
  const display = runtime.display();
  if (!display) throw new Error("terminal lifecycle mounted without display");

  createEffect(on(viewport.viewActive, (active) => {
    presentation.refreshCursorBlink();
    if (!active) {
      viewport.parkView();
      return;
    }
    runtime.revealStartedAt = performance.now();
    viewport.publishViewportNow();
  }));
  createEffect(() => {
    if (pending() || !viewport.viewActive()) return;
    viewport.scheduleViewport();
  });

  let lastZoom = termFontSize();
  createEffect(() => {
    const zoom = termFontSize();
    if (zoom === lastZoom) return;
    lastZoom = zoom;
    runtime.cellWidth = 0;
    runtime.cellHeight = 0;
    runtime.renderer?.invalidateRowHeight();
    viewport.scheduleViewport();
  });
  const resizeObserver = new ResizeObserver(() => {
    presentation.notifyBackfill(runtime.renderer?.noteBoxResize());
    if (!isResizeDragging()) viewport.scheduleViewport();
  });
  resizeObserver.observe(display);

  let wasResizeDragging = false;
  createEffect(() => {
    const dragging = isResizeDragging();
    if (dragging) {
      viewport.cancelScheduled();
    } else if (wasResizeDragging && viewport.viewActive() && isPageVisible()) {
      viewport.publishViewport();
    }
    wasResizeDragging = dragging;
  });
  const spotlit = createMemo(() => !!props.spotlit);
  let wasSpotlit = false;
  createEffect(() => {
    const current = spotlit();
    if (current === wasSpotlit) return;
    wasSpotlit = current;
    requestAnimationFrame(() => {
      if (viewport.viewActive() && isPageVisible()) viewport.scheduleViewport();
    });
  });
  createEffect(on(arrangeEpoch, () => {
    requestAnimationFrame(() => {
      if (viewport.viewActive() && isPageVisible()) viewport.scheduleViewport();
    });
  }, { defer: true }));

  const onVisibility = (): void => {
    if (!viewport.viewActive() || !isPageVisible()) {
      viewport.parkView();
      return;
    }
    presentation.refreshCursorBlink();
    presentation.refreshTerminalPresentation();
    viewport.publishViewportNow();
  };
  const onWindowResize = (): void => {
    if (viewport.viewActive() && isPageVisible()) viewport.scheduleViewport();
  };
  const onPageHide = (): void => {
    presentation.clearFrameActivity();
    presentation.clearCursorBlink();
    presentation.releasePaintHolds();
    viewport.publishInactive();
  };
  const onPageShow = (): void => {
    presentation.refreshCursorBlink();
    presentation.refreshTerminalPresentation();
    if (isPageVisible() && viewport.viewActive()) viewport.publishViewportNow();
  };
  document.addEventListener("visibilitychange", onVisibility);
  window.addEventListener("resize", onWindowResize);
  window.addEventListener("pagehide", onPageHide);
  window.addEventListener("pageshow", onPageShow);

  const onDocumentMouseDown = (event: MouseEvent): void => {
    if (pending() || !viewport.viewActive() || !props.focused || !isPageVisible()) return;
    if (event.button !== 0) return;
    const target = event.target as HTMLElement | null;
    if (target?.closest(FOCUS_OWNERS) || target?.closest("[data-pane]")) return;
    event.preventDefault();
  };
  const onDocumentKeyDown = (event: KeyboardEvent): void => {
    if (event.defaultPrevented) return;
    if (!event.isTrusted && isTouchDevice()) return;
    if (pending() || !viewport.viewActive() || !props.focused || !isPageVisible()) return;

    if ((event.metaKey || event.ctrlKey) && event.shiftKey && !event.altKey) {
      const key = event.key.toLowerCase();
      if (key === "c") {
        event.preventDefault();
        event.stopPropagation();
        void input.copySelectionToClipboard();
        return;
      }
      if (key === "v") {
        event.preventDefault();
        event.stopPropagation();
        void input.pasteFromClipboard();
        return;
      }
    }
    if (
      !event.altKey
      && event.key.toLowerCase() === "f"
      && (
        (event.metaKey && !event.ctrlKey && !event.shiftKey)
        || (event.ctrlKey && event.shiftKey)
      )
    ) {
      event.preventDefault();
      event.stopPropagation();
      input.find.openFind();
      return;
    }
    const activeElement = document.activeElement as HTMLElement | null;
    if (runtime.inputController?.ownsTarget(activeElement)) return;
    if (
      activeElement === document.body
      || activeElement === document.documentElement
    ) {
      const altGraph = isAltGraphKey(event);
      if (event.metaKey || (event.altKey && !altGraph) || event.isComposing) return;
      if (event.key === "Control" || event.key === "Shift") return;
      runtime.inputController?.forceFocus();
      if (runtime.inputController?.dispatchKeydown(event.key, {
        code: event.code,
        ctrlKey: event.ctrlKey,
        shiftKey: event.shiftKey,
        altKey: event.altKey,
        metaKey: event.metaKey,
        altGraph,
      })) {
        event.preventDefault();
        event.stopPropagation();
      }
      return;
    }
    if (event.metaKey || event.ctrlKey || event.altKey) return;
    if (activeElement?.closest(FOCUS_OWNERS)) return;
    if (event.key.length !== 1 || event.key === " ") return;
    diag("focus.recover", {
      sid: runtime.sessionId,
      via: "keydown",
      key: "char",
    });
    runtime.inputController?.forceFocus();
  };
  document.addEventListener("mousedown", onDocumentMouseDown, true);
  document.addEventListener("keydown", onDocumentKeyDown, true);

  const dispose = (): void => {
    document.removeEventListener("visibilitychange", onVisibility);
    window.removeEventListener("resize", onWindowResize);
    window.removeEventListener("pagehide", onPageHide);
    window.removeEventListener("pageshow", onPageShow);
    document.removeEventListener("keydown", onDocumentKeyDown, true);
    document.removeEventListener("mousedown", onDocumentMouseDown, true);
    resizeObserver.disconnect();
  };
  return { dispose };
}
