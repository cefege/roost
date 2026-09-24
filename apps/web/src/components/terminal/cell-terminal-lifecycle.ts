// Binds viewport, resize, and document-focus lifecycle for one terminal.
// The document coordinator fans shared page transitions to this mounted pane.
// It converts visibility transitions into explicit active or inactive view intent,
// classifies every withdraw as a real hide or a transient layout gap,
// and keeps reserved copy, paste, and find chords ahead of PTY key encoding.
// Renderer resources remain owned by the mounting controller.

import {
  createEffect,
  createMemo,
  on,
  type Accessor,
} from "solid-js";
import { diag } from "@roost/observability/diag";
import { termFontSize } from "../../store/prefs/terminalFontPref.ts";
import { arrangeEpoch, isResizeDragging } from "../../lib/resizeDrag.ts";
import { isPageVisible } from "../../browser/pageVisible.ts";
import { FOCUS_OWNERS } from "../../lib/focusOwners.ts";
import { isAltGraphKey } from "../../client/input/terminalInput.ts";
import { isTouchDevice } from "../../browser/windowSizeClass.ts";
import { directionalInputActive } from "../../lib/directionalInput.ts";
import { registerCellTerminalDocumentLifecycle } from "./cell-terminal-document-lifecycle.ts";
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

  let lifecycleDisposed = false;
  // A face that settles behind a hidden, inactive, or pending pane still
  // changes what one cell measures. Invalidating only while publishable leaves
  // the fallback advance cached for that pane's whole life, and the canonical
  // cols × 1ch sheet then paints past the clipped content box: the last column
  // is unreachable until something else happens to invalidate the cache.
  const onTerminalFontsSettled = (): void => {
    if (lifecycleDisposed || runtime.unmounted) return;
    runtime.cellWidth = 0;
    runtime.cellHeight = 0;
    runtime.renderer?.invalidateRowHeight();
    if (viewport.shouldPublishActive()) viewport.publishViewportNow();
  };
  const fonts = document.fonts;
  void fonts?.ready?.then(onTerminalFontsSettled, () => undefined);
  // `ready` answers for the loading epoch in flight at mount. A face that
  // starts loading afterwards settles only through these events, and a failed
  // download still means re-measuring whatever face actually paints.
  fonts?.addEventListener("loadingdone", onTerminalFontsSettled);
  fonts?.addEventListener("loadingerror", onTerminalFontsSettled);

  // viewActive() = inLayout && surfaceVisible && surfaceActive (CellTerminal).
  // Every REAL cause of a withdraw is readable right here: an overlay route or
  // another pane's spotlight scrim drops surfaceVisible/surfaceActive, a hidden
  // page drops isPageVisible, teardown sets its own flags. What is left is
  // `inLayout` alone, and its transient form is a deck box that measures zero:
  // terminal-deck-model's view() returns zero panes for a 0-sized deck, which
  // removes every pane from layout for one ResizeObserver tick with nothing
  // hidden. The deck element is the same stable anchor that model measures, so
  // reading it at the transition tells the jitter from a genuine leave.
  const deckBoxCollapsed = (): boolean => {
    const deck = document.querySelector(
      '[data-testid="terminal-deck"]',
    ) as HTMLElement | null;
    if (!deck) return false;
    return deck.clientWidth === 0 || deck.clientHeight === 0;
  };
  const withdrawIsTransientLayoutGap = (): boolean =>
    !lifecycleDisposed
    && !runtime.unmounted
    && props.surfaceVisible
    && props.surfaceActive
    && isPageVisible()
    && deckBoxCollapsed();
  const withdrawView = (): void => {
    if (withdrawIsTransientLayoutGap()) viewport.parkViewAfterLayoutGap();
    else viewport.parkView();
  };

  createEffect(on(viewport.viewActive, (active) => {
    presentation.refreshCursorBlink();
    if (!active) {
      withdrawView();
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

  const unregisterDocumentLifecycle = registerCellTerminalDocumentLifecycle((event) => {
    if (event === "hidden") {
      viewport.parkView();
      return;
    }
    if (event === "pagehide") {
      presentation.clearFrameActivity();
      presentation.clearCursorBlink();
      presentation.releasePaintHolds();
      viewport.publishInactive();
      return;
    }
    if (!isPageVisible() || !viewport.viewActive()) {
      if (event === "visible") withdrawView();
      return;
    }
    presentation.refreshCursorBlink();
    presentation.refreshTerminalPresentation();
    viewport.publishViewportNow();
    runtime.view?.refresh();
  });
  const onWindowResize = (): void => {
    if (viewport.viewActive() && isPageVisible()) viewport.scheduleViewport();
  };
  window.addEventListener("resize", onWindowResize);

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
    // A remote or a game controller has no physical keyboard: focus recovery
    // would pull its focus into the off-screen textarea and leave the D-pad
    // with nothing to drive. Raw keys reach the PTY from the on-screen key pad.
    if (directionalInputActive()) return;
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
    if (lifecycleDisposed) return;
    lifecycleDisposed = true;
    fonts?.removeEventListener("loadingdone", onTerminalFontsSettled);
    fonts?.removeEventListener("loadingerror", onTerminalFontsSettled);
    viewport.parkView();
    unregisterDocumentLifecycle();
    window.removeEventListener("resize", onWindowResize);
    document.removeEventListener("keydown", onDocumentKeyDown, true);
    document.removeEventListener("mousedown", onDocumentMouseDown, true);
    resizeObserver.disconnect();
  };
  return { dispose };
}
