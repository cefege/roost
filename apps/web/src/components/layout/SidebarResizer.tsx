// Desktop sidebar boundary. Owns pointer capture, persisted width updates, and reset.
// AppShell positions this focused control in the workbench grid.
// It depends on the existing resize-drag lifecycle and uiStore only.

import { onCleanup } from "solid-js";
import {
  SIDEBAR_WIDTH_DEFAULT,
  SIDEBAR_WIDTH_MAX,
  SIDEBAR_WIDTH_MIN,
  setSidebarWidth,
  uiStore,
} from "../../store/uiStore.ts";
import { beginPointerResizeDrag } from "../../lib/resizeDrag.ts";
export function SidebarResizer() {
  let disposePointerResize: (() => void) | undefined;

  onCleanup(() => disposePointerResize?.());

  function startPointerResize(event: PointerEvent) {
    if (event.button !== 0 || disposePointerResize) return;

    const target = event.currentTarget as HTMLElement;
    const startX = event.clientX;
    const startWidth = uiStore.sidebarWidth;
    document.body.style.cursor = "col-resize";
    document.body.style.userSelect = "none";

    disposePointerResize = beginPointerResizeDrag({
      target,
      pointerId: event.pointerId,
      initialGeometry: startWidth,
      geometryFor: (moveEvent) => startWidth + (moveEvent.clientX - startX),
      onMove: setSidebarWidth,
      onCommit: setSidebarWidth,
      onRelease: () => {
        disposePointerResize = undefined;
        document.body.style.cursor = "";
        document.body.style.userSelect = "";
      },
    });
  }

  function resizeFromKeyboard(event: KeyboardEvent) {
    let nextWidth: number | undefined;
    switch (event.key) {
      case "ArrowLeft":
        nextWidth = uiStore.sidebarWidth - 10;
        break;
      case "ArrowRight":
        nextWidth = uiStore.sidebarWidth + 10;
        break;
      case "Home":
        nextWidth = SIDEBAR_WIDTH_MIN;
        break;
      case "End":
        nextWidth = SIDEBAR_WIDTH_MAX;
        break;
      default:
        return;
    }
    event.preventDefault();
    setSidebarWidth(nextWidth);
  }

  return (
    <div
      class="workbench-sidebar-resizer"
      data-testid="sidebar-resizer"
      role="separator"
      aria-label="Resize sidebar"
      aria-orientation="vertical"
      aria-valuemin={SIDEBAR_WIDTH_MIN}
      aria-valuemax={SIDEBAR_WIDTH_MAX}
      aria-valuenow={uiStore.sidebarWidth}
      tabIndex={0}
      onPointerDown={startPointerResize}
      onKeyDown={resizeFromKeyboard}
      onDblClick={() => setSidebarWidth(SIDEBAR_WIDTH_DEFAULT)}
    />
  );
}
