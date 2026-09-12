// Compact sidebar drawer and its left-edge gesture owner.
// AppShell mounts this surface only for compact layouts and closes it on route changes.
// The real SidebarRoot and drawerDrag owner preserve the existing navigation behavior.

import { onCleanup, onMount } from "solid-js";
import { SidebarRoot } from "../sidebar/SidebarRoot.tsx";
import { uiStore, closeSidebar } from "../../store/uiStore.ts";
import { EDGE_PX, closeOffsetPx, lockAxis, openOffsetPx, shouldClose, shouldOpen } from "../../lib/edgeSwipeDrawer.ts";
import { dragDrawer, registerDrawer, settleDrawerClose, settleDrawerOpen } from "../../lib/drawerDrag.ts";

type DrawerGestureMode = "open" | "close" | null;
type TouchSample = { x: number; t: number };

export function MobileSidebarDrawer() {
  let drawerGestureMode: DrawerGestureMode = null;
  let gestureStartX = 0;
  let gestureStartY = 0;
  let latestX = 0;
  let lockedAxis: "none" | "x" | "y" = "none";
  let gestureArmed = false;
  let gestureCandidate = false;
  let gestureSamples: TouchSample[] = [];

  function resetGesture() {
    drawerGestureMode = null;
    gestureArmed = false;
    gestureCandidate = false;
    gestureSamples = [];
  }

  function handleTouchStart(event: TouchEvent) {
    drawerGestureMode = null;
    if (event.touches.length !== 1) {
      gestureCandidate = false;
      return;
    }

    const touch = event.touches[0];
    if (!touch) return;
    const target = event.target as Element | null;
    if (uiStore.sidebarOpen) {
      if (target?.closest(".df-tab-bar, .df-row-swipe")) {
        gestureCandidate = false;
        return;
      }
      drawerGestureMode = "close";
      gestureCandidate = true;
    } else if (touch.clientX <= EDGE_PX) {
      drawerGestureMode = "open";
      gestureCandidate = true;
    } else {
      gestureCandidate = false;
      return;
    }

    gestureStartX = touch.clientX;
    gestureStartY = touch.clientY;
    latestX = touch.clientX;
    lockedAxis = "none";
    gestureArmed = false;
    gestureSamples = [{ x: touch.clientX, t: performance.now() }];
  }

  function handleTouchMove(event: TouchEvent) {
    if (!gestureCandidate || drawerGestureMode === null) return;
    const touch = event.touches[0];
    if (!touch) return;

    const deltaX = touch.clientX - gestureStartX;
    const deltaY = touch.clientY - gestureStartY;
    if (lockedAxis === "none") {
      const nextAxis = lockAxis(deltaX, deltaY);
      if (nextAxis === "none") return;
      if (nextAxis === "y") {
        gestureCandidate = false;
        return;
      }
      lockedAxis = "x";
    }

    if ((drawerGestureMode === "open" && deltaX <= 0) || (drawerGestureMode === "close" && deltaX >= 0)) {
      gestureCandidate = false;
      return;
    }

    event.preventDefault();
    event.stopPropagation();
    gestureArmed = true;
    const now = performance.now();
    gestureSamples.push({ x: touch.clientX, t: now });
    while (gestureSamples.length > 2 && gestureSamples[0]!.t < now - 120) gestureSamples.shift();
    latestX = touch.clientX;

    const offset = drawerGestureMode === "close"
      ? closeOffsetPx(deltaX, window.innerWidth)
      : openOffsetPx(deltaX, window.innerWidth);
    dragDrawer(offset);
  }

  function handleTouchEnd() {
    if (!gestureArmed || drawerGestureMode === null) {
      resetGesture();
      return;
    }

    const deltaX = latestX - gestureStartX;
    const now = performance.now();
    while (gestureSamples.length > 1 && gestureSamples[0]!.t < now - 80) gestureSamples.shift();
    const firstSample = gestureSamples[0];
    const lastSample = gestureSamples.at(-1);
    const elapsed = firstSample && lastSample ? lastSample.t - firstSample.t : 0;
    const velocity = elapsed > 0 && firstSample && lastSample
      ? (lastSample.x - firstSample.x) / elapsed
      : 0;

    if (drawerGestureMode === "open") {
      settleDrawerOpen(shouldOpen(deltaX, velocity, window.innerWidth));
    } else {
      settleDrawerClose(shouldClose(deltaX, velocity, window.innerWidth));
    }
    resetGesture();
  }

  onMount(() => {
    window.addEventListener("touchstart", handleTouchStart, { capture: true, passive: true });
    window.addEventListener("touchmove", handleTouchMove, { capture: true, passive: false });
    window.addEventListener("touchend", handleTouchEnd, { capture: true, passive: true });
    window.addEventListener("touchcancel", handleTouchEnd, { capture: true, passive: true });
  });

  onCleanup(() => {
    window.removeEventListener("touchstart", handleTouchStart, true);
    window.removeEventListener("touchmove", handleTouchMove, true);
    window.removeEventListener("touchend", handleTouchEnd, true);
    window.removeEventListener("touchcancel", handleTouchEnd, true);
    registerDrawer(null);
  });

  return (
    <>
      <div
        class="roost-drawer-overlay"
        data-testid="sidebar-overlay"
        data-open={uiStore.sidebarOpen ? "true" : "false"}
        onClick={closeSidebar}
        aria-hidden="true"
      />
      <aside
        class="roost-drawer workbench-sidebar-drawer"
        data-testid="sidebar-drawer"
        data-open={uiStore.sidebarOpen ? "true" : "false"}
        aria-hidden={!uiStore.sidebarOpen}
        ref={registerDrawer}
      >
        <SidebarRoot />
      </aside>
    </>
  );
}
