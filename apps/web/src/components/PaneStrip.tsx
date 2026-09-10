/*
 * Per-pane terminal tab state owner.
 * TerminalDeck supplies layout callbacks; this component measures tabs, runs drag
 * transitions, and coordinates overflow/hover state without owning a session route.
 * PaneTab, PaneTabList, and PaneTabHoverCard render the split presentational pieces.
 */

import { For, Show, createEffect, createMemo, createSignal, on, onCleanup, onMount } from "solid-js";
import type { Session } from "@roost/shared/wire";
import { dragArmed } from "../lib/dragThreshold.ts";
import { animateSpring, SPRING_SNAP } from "../lib/spring.ts";
import { prefersReducedMotion } from "../lib/prefersReducedMotion.ts";
import { isCompact, isTouchDevice } from "../lib/windowSizeClass.ts";
import { anchoredMenuPosition } from "./contextMenuPrimitives.tsx";
import { createTrackedTimeouts } from "./trackedTimeout.ts";
import { IconButton } from "./Settings/md/IconButton.tsx";
import { PaneTab } from "./PaneTab.tsx";
import { PaneTabHoverCard } from "./PaneTabHoverCard.tsx";
import { PaneTabList } from "./PaneTabList.tsx";

export interface PaneStripProps {
  paneId: string;
  tabs: Session[];
  selectedTab: string;
  focused: boolean;
  onSelect: (id: string) => void;
  onClose: (session: Session) => void;
  onReorder: (orderedIds: string[]) => void;
  onNewTab: () => void;
  onTabDragMove?: (clientX: number, clientY: number) => void;
  onTabTileDrop?: (tabId: string, clientX: number, clientY: number) => boolean;
  onTabDragEnd?: () => void;
}

type TabRect = {
  left: number;
  width: number;
  center: number;
};

type DragState = {
  id: string;
  fromIdx: number;
  toIdx: number;
  dx: number;
  slot: number;
  rects: TabRect[];
  released: boolean;
};

