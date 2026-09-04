// Owns compact-deck horizontal swipe tracking and settle timing.
// It arbitrates against the workspace edge drawer, then delegates committed tab
// switches and terminal creation to the deck's canonical operations.
// Terminal vertical gestures remain untouched for scroll and mouse forwarding.

import {
  onCleanup,
  onMount,
  type Accessor,
  type Setter,
} from "solid-js";
import type { Layout } from "../store/paneLayout.ts";
import type { Session } from "@roost/shared/wire";
import {
  NEW_BLOOM_MS,
  endMode,
  newFabProgress,
  settleDurationMs,
  shouldCommitSwitch,
  type Swipe,
} from "../lib/deckSwipe.ts";
import { EDGE_PX, lockAxis, openOffsetPx } from "../lib/edgeSwipeDrawer.ts";
import { dragDrawer, settleDrawerOpen } from "../lib/drawerDrag.ts";
import { isCompact } from "../lib/windowSizeClass.ts";
import type { TerminalDeckProps } from "./terminal-deck-model.ts";

const SETTLE_SLACK_MS = 20;

interface SwipeDeckModel {
  layout: Accessor<Layout | null>;
  mobileTabs: Accessor<Session[]>;
  size: Accessor<{ w: number; h: number }>;
}

interface SwipeDeckOperations {
  newTab(paneId: string): Promise<void>;
  select(id: string): void;
}

export function bindTerminalDeckSwipe(
  props: TerminalDeckProps,
  swipe: Accessor<Swipe | null>,
  setSwipe: Setter<Swipe | null>,
  model: SwipeDeckModel,
  operations: SwipeDeckOperations,
  getDeckElement: () => HTMLDivElement | undefined,
): void {
  let newTerminalArmed = false;

  const armSwipe = (deltaX: number): void => {
    newTerminalArmed = false;
    if (swipe()?.phase === "settle" || !isCompact()) return;
    const tabs = model.mobileTabs();
    const currentIdx = tabs.findIndex((tab) => tab.id === props.activeSessionId);
    if (currentIdx < 0) return;
    const direction: 1 | -1 = deltaX < 0 ? 1 : -1;
    const neighborId = direction === 1
      ? tabs[currentIdx + 1]?.id ?? null
      : tabs[currentIdx - 1]?.id ?? null;
    setSwipe({
      phase: "track",
      currentId: props.activeSessionId!,
      neighborId,
      dir: direction,
      offset: deltaX,
      mode: endMode(direction, !!neighborId),
    });
  };
  const trackSwipe = (deltaX: number): void => {
    setSwipe((previous) => {
      if (!previous || previous.phase !== "track") return previous;
      const width = model.size().w;
      return {
        ...previous,
        offset: Math.max(-width, Math.min(width, deltaX)),
      };
    });
    const current = swipe();
    if (current?.mode === "workspace") {
      dragDrawer(openOffsetPx(current.offset, window.innerWidth));
      return;
    }
    if (
      current?.mode === "new-terminal"
      && !newTerminalArmed
      && newFabProgress(current.offset, model.size().w) >= 1
    ) {
      newTerminalArmed = true;
      navigator.vibrate?.(8);
    }
  };
  const endSwipe = (deltaX: number, velocity: number): void => {
    const current = swipe();
    if (current?.mode === "workspace" && current.phase === "track") {
      settleDrawerOpen(
        shouldCommitSwitch(deltaX, velocity, current.dir, window.innerWidth),
      );
      setSwipe(null);
      return;
    }
    setSwipe((previous) => {
      if (!previous || previous.phase !== "track") return previous;
      const width = model.size().w;
      const offset = Math.abs(previous.offset);
      const commit = shouldCommitSwitch(deltaX, velocity, previous.dir, width);
      if (commit) {
        if (previous.mode === "new-terminal") {
          navigator.vibrate?.(12);
          setTimeout(() => {
            void operations.newTab(model.layout()?.focusedPaneId ?? "");
            setSwipe(null);
          }, NEW_BLOOM_MS + SETTLE_SLACK_MS);
          return {
            ...previous,
            phase: "settle",
            settleTarget: "commit",
            settleMs: NEW_BLOOM_MS,
          };
        }
        const settleMs = settleDurationMs(width - offset, width);
        const neighborId = previous.neighborId;
        setTimeout(() => {
          operations.select(neighborId!);
          setSwipe(null);
        }, settleMs + SETTLE_SLACK_MS);
        return {
          ...previous,
          phase: "settle",
          settleTarget: "commit",
          offset: -previous.dir * width,
          settleMs,
        };
      }
      const settleMs = settleDurationMs(offset, width);
      setTimeout(() => setSwipe(null), settleMs + SETTLE_SLACK_MS);
      return {
        ...previous,
        phase: "settle",
        settleTarget: "cancel",
        offset: 0,
        settleMs,
      };
    });
  };

  onMount(() => {
    const deckElement = getDeckElement();
    if (!deckElement) return;
    let startX = 0;
    let startY = 0;
    let lastX = 0;
    let axis: "none" | "x" | "y" = "none";
    let armed = false;
    let tracking = false;
    let samples: Array<{ x: number; t: number }> = [];
    const onStart = (event: TouchEvent): void => {
      armed = false;
      axis = "none";
      tracking = false;
      if (!isCompact() || event.touches.length !== 1) return;
      const touch = event.touches[0]!;
      if (touch.clientX <= EDGE_PX) return;
      startX = touch.clientX;
      startY = touch.clientY;
      lastX = touch.clientX;
      samples = [{ x: touch.clientX, t: performance.now() }];
      tracking = true;
    };
    const onMove = (event: TouchEvent): void => {
      if (!tracking) return;
      const touch = event.touches[0];
      if (!touch) return;
      const deltaX = touch.clientX - startX;
      const deltaY = touch.clientY - startY;
      if (axis === "none") {
        const lockedAxis = lockAxis(deltaX, deltaY);
        if (lockedAxis === "none") return;
        axis = lockedAxis;
      }
      if (axis !== "x") return;
      event.preventDefault();
      event.stopPropagation();
      const now = performance.now();
      samples.push({ x: touch.clientX, t: now });
      while (samples.length > 2 && samples[0]!.t < now - 120) samples.shift();
      lastX = touch.clientX;
      if (!armed) {
        armed = true;
        armSwipe(deltaX);
      }
      trackSwipe(deltaX);
    };
    const onEnd = (): void => {
      if (!armed) {
        tracking = false;
        return;
      }
      armed = false;
      tracking = false;
      const now = performance.now();
      while (samples.length > 1 && samples[0]!.t < now - 80) samples.shift();
      let velocity = 0;
      if (samples.length >= 2) {
        const first = samples[0]!;
        const last = samples[samples.length - 1]!;
        const elapsed = last.t - first.t;
        if (elapsed > 0) velocity = (last.x - first.x) / elapsed;
      }
      endSwipe(lastX - startX, velocity);
      samples = [];
    };
    deckElement.addEventListener("touchstart", onStart, { capture: true, passive: true });
    deckElement.addEventListener("touchmove", onMove, { capture: true, passive: false });
    deckElement.addEventListener("touchend", onEnd, { capture: true, passive: true });
    deckElement.addEventListener("touchcancel", onEnd, { capture: true, passive: true });
    onCleanup(() => {
      deckElement.removeEventListener("touchstart", onStart, { capture: true });
      deckElement.removeEventListener("touchmove", onMove, { capture: true });
      deckElement.removeEventListener("touchend", onEnd, { capture: true });
      deckElement.removeEventListener("touchcancel", onEnd, { capture: true });
    });
  });
}
