/*
 * Rail-viewport owner for a pane's terminal tab strip.
 * PaneStrip calls this once, renders its overflow chevron from `overflowing`, and hands
 * `revealSelected` to the overflow menu; this module owns every rail measurement and the
 * only scroll-into-view of the selected tab. It depends on the rail element and Solid hooks.
 */

import { createEffect, createSignal, on, onCleanup, onMount, type Accessor } from "solid-js";
import { diag } from "@roost/observability/diag";

export interface PaneTabRailScrollOptions {
  paneId: Accessor<string>;
  rail: Accessor<HTMLElement | undefined>;
  selectedTab: Accessor<string>;
  tabIdsKey: Accessor<string>;
  dragging: Accessor<boolean>;
}

export interface PaneTabRailScroll {
  /** True only while the rail's content exceeds its box — tabs shrink to fit first, so
   *  this is both the honest overflow-chevron condition and the "tabs are packed" flag. */
  overflowing: Accessor<boolean>;
  /** Scroll the active tab into view. The selection effect covers a CHANGED selection;
   *  re-picking the tab that is already selected writes no signal, so the overflow menu
   *  calls this directly instead of focusing a tab scrolled out of the rail. */
  revealSelected: () => void;
}

export function createPaneTabRailScroll(options: PaneTabRailScrollOptions): PaneTabRailScroll {
  const [overflowing, setOverflowing] = createSignal(false);
  let measurementFrame = 0;

  function measure(): void {
    const rail = options.rail();
    if (!rail) return;
    const isOverflowing = rail.scrollWidth > rail.clientWidth + 1;
    setOverflowing((previous) => {
      if (previous === isOverflowing) return previous;
      diag("paneTabs.rail_overflow", { pane_id: options.paneId(), overflowing: isOverflowing });
      return isOverflowing;
    });
  }

  function revealSelected(): void {
    if (options.dragging()) return;
    options.rail()
      ?.querySelector<HTMLElement>(".df-tab[data-active='true']")
      ?.scrollIntoView({ inline: "nearest", block: "nearest" });
  }

  const tabResizeObserver = new ResizeObserver(() => measure());
  const tabRailResizeObserver = new ResizeObserver(() => {
    measure();
    revealSelected();
  });

  onMount(() => {
    const rail = options.rail();
    if (rail) tabRailResizeObserver.observe(rail);
    measure();
  });
  createEffect(on([options.selectedTab, options.tabIdsKey], () => {
    cancelAnimationFrame(measurementFrame);
    measurementFrame = requestAnimationFrame(() => {
      measurementFrame = 0;
      tabResizeObserver.disconnect();
      const tabs = options.rail()?.querySelectorAll<HTMLElement>(".df-tab") ?? [];
      for (const tab of tabs) tabResizeObserver.observe(tab);
      measure();
      revealSelected();
    });
  }));
  onCleanup(() => {
    cancelAnimationFrame(measurementFrame);
    tabResizeObserver.disconnect();
    tabRailResizeObserver.disconnect();
  });

  return { overflowing, revealSelected };
}
