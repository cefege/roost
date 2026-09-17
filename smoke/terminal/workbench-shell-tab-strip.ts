// Workbench tab strip browser assertions own the nested rail and overflow-action contract.
// workbench-shell.spec.ts calls this after creating six real terminal sessions.
// The helper measures rendered states, probes unfocused state, and drives tab hover.
// Session and layout operations stay in the scenario.
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
    const newTab = rail?.querySelector<HTMLElement>("[data-testid='tab-new']");
    const editor = shell.closest<HTMLElement>(".workbench-editor-region");
    const arrange = document.querySelector<HTMLElement>("[data-testid='arrange-btn']");
    const tabs = rail ? Array.from(rail.querySelectorAll<HTMLElement>(".df-tab")) : [];
    const lastTab = tabs.at(-1);
    if (!rail || !actions || !activeTab || !inactiveTab || !newTab || !lastTab || !editor || !arrange) {
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

    const railRectAtStart = toRect(rail);
    const newTabRectAtStart = toRect(newTab);
    const actionRectsBeforeScroll = Array.from(actions.querySelectorAll<HTMLElement>("button")).map(toRect);
    const visibleTabRects = tabs.map(toRect).map((rect) => ({
      ...rect,
      left: Math.max(rect.left, railRectAtStart.left),
      right: Math.min(rect.right, railRectAtStart.right),
    })).filter((rect) => rect.left < rect.right);
    rail.scrollLeft = rail.scrollWidth;
    const railRectAtEnd = toRect(rail);
    const newTabRectAtEnd = toRect(newTab);
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
    const inactiveClose = inactiveTab.querySelector<HTMLElement>(".df-tab-close");
    const stripRect = shell.getBoundingClientRect();
    return {
      shellScrolls: shell.scrollWidth > shell.clientWidth + 1,
      railScrolls: rail.scrollWidth > rail.clientWidth + 1,
      actionsAreSibling: actions.parentElement === shell,
      newTabFollowsLastTab: lastTab.nextElementSibling === newTab,
      newTabWithinRailAtStart: newTabRectAtStart.left >= railRectAtStart.left
        && newTabRectAtStart.right <= railRectAtStart.right
        && newTabRectAtStart.top >= railRectAtStart.top
        && newTabRectAtStart.bottom <= railRectAtStart.bottom,
      newTabWithinRailAtEnd: newTabRectAtEnd.left >= railRectAtEnd.left
        && newTabRectAtEnd.right <= railRectAtEnd.right
        && newTabRectAtEnd.top >= railRectAtEnd.top
        && newTabRectAtEnd.bottom <= railRectAtEnd.bottom,
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
      sizesMatchTokens: minSize === tokenSize * 3 && maxSize === tokenSize * 5,
      widthsWithinRange: tabs.every((tab) => {
        const rect = toRect(tab);
        const width = rect.right - rect.left;
        return width >= minSize && width <= maxSize;
      }),
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
  expect(tabStripLayout.railScrolls).toBe(true);
  expect(tabStripLayout.actionsAreSibling).toBe(true);
  expect(tabStripLayout.newTabFollowsLastTab).toBe(true);
  expect(tabStripLayout.newTabWithinRailAtStart).toBe(true);
  expect(tabStripLayout.newTabWithinRailAtEnd).toBe(true);
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

  // Width stability only bites on a tab sized by its own content: every session
  // in this scenario rests at the 3×--md-space-8 floor, where the label absorbs
  // the reserved close slot instead of widening the tab. Retitle one PTY so its
  // tab sizes to content — the state the hover-time reflow was reported in.
  const contentSizedId = (await inactiveTab.getAttribute("data-testid"))?.replace("tab-", "");
  if (!contentSizedId) throw new Error("inactive workbench tab carries no session id");
  await page.mouse.move(0, 0);
  await page.evaluate(
    (id) => window.__smoke.input(id, "printf '\\033]0;wb-tab-sizing-fix\\007'\n"),
    contentSizedId,
  );
  await expect(label).toHaveText("wb-tab-sizing-fix");
  const contentSizedWidth = (await inactiveTab.boundingBox())?.width ?? 0;
  expect(contentSizedWidth).toBeGreaterThan(tabStripLayout.minSize);
  expect(contentSizedWidth).toBeLessThan(tabStripLayout.maxSize);
  await inactiveTab.hover();
  await expect(inactiveClose).toHaveCSS("opacity", "1");
  expect(Math.abs(((await inactiveTab.boundingBox())?.width ?? 0) - contentSizedWidth)).toBeLessThanOrEqual(0.5);

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
