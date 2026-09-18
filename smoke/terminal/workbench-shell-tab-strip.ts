// Workbench tab strip browser assertions own the rail fit contract and its packed floor.
// workbench-shell.spec.ts calls the fit helper after creating six real terminal sessions,
// then the floor helper after spawning enough sessions to pack the rail.
// Both measure rendered geometry; session and layout operations stay in the scenario.
// It depends on Playwright's live browser surface and fixture assertions.

import type { Page } from "@playwright/test";
import { expect } from "./fixtures.ts";

export async function expectConnectedWorkbenchTabStrip(page: Page): Promise<void> {
  const paneStripShell = page.locator("[data-pane-strip].workbench-pane-tab-strip").first();
  const tabRail = paneStripShell.locator(":scope > .workbench-pane-tab-strip__tabs");
  const tabStripLayout = await paneStripShell.evaluate((shell) => {
    const rail = shell.querySelector<HTMLElement>(":scope > .workbench-pane-tab-strip__tabs");
    const actions = shell.querySelector<HTMLElement>(":scope > .workbench-pane-tab-strip__actions");
    const activeTab = rail?.querySelector<HTMLElement>(".df-tab[data-active='true']");
    const inactiveTab = rail?.querySelector<HTMLElement>(".df-tab[data-active='false']");
    const newTab = shell.querySelector<HTMLElement>("[data-testid='tab-new']");
    const filler = shell.querySelector<HTMLElement>("[data-testid='tab-filler']");
    const editor = shell.closest<HTMLElement>(".workbench-editor-region");
    const arrange = document.querySelector<HTMLElement>("[data-testid='arrange-btn']");
    const tabs = rail ? Array.from(rail.querySelectorAll<HTMLElement>(".df-tab")) : [];
    if (!rail || !actions || !activeTab || !inactiveTab || !newTab || !filler
      || tabs.length === 0 || !editor || !arrange) {
      throw new Error("workbench tab shell is incomplete");
    }

    const toRect = (element: HTMLElement) => {
      const rect = element.getBoundingClientRect();
      return { left: rect.left, right: rect.right, top: rect.top, bottom: rect.bottom };
    };
    const intersects = (
      first: { left: number; right: number; top: number; bottom: number },
      second: { left: number; right: number; top: number; bottom: number },
    ) => first.left < second.right
      && first.right > second.left
      && first.top < second.bottom
      && first.bottom > second.top;
    const clippedTabRects = (railRect: { left: number; right: number }) => tabs.map(toRect).map((rect) => ({
      ...rect,
      left: Math.max(rect.left, railRect.left),
      right: Math.min(rect.right, railRect.right),
    })).filter((rect) => rect.left < rect.right);

    const railRectAtStart = toRect(rail);
    const newTabRectAtStart = toRect(newTab);
    const actionRectsBeforeScroll = Array.from(actions.querySelectorAll<HTMLElement>("button")).map(toRect);
    const visibleTabRects = clippedTabRects(railRectAtStart);
    rail.scrollLeft = rail.scrollWidth;
    const newTabRectAfterScroll = toRect(newTab);
    const visibleTabRectsAtEnd = clippedTabRects(toRect(rail));
    const actionRectsAfterScroll = Array.from(actions.querySelectorAll<HTMLElement>("button")).map(toRect);
    rail.scrollLeft = 0;

    const activeStyle = getComputedStyle(activeTab);
    const inactiveStyle = getComputedStyle(inactiveTab);
    const rootStyle = getComputedStyle(document.documentElement);
    const focusRingProbe = document.createElement("div");
    focusRingProbe.style.borderTop = "var(--workbench-border-width) solid var(--workbench-focus-ring)";
    document.body.append(focusRingProbe);
    const focusRingColor = getComputedStyle(focusRingProbe).borderTopColor;
    focusRingProbe.remove();
    const focused = rail.getAttribute("data-focused");
    rail.setAttribute("data-focused", "false");
    const unfocusedActiveBorder = getComputedStyle(activeTab).borderTopColor;
    if (focused === null) rail.removeAttribute("data-focused");
    else rail.setAttribute("data-focused", focused);

    const minSize = Number.parseFloat(activeStyle.getPropertyValue("min-inline-size"));
    const maxSize = Number.parseFloat(activeStyle.getPropertyValue("max-inline-size"));
    const tokenSize = Number.parseFloat(rootStyle.getPropertyValue("--md-space-8"));
    const actionSize = Number.parseFloat(rootStyle.getPropertyValue("--workbench-tab-action-size"));
    const firstWidth = toRect(tabs[0]!).right - toRect(tabs[0]!).left;
    const inactiveClose = inactiveTab.querySelector<HTMLElement>(".df-tab-close");
    const stripRect = shell.getBoundingClientRect();
    return {
      shellScrolls: shell.scrollWidth > shell.clientWidth + 1,
      railScrolls: rail.scrollWidth > rail.clientWidth + 1,
      actionsAreSibling: actions.parentElement === shell,
      newTabFollowsRail: rail.nextElementSibling === newTab,
      newTabClearOfRail: newTabRectAtStart.left >= railRectAtStart.right - 0.5,
      newTabIntersectsTabAtStart: visibleTabRects.some((tab) => intersects(newTabRectAtStart, tab)),
      newTabIntersectsTabAtEnd: visibleTabRectsAtEnd.some((tab) => intersects(newTabRectAfterScroll, tab)),
      newTabRectAtStart,
      newTabRectAfterScroll,
      actionIntersectsTab: actionRectsBeforeScroll.some((action) => visibleTabRects.some((tab) => intersects(action, tab))),
      arrangeIntersectsAction: actionRectsBeforeScroll.some((action) => intersects(toRect(arrange), action)),
      actionRectsBeforeScroll,
      actionRectsAfterScroll,
      activeBackground: activeStyle.backgroundColor,
      editorBackground: getComputedStyle(editor).backgroundColor,
      inactiveBackground: inactiveStyle.backgroundColor,
      stripBackground: getComputedStyle(shell).backgroundColor,
      activeTopBorder: activeStyle.borderTopColor,
      unfocusedActiveBorder,
      inactiveTopBorder: inactiveStyle.borderTopColor,
      focusRingColor,
      topBorderReserved: tabs.every((tab) => getComputedStyle(tab).borderTopWidth === rootStyle.getPropertyValue("--workbench-border-width").trim()),
      tabsFillStrip: tabs.every((tab) => {
        const rect = toRect(tab);
        return Math.abs(rect.top - stripRect.top) < 0.5 && Math.abs(rect.bottom - stripRect.bottom) < 0.5;
      }),
      minSize,
      maxSize,
      sizesMatchTokens: minSize === actionSize + tokenSize && maxSize === tokenSize * 5,
      widthsWithinRange: tabs.every((tab) => {
        const rect = toRect(tab);
        const width = rect.right - rect.left;
        return width >= minSize && width <= maxSize;
      }),
      widthsUniform: tabs.every((tab) => {
        const rect = toRect(tab);
        return Math.abs((rect.right - rect.left) - firstWidth) <= 0.5;
      }),
      // Free width belongs to the tabs, never to the filler: when the tabs are below
      // their max, nothing is left over. A rail whose basis collapsed (inline-size
      // containment zeroes a tab's intrinsic contribution) donates that width to the
      // filler instead and parks every tab on its floor.
      fillerYieldsToTabs: firstWidth >= maxSize - 0.5
        || toRect(filler).right - toRect(filler).left <= 0.5,
      widthsAboveFloor: firstWidth > minSize,
      overflowChevronPresent: shell.querySelector("[data-testid='tab-overflow']") !== null,
      iconAndTitleIdentity: tabs.every((tab) => {
        const select = tab.querySelector<HTMLElement>(".workbench-pane-tab__select");
        return Boolean(
          select?.querySelector(".workbench-pane-tab__icon")
          && select.querySelector(".workbench-pane-tab__label")
          && tab.querySelector(".agent-status") === null,
        );
      }),
      closeIsPointerInert: inactiveClose ? getComputedStyle(inactiveClose).pointerEvents === "none" : false,
    };
  });

  expect(tabStripLayout.shellScrolls).toBe(false);
  expect(tabStripLayout.railScrolls).toBe(false);
  expect(tabStripLayout.overflowChevronPresent).toBe(false);
  expect(tabStripLayout.actionsAreSibling).toBe(true);
  expect(tabStripLayout.newTabFollowsRail).toBe(true);
  expect(tabStripLayout.fillerYieldsToTabs).toBe(true);
  expect(tabStripLayout.widthsAboveFloor).toBe(true);
  expect(tabStripLayout.newTabClearOfRail).toBe(true);
  expect(tabStripLayout.newTabIntersectsTabAtStart).toBe(false);
  expect(tabStripLayout.newTabIntersectsTabAtEnd).toBe(false);
  expect(tabStripLayout.newTabRectAfterScroll).toEqual(tabStripLayout.newTabRectAtStart);
  expect(tabStripLayout.actionIntersectsTab).toBe(false);
  expect(tabStripLayout.arrangeIntersectsAction).toBe(false);
  expect(tabStripLayout.actionRectsAfterScroll).toEqual(tabStripLayout.actionRectsBeforeScroll);
  expect(tabStripLayout.activeBackground).toBe(tabStripLayout.editorBackground);
  expect(tabStripLayout.inactiveBackground).toBe(tabStripLayout.stripBackground);
  expect(tabStripLayout.activeTopBorder).toBe(tabStripLayout.focusRingColor);
  expect(tabStripLayout.unfocusedActiveBorder).toBe(tabStripLayout.inactiveTopBorder);
  expect(tabStripLayout.topBorderReserved).toBe(true);
  expect(tabStripLayout.tabsFillStrip).toBe(true);
  expect(tabStripLayout.sizesMatchTokens).toBe(true);
  expect(tabStripLayout.widthsWithinRange).toBe(true);
  expect(tabStripLayout.widthsUniform).toBe(true);
  expect(tabStripLayout.iconAndTitleIdentity).toBe(true);
  expect(tabStripLayout.closeIsPointerInert).toBe(true);

  const inactiveTab = tabRail.locator(".df-tab[data-active='false']").first();
  const inactiveClose = inactiveTab.locator(".df-tab-close");
  await expect(inactiveClose).toHaveCSS("opacity", "0");
  await expect(inactiveClose).toHaveCSS("pointer-events", "none");
  const label = inactiveTab.locator(".df-tab-label");
  const tabWidthBeforeHover = (await inactiveTab.boundingBox())?.width ?? 0;
  const labelWidthBeforeHover = (await label.boundingBox())?.width ?? 0;
  expect(tabWidthBeforeHover).toBeGreaterThan(0);
  expect(labelWidthBeforeHover).toBeGreaterThan(0);
  await inactiveTab.hover();
  await expect(inactiveClose).toHaveCSS("opacity", "1");
  await expect(inactiveClose).toHaveCSS("pointer-events", "auto");

  // The close slot is reserved in layout whether or not the X is painted, so
  // revealing it must move nothing: neither the tab box (which would shove its
  // neighbours and stale the hover-card anchor rect PaneStrip captured on
  // mouseenter) nor the label box (which would re-truncate the title mid-hover).
  expect(Math.abs(((await inactiveTab.boundingBox())?.width ?? 0) - tabWidthBeforeHover)).toBeLessThanOrEqual(0.5);
  expect(Math.abs(((await label.boundingBox())?.width ?? 0) - labelWidthBeforeHover)).toBeLessThanOrEqual(0.5);

  // Tabs share the rail's width evenly, so a tab's width is a property of the rail,
  // never of its own title: a PTY rewriting its OSC title must not resize the tab
  // under the cursor, which is the reflow the hover-card anchor was reported against.
  const retitledId = (await inactiveTab.getAttribute("data-testid"))?.replace("tab-", "");
  if (!retitledId) throw new Error("inactive workbench tab carries no session id");
  await page.mouse.move(0, 0);
  await page.evaluate(
    (id) => window.__smoke.input(id, "printf '\\033]0;wb-tab-sizing-fix\\007'\n"),
    retitledId,
  );
  await expect(label).toHaveText("wb-tab-sizing-fix");
  expect(Math.abs(((await inactiveTab.boundingBox())?.width ?? 0) - tabWidthBeforeHover)).toBeLessThanOrEqual(0.5);
  await inactiveTab.hover();
  await expect(inactiveClose).toHaveCSS("opacity", "1");
  expect(Math.abs(((await inactiveTab.boundingBox())?.width ?? 0) - tabWidthBeforeHover)).toBeLessThanOrEqual(0.5);

  // The tab body is the only hovered surface: no descendant may paint its own
  // fill or state layer over it, which is what produced the inset rounded patch.
  const paintedInnerSurfaces = await inactiveTab.evaluate((tab) =>
    Array.from(tab.querySelectorAll<HTMLElement>("*")).filter((element) => {
      if (element.closest(".workbench-pane-tab__close")) return false;
      const opaque = (color: string) => color !== "rgba(0, 0, 0, 0)" && color !== "transparent";
      const layer = getComputedStyle(element, "::after");
      return (layer.content !== "none" && opaque(layer.backgroundColor))
        || opaque(getComputedStyle(element).backgroundColor);
    }).length);
  expect(paintedInnerSurfaces).toBe(0);

  const activeClose = tabRail.locator(".df-tab[data-active='true'] .df-tab-close");
  await expect(activeClose).toHaveCSS("opacity", "1");
  await expect(activeClose).toHaveCSS("pointer-events", "auto");
}

