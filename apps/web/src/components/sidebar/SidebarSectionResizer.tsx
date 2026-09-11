// Horizontal divider for the desktop Spaces and Agents sidebar regions.
// SidebarRoot supplies the measured container; uiStore owns the persisted ratio.
// The shared pointer lifecycle releases capture and listeners on every exit.

import { onCleanup } from "solid-js";
import {
  SIDEBAR_SPLIT_DEFAULT,
  SIDEBAR_SPLIT_MAX,
  SIDEBAR_SPLIT_MIN,
  setSidebarSplitRatio,
  uiStore,
} from "../../store/uiStore.ts";
import { beginPointerResizeDrag } from "../../lib/resizeDrag.ts";

type SidebarSectionResizerProps = {
  container: () => HTMLElement | undefined;
};

const KEYBOARD_RATIO_STEP = 0.05;

export function SidebarSectionResizer(props: SidebarSectionResizerProps) {
  let disposePointerResize: (() => void) | undefined;

  onCleanup(() => disposePointerResize?.());

  function startPointerResize(event: PointerEvent): void {
    if (event.button !== 0 || disposePointerResize) return;

    const containerHeight = props.container()?.getBoundingClientRect().height;
    if (containerHeight === undefined || !Number.isFinite(containerHeight) || containerHeight <= 0) return;

    event.preventDefault();
    const target = event.currentTarget as HTMLElement;
    const startY = event.clientY;
    const startRatio = uiStore.sidebarSplitRatio;
    disposePointerResize = beginPointerResizeDrag({
      target,
      pointerId: event.pointerId,
      initialGeometry: startRatio,
      geometryFor: (moveEvent) => startRatio + (moveEvent.clientY - startY) / containerHeight,
      onMove: setSidebarSplitRatio,
      onCommit: setSidebarSplitRatio,
      onRelease: () => {
        disposePointerResize = undefined;
      },
    });
  }

  function resizeFromKeyboard(event: KeyboardEvent): void {
    let nextRatio: number | undefined;
    switch (event.key) {
      case "ArrowUp":
        nextRatio = uiStore.sidebarSplitRatio - KEYBOARD_RATIO_STEP;
        break;
      case "ArrowDown":
        nextRatio = uiStore.sidebarSplitRatio + KEYBOARD_RATIO_STEP;
        break;
      case "Home":
        nextRatio = SIDEBAR_SPLIT_MIN;
        break;
      case "End":
        nextRatio = SIDEBAR_SPLIT_MAX;
        break;
      default:
        return;
    }
    event.preventDefault();
    setSidebarSplitRatio(nextRatio);
  }

  return (
    <div
      class="workbench-sidebar-section-resizer"
      data-testid="sidebar-section-resizer"
      role="separator"
      aria-label="Resize Spaces and Agents"
      aria-orientation="horizontal"
      aria-valuemin={SIDEBAR_SPLIT_MIN * 100}
      aria-valuemax={SIDEBAR_SPLIT_MAX * 100}
      aria-valuenow={Math.round(uiStore.sidebarSplitRatio * 100)}
      aria-valuetext={`${Math.round(uiStore.sidebarSplitRatio * 100)}%`}
      tabIndex={0}
      onPointerDown={startPointerResize}
      onKeyDown={resizeFromKeyboard}
      onDblClick={() => setSidebarSplitRatio(SIDEBAR_SPLIT_DEFAULT)}
    />
  );
}