export function PaneStrip(props: PaneStripProps) {
  let barElement: HTMLDivElement | undefined;
  let overflowButtonElement: HTMLButtonElement | undefined;
  let indicatorFrame = 0;
  let hoverTimer = 0;
  let cancelSettle: (() => void) | undefined;

  const [indicator, setIndicator] = createSignal({ left: 0, width: 0, ready: false });
  const [overflow, setOverflow] = createSignal(false);
  const [drag, setDrag] = createSignal<DragState | null>(null);
  const [closing, setClosing] = createSignal<Set<string>>(new Set());
  const [listOpen, setListOpen] = createSignal<{ right: number; y: number } | null>(null);
  const [hover, setHover] = createSignal<{ id: string; rect: DOMRect } | null>(null);
  const tabIdsKey = createMemo(() => props.tabs.map((session) => session.id).join("\u0000"));
  const hoveredSession = createMemo(() => {
    const hovered = hover();
    return hovered ? props.tabs.find((tab) => tab.id === hovered.id) : null;
  });
  const setTimeoutTracked = createTrackedTimeouts();

  function measureOverflow(): void {
    if (!barElement) return;
    const isOverflowing = barElement.scrollWidth > barElement.clientWidth + 1;
    setOverflow((previous) => previous === isOverflowing ? previous : isOverflowing);
  }

  function measureTabIndicator(): void {
    const activeTab = barElement?.querySelector<HTMLElement>('[data-active="true"]');
    if (!activeTab) {
      setIndicator((previous) => previous.ready ? { ...previous, ready: false } : previous);
      measureOverflow();
      return;
    }
    const left = activeTab.offsetLeft;
    const width = activeTab.offsetWidth;
    setIndicator((previous) =>
      previous.ready && previous.left === left && previous.width === width
        ? previous
        : { left, width, ready: true });
    measureOverflow();
  }

  const tabResizeObserver = new ResizeObserver(measureTabIndicator);
  const barResizeObserver = new ResizeObserver(measureOverflow);

  onMount(() => {
    if (barElement) barResizeObserver.observe(barElement);
    measureOverflow();
  });
  createEffect(on([() => props.selectedTab, tabIdsKey], () => {
    cancelAnimationFrame(indicatorFrame);
    indicatorFrame = requestAnimationFrame(() => {
      indicatorFrame = 0;
      tabResizeObserver.disconnect();
      const tabs = barElement?.querySelectorAll<HTMLElement>(".df-tab") ?? [];
      for (const tab of tabs) tabResizeObserver.observe(tab);
      measureTabIndicator();
    });
  }));
  onCleanup(() => {
    cancelAnimationFrame(indicatorFrame);
    tabResizeObserver.disconnect();
    barResizeObserver.disconnect();
    cancelSettle?.();
    clearTimeout(hoverTimer);
  });

  function clearHover(): void {
    if (hoverTimer) {
      clearTimeout(hoverTimer);
      hoverTimer = 0;
    }
    setHover((previous) => previous ? null : previous);
  }

  function toggleList(): void {
    if (listOpen()) {
      setListOpen(null);
      return;
    }
    if (!overflowButtonElement) return;
    clearHover();
    setListOpen(anchoredMenuPosition(overflowButtonElement));
  }

  function armHover(id: string, rect: DOMRect): void {
    if (isCompact() || isTouchDevice() || drag() || listOpen()) return;
    clearHover();
    hoverTimer = window.setTimeout(() => setHover({ id, rect }), 450);
  }

  function closeTab(session: Session): void {
    if (prefersReducedMotion() || closing().has(session.id)) {
      props.onClose(session);
      return;
    }
    setClosing((previous) => new Set(previous).add(session.id));
    setTimeoutTracked(() => {
      props.onClose(session);
      setClosing((previous) => {
        const next = new Set(previous);
        next.delete(session.id);
        return next;
      });
    }, 220);
  }

  function tabRects(): TabRect[] {
    const elements = barElement?.querySelectorAll<HTMLElement>(".df-tab") ?? [];
    return Array.from(elements).map((element) => ({
      left: element.offsetLeft,
      width: element.offsetWidth,
      center: element.offsetLeft + element.offsetWidth / 2,
    }));
  }

  function restingDx(currentDrag: DragState): number {
    if (currentDrag.toIdx === currentDrag.fromIdx) return 0;
    const { rects, fromIdx, toIdx } = currentDrag;
    if (toIdx > fromIdx) {
      return rects[toIdx].left + rects[toIdx].width - rects[fromIdx].width - rects[fromIdx].left;
    }
    return rects[toIdx].left - rects[fromIdx].left;
  }

  function onTabPointerDown(event: PointerEvent, id: string): void {
    if (event.button !== 0) return;
    cancelSettle?.();
    clearHover();
    const index = props.tabs.findIndex((tab) => tab.id === id);
    if (index < 0) return;

    const start = { x: event.clientX, y: event.clientY, id, index };
    const onTabDragMove = props.onTabDragMove;
    const onTabTileDrop = props.onTabTileDrop;
    const onTabDragEnd = props.onTabDragEnd;
    const onReorder = props.onReorder;

    function teardown(): void {
      window.removeEventListener("pointermove", onMove);
      window.removeEventListener("pointerup", onUp);
      window.removeEventListener("pointercancel", onCancel);
    }

    function onMove(moveEvent: PointerEvent): void {
      const currentDrag = drag();
      if (!currentDrag) {
        if (!dragArmed(start, moveEvent.clientX, moveEvent.clientY)) return;
        const rects = tabRects();
        setDrag({
          id: start.id,
          fromIdx: start.index,
          toIdx: start.index,
          dx: moveEvent.clientX - start.x,
          slot: rects[start.index].width + 2,
          rects,
          released: false,
        });
        onTabDragMove?.(moveEvent.clientX, moveEvent.clientY);
        return;
      }

      const dx = moveEvent.clientX - start.x;
      const center = currentDrag.rects[currentDrag.fromIdx].center + dx;
      let toIdx = currentDrag.fromIdx;
      while (toIdx < currentDrag.rects.length - 1 && center > currentDrag.rects[toIdx + 1].center) toIdx++;
      while (toIdx > 0 && center < currentDrag.rects[toIdx - 1].center) toIdx--;
      setDrag({ ...currentDrag, dx, toIdx });
      onTabDragMove?.(moveEvent.clientX, moveEvent.clientY);
    }

    function onUp(upEvent: PointerEvent): void {
      teardown();
      const currentDrag = drag();
      if (!currentDrag) return;

      function swallowClick(): void {
        window.addEventListener(
          "click",
          (clickEvent) => {
            clickEvent.stopPropagation();
            clickEvent.preventDefault();
          },
          { capture: true, once: true },
        );
      }

      const tiled = onTabTileDrop?.(currentDrag.id, upEvent.clientX, upEvent.clientY) ?? false;
      onTabDragEnd?.();
      if (tiled) {
        swallowClick();
        setDrag(null);
        return;
      }

      if (currentDrag.toIdx !== currentDrag.fromIdx) {
        swallowClick();
        const { fromIdx, toIdx } = currentDrag;
        const commit = (): void => {
          const ids = props.tabs.map((tab) => tab.id);
          const [moved] = ids.splice(fromIdx, 1);
          ids.splice(toIdx, 0, moved);
          onReorder(ids);
          setDrag(null);
        };
        const rest = restingDx(currentDrag);
        if (prefersReducedMotion()) {
          commit();
          return;
        }
        setDrag({ ...currentDrag, released: true });
        cancelSettle = animateSpring(
          { position: currentDrag.dx, velocity: 0 },
          rest,
          SPRING_SNAP,
          (position) => setDrag((activeDrag) =>
            activeDrag ? { ...activeDrag, dx: position, released: true } : null),
          commit,
        );
        return;
      }

      setDrag(null);
    }

    function onCancel(): void {
      teardown();
      setDrag(null);
      onTabDragEnd?.();
    }

    window.addEventListener("pointermove", onMove);
    window.addEventListener("pointerup", onUp);
    window.addEventListener("pointercancel", onCancel);
  }

  function tabDragStyle(index: number): Record<string, string> {
    const currentDrag = drag();
    if (!currentDrag) return {};
    if (index === currentDrag.fromIdx) {
      return {
        transform: `translateX(${currentDrag.dx}px)`,
        transition: "none",
        "z-index": "3",
      };
    }
    let shift = 0;
    if (currentDrag.fromIdx < currentDrag.toIdx && index > currentDrag.fromIdx && index <= currentDrag.toIdx) {
      shift = -currentDrag.slot;
    } else if (currentDrag.fromIdx > currentDrag.toIdx && index >= currentDrag.toIdx && index < currentDrag.fromIdx) {
      shift = currentDrag.slot;
    }
    return {
      transform: shift ? `translateX(${shift}px)` : "translateX(0)",
      transition: "transform var(--md-sys-motion-duration-short4) var(--md-sys-motion-easing-emphasized)",
    };
  }

  return (
    <div
      ref={barElement}
      class="df-tab-bar workbench-pane-tab-strip"
      data-testid={`pane-strip-${props.paneId}`}
      data-pane-strip={props.paneId}
      data-focused={props.focused ? "true" : "false"}
      data-dragging={drag() ? "true" : "false"}
    >
      <div
        class="df-tab-indicator workbench-pane-tab-strip__indicator"
        aria-hidden="true"
        style={{
          transform: `translateX(${indicator().left}px) scaleX(${indicator().width})`,
          opacity: indicator().ready && !drag() ? "1" : "0",
        }}
      />
      <For each={props.tabs}>
        {(session, index) => (
          <PaneTab
            session={session}
            active={session.id === props.selectedTab}
            dragging={drag()?.id === session.id}
            closing={closing().has(session.id)}
            style={tabDragStyle(index())}
            onPointerDown={(event) => onTabPointerDown(event, session.id)}
            onSelect={() => {
              clearHover();
              props.onSelect(session.id);
            }}
            onHoverStart={(element) => armHover(session.id, element.getBoundingClientRect())}
            onHoverEnd={clearHover}
            onClose={(event) => {
              event.stopPropagation();
              event.preventDefault();
              closeTab(session);
            }}
          />
        )}
      </For>
      <Show when={overflow()}>
        <IconButton
          ref={overflowButtonElement}
          icon="keyboard_arrow_down"
          label="All terminals in this pane"
          class="df-tab-overflow workbench-pane-tab-control"
          data-testid="tab-overflow"
          title="All terminals"
          onClick={toggleList}
        />
      </Show>
      <IconButton
        icon="add"
        label="New terminal — same folder and server"
        class="df-tab-new workbench-pane-tab-control"
        data-testid="tab-new"
        title="New terminal in this folder (or double-click the empty bar)"
        onClick={props.onNewTab}
      />
      <div
        class="df-tab-filler workbench-pane-tab-strip__filler"
        data-testid="tab-filler"
        title="Double-click to open a new terminal in this folder"
        onDblClick={props.onNewTab}
      />
      <Show when={listOpen()}>
        {(position) => (
          <PaneTabList
            position={position()}
            tabs={props.tabs}
            selectedTab={props.selectedTab}
            trigger={() => overflowButtonElement}
            tabBar={() => barElement}
            onSelect={props.onSelect}
            onClose={() => setListOpen(null)}
          />
        )}
      </Show>
      <Show when={hover()}>
        {(hovered) => (
          <Show when={hoveredSession()}>
            {(session) => <PaneTabHoverCard session={session()} rect={hovered().rect} />}
          </Show>
        )}
      </Show>
    </div>
  );
}