/** The packed state: enough terminals that every tab sits on the width floor.
 *  This is where the reported defect lived — tabs painting over the + button — so the
 *  + and chevron rects are measured at both scroll extremes, not just at rest. */
export async function expectWorkbenchTabStripAtFloor(page: Page): Promise<void> {
  const paneStripShell = page.locator("[data-pane-strip].workbench-pane-tab-strip").first();
  const packed = await paneStripShell.evaluate((shell) => {
    const rail = shell.querySelector<HTMLElement>(":scope > .workbench-pane-tab-strip__tabs");
    const newTab = shell.querySelector<HTMLElement>("[data-testid='tab-new']");
    const chevron = shell.querySelector<HTMLElement>("[data-testid='tab-overflow']");
    const activeTab = rail?.querySelector<HTMLElement>(".df-tab[data-active='true']");
    const inactiveTab = rail?.querySelector<HTMLElement>(".df-tab[data-active='false']");
    const tabs = rail ? Array.from(rail.querySelectorAll<HTMLElement>(".df-tab")) : [];
    if (!rail || !newTab || !chevron || !activeTab || !inactiveTab || tabs.length === 0) {
      throw new Error("packed workbench tab strip is incomplete");
    }

    const toRect = (element: HTMLElement) => {
      const rect = element.getBoundingClientRect();
      return { left: rect.left, right: rect.right, top: rect.top, bottom: rect.bottom };
    };
    const intersects = (
      first: { left: number; right: number; top: number; bottom: number },
      second: { left: number; right: number; top: number; bottom: number },
    ) => first.left < second.right
      && first.right > second.left
      && first.top < second.bottom
      && first.bottom > second.top;
    const clippedTabRects = (railRect: { left: number; right: number }) => tabs.map(toRect).map((rect) => ({
      ...rect,
      left: Math.max(rect.left, railRect.left),
      right: Math.min(rect.right, railRect.right),
    })).filter((rect) => rect.left < rect.right);

    const newTabRectAtStart = toRect(newTab);
    const chevronRectAtStart = toRect(chevron);
    const tabRectsAtStart = clippedTabRects(toRect(rail));
    rail.scrollLeft = rail.scrollWidth;
    const newTabRectAtEnd = toRect(newTab);
    const chevronRectAtEnd = toRect(chevron);
    const tabRectsAtEnd = clippedTabRects(toRect(rail));
    rail.scrollLeft = 0;

    // A custom property reads back as its unresolved calc() text, so the floor is
    // read from the computed min-inline-size the token feeds.
    const floor = Number.parseFloat(getComputedStyle(activeTab).minInlineSize);
    const display = (element: Element | null) => element ? getComputedStyle(element).display : "missing";
    return {
      floor,
      widthsAtFloor: tabs.every((tab) => {
        const rect = toRect(tab);
        return Math.abs((rect.right - rect.left) - floor) <= 0.5;
      }),
      railScrolls: rail.scrollWidth > rail.clientWidth + 1,
      shellScrolls: shell.scrollWidth > shell.clientWidth + 1,
      newTabIntersectsTabAtStart: tabRectsAtStart.some((tab) => intersects(newTabRectAtStart, tab)),
      newTabIntersectsTabAtEnd: tabRectsAtEnd.some((tab) => intersects(newTabRectAtEnd, tab)),
      newTabRectAtStart,
      newTabRectAtEnd,
      chevronRectAtStart,
      chevronRectAtEnd,
      chevronVisible: chevronRectAtStart.right > chevronRectAtStart.left
        && getComputedStyle(chevron).visibility === "visible",
      labelsHidden: tabs.every((tab) => display(tab.querySelector(".df-tab-label")) === "none"),
      inactiveCloseDisplay: display(inactiveTab.querySelector(".df-tab-close")),
      activeCloseDisplay: display(activeTab.querySelector(".df-tab-close")),
    };
  });

  expect(packed.floor).toBeGreaterThan(0);
  expect(packed.widthsAtFloor).toBe(true);
  expect(packed.railScrolls).toBe(true);
  expect(packed.shellScrolls).toBe(false);
  expect(packed.newTabIntersectsTabAtStart).toBe(false);
  expect(packed.newTabIntersectsTabAtEnd).toBe(false);
  expect(packed.newTabRectAtEnd).toEqual(packed.newTabRectAtStart);
  expect(packed.chevronVisible).toBe(true);
  expect(packed.chevronRectAtEnd).toEqual(packed.chevronRectAtStart);
  expect(packed.labelsHidden).toBe(true);
  expect(packed.inactiveCloseDisplay).toBe("none");
  expect(packed.activeCloseDisplay).not.toBe("none");
}
