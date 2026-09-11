// Workbench tab strip browser assertions own the nested rail and fixed-action contract.
// workbench-shell.spec.ts calls this after creating six real terminal sessions.
// The helper reads rendered geometry only; session and layout operations stay in the scenario.
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
    const editor = shell.closest<HTMLElement>(".workbench-editor-region");
    const arrange = document.querySelector<HTMLElement>("[data-testid='arrange-btn']");
    if (!rail || !actions || !activeTab || !inactiveTab || !editor || !arrange) {
      throw new Error("workbench tab shell is incomplete");
    }

    const toRect = (element: HTMLElement) => {
      const rect = element.getBoundingClientRect();
      return { left: rect.left, right: rect.right, top: rect.top, bottom: rect.bottom };
    };
    const railRect = toRect(rail);
    const actionRects = Array.from(actions.querySelectorAll<HTMLElement>("button")).map(toRect);
    const visibleTabRects = Array.from(rail.querySelectorAll<HTMLElement>(".df-tab"))
      .map(toRect)
      .map((rect) => ({
        ...rect,
        left: Math.max(rect.left, railRect.left),
        right: Math.min(rect.right, railRect.right),
      }))
      .filter((rect) => rect.left < rect.right);
    const intersects = (
      first: { left: number; right: number; top: number; bottom: number },
      second: { left: number; right: number; top: number; bottom: number },
    ) => first.left < second.right
      && first.right > second.left
      && first.top < second.bottom
      && first.bottom > second.top;
    const actionRectsBeforeScroll = actionRects;
    rail.scrollLeft = rail.scrollWidth;
    const actionRectsAfterScroll = Array.from(actions.querySelectorAll<HTMLElement>("button")).map(toRect);
    rail.scrollLeft = 0;
    const activeIcon = activeTab.querySelector<HTMLElement>(".workbench-pane-tab__icon");
    const inactiveIcon = inactiveTab.querySelector<HTMLElement>(".workbench-pane-tab__icon");
    const activeLabel = activeTab.querySelector<HTMLElement>(".workbench-pane-tab__label");
    const inactiveLabel = inactiveTab.querySelector<HTMLElement>(".workbench-pane-tab__label");
    const inactiveSelect = inactiveTab.querySelector<HTMLElement>(".workbench-pane-tab__select");
    return {
      shellScrolls: shell.scrollWidth > shell.clientWidth + 1,
      railScrolls: rail.scrollWidth > rail.clientWidth + 1,
      actionsAreSibling: actions.parentElement === shell,
      actionRectsBeforeScroll,
      actionRectsAfterScroll,
      actionIntersectsTab: actionRects.some((action) => visibleTabRects.some((tab) => intersects(action, tab))),
      arrangeIntersectsAction: actionRects.some((action) => intersects(toRect(arrange), action)),
      activeRadius: getComputedStyle(activeTab).borderTopLeftRadius,
      activeBackground: getComputedStyle(activeTab).backgroundColor,
      editorBackground: getComputedStyle(editor).backgroundColor,
      inactiveSelectRadius: inactiveSelect ? getComputedStyle(inactiveSelect).borderTopLeftRadius : "",
      activeIconColor: activeIcon ? getComputedStyle(activeIcon).color : "",
      inactiveIconColor: inactiveIcon ? getComputedStyle(inactiveIcon).color : "",
      activeLabelColor: activeLabel ? getComputedStyle(activeLabel).color : "",
      inactiveLabelColor: inactiveLabel ? getComputedStyle(inactiveLabel).color : "",
    };
  });
  expect(tabStripLayout.shellScrolls).toBe(false);
  expect(tabStripLayout.railScrolls).toBe(true);
  expect(tabStripLayout.actionsAreSibling).toBe(true);
  expect(tabStripLayout.actionIntersectsTab).toBe(false);
  expect(tabStripLayout.arrangeIntersectsAction).toBe(false);
  expect(tabStripLayout.activeRadius).toBe("0px");
  expect(tabStripLayout.inactiveSelectRadius).toBe("0px");
  expect(tabStripLayout.activeBackground).toBe(tabStripLayout.editorBackground);
  expect(tabStripLayout.inactiveIconColor).not.toBe(tabStripLayout.activeIconColor);
  expect(tabStripLayout.inactiveLabelColor).not.toBe(tabStripLayout.activeLabelColor);
  expect(tabStripLayout.actionRectsAfterScroll).toEqual(tabStripLayout.actionRectsBeforeScroll);
  expect(await tabRail.evaluate((rail) => rail.scrollLeft)).toBe(0);
}
