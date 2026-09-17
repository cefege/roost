// Exercises the canonical desktop workbench against a real coordinator, worker, keeper, and PTY.
// The scenarios cover geometry, the retained Folders/Agents selector, and narrow tab/deck behavior.
// They use the terminal smoke fixtures without synthetic clients or renderers.

import type { Page } from "@playwright/test";
import { test, expect } from "./fixtures.ts";
import {
  navigateToSmokeSession,
  pressPlatformShortcut,
  spawnSmokeShell,
} from "./terminal-helpers.ts";
import { readRenderedLayout } from "./layout-document-snapshots.ts";
import { expectStatusTruth } from "./workbench-status.ts";
import { exerciseSidebarAgents } from "./workbench-shell-interactions.ts";
import { expectConnectedWorkbenchTabStrip } from "./workbench-shell-tab-strip.ts";

const WIDE_VIEWPORT = { width: 1440, height: 900 } as const;
const NARROW_VIEWPORT = { width: 1024, height: 768 } as const;
const SIDEBAR_WIDTH_DEFAULT = 300;
type Rect = { x: number; y: number; width: number; height: number; top: number; right: number; bottom: number };
type ShellGeometry = {
  viewport: { width: number; height: number }; domOrder: string[];
  title: Rect | null; rail: Rect | null; sidebar: Rect | null; editor: Rect | null;
  status: Rect | null; strip: Rect | null; visibleRows: Rect[];
};
async function readShellGeometry(page: Page, sessionId: string): Promise<ShellGeometry> {
  return page.evaluate((id) => {
    const shell = document.querySelector<HTMLElement>(".workbench-shell");
    const slot = document.querySelector<HTMLElement>(`[data-testid="terminal-slot-${CSS.escape(id)}"]`);
    const names = (element: Element): string => {
      if (element.classList.contains("workbench-titlebar")) return "title";
      if (element.classList.contains("workbench-activity-bar")) return "rail";
      if (element.classList.contains("workbench-sidebar-region")) return "sidebar";
      if (element.classList.contains("workbench-editor-region")) return "editor";
      return element.classList.contains("workbench-status-bar") ? "status" : "other";
    };
    const rect = (element: Element | null): Rect | null => {
      if (!(element instanceof HTMLElement)) return null;
      const box = element.getBoundingClientRect();
      return { x: box.x, y: box.y, width: box.width, height: box.height, top: box.top, right: box.right, bottom: box.bottom };
    };
    const slotRect = rect(slot);
    const visibleRows = Array.from(slot?.querySelectorAll<HTMLElement>(".cell-row") ?? []).map(rect)
      .filter((row): row is Rect => row !== null && row.width > 0 && row.height > 0
        && !!slotRect && row.top < slotRect.bottom && row.bottom > slotRect.top);
    return {
      viewport: { width: window.innerWidth, height: window.innerHeight }, domOrder: shell ? Array.from(shell.children).map(names) : [],
      title: rect(document.querySelector(".workbench-titlebar")), rail: rect(document.querySelector(".workbench-activity-bar")),
      sidebar: rect(document.querySelector(".workbench-sidebar-region")), editor: rect(document.querySelector(".workbench-editor-region")),
      status: rect(document.querySelector(".workbench-status-bar")), strip: rect(document.querySelector("[data-pane-strip]")), visibleRows,
    };
  }, sessionId);
}
async function expectDesktopGeometry(page: Page, sessionId: string, viewport: { width: number; height: number }): Promise<void> {
  await expect.poll(() => readShellGeometry(page, sessionId)).toMatchObject({ viewport, domOrder: ["title", "rail", "sidebar", "editor", "status"] });
  const geometry = await readShellGeometry(page, sessionId);
  const { title, rail, sidebar, editor, status, strip, visibleRows } = geometry;
  if (!title || !rail || !sidebar || !editor || !status || !strip) throw new Error("desktop workbench did not expose every measured region");
  expect(title.height).toBe(35); expect(rail.width).toBe(48); expect(status.height).toBe(22);
  expect(title.top).toBe(0); expect(rail.top).toBe(title.bottom); expect(editor.top).toBe(title.bottom);
  expect(strip.height).toBe(35); expect(strip.top).toBe(editor.top);
  expect(status.bottom).toBe(viewport.height); expect(status.top).toBe(viewport.height - 22);
  expect(sidebar.x).toBe(48); expect(editor.x).toBeGreaterThan(sidebar.x); expect(editor.right).toBe(viewport.width);
  expect(visibleRows.length).toBeGreaterThan(0);
  for (const row of visibleRows) { expect(row.top).toBeGreaterThanOrEqual(strip.bottom - 1); expect(row.bottom).toBeLessThanOrEqual(status.top + 1); }
}
async function expectCollapsedSidebarGeometry(page: Page, sessionId: string): Promise<void> {
  await expect.poll(async () => (await readShellGeometry(page, sessionId)).sidebar?.width ?? -1).toBe(0);
  const geometry = await readShellGeometry(page, sessionId);
  const { rail, sidebar, editor } = geometry;
  if (!rail || !sidebar || !editor) throw new Error("collapsed workbench lost a measured region");
  expect(sidebar.width).toBe(0); expect(Math.abs(editor.x - rail.right)).toBeLessThanOrEqual(1);
}
async function typeTrustedMarker(page: Page, sessionId: string, marker: string): Promise<void> {
  const slot = page.getByTestId(`terminal-slot-${sessionId}`);
  await slot.getByTestId("terminal-display").click();
  await expect(slot).toHaveAttribute("data-focused", "true");
  await page.keyboard.type(`printf '%s\\n' ${marker}`); await page.keyboard.press("Enter");
  await expect.poll(() => slot.textContent(), { timeout: 30_000 }).toContain(marker);
}
function tabWrapper(page: Page, sessionId: string) { return page.locator(`.df-tab[data-testid="tab-${sessionId}"]`).first(); }
async function dragTab(page: Page, sessionId: string, destination: { x: number; y: number }): Promise<void> {
  const select = tabWrapper(page, sessionId).locator(".workbench-pane-tab__select");
  const box = await select.boundingBox();
  if (!box) throw new Error(`tab ${sessionId} has no selectable bounds`);
  await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
  await page.mouse.down();
  await page.mouse.move(destination.x, destination.y, { steps: 8 });
  await page.mouse.up();
}

test("desktop workbench keeps measured geometry, status truth, and navigation", async ({
  smokePage,
  stack,
}, testInfo) => {
  test.skip(testInfo.project.name !== "chromium-desktop", "Chromium desktop workbench contract");
  await smokePage.setViewportSize(WIDE_VIEWPORT);
  const sessionId = (await spawnSmokeShell(smokePage, stack.workerFp)).session_id;
  await navigateToSmokeSession(smokePage, sessionId);
  const trustedMarker = `WB_WIDE_${crypto.randomUUID().replaceAll("-", "")}`;
  await typeTrustedMarker(smokePage, sessionId, trustedMarker);
  await expectDesktopGeometry(smokePage, sessionId, WIDE_VIEWPORT);
  await expectStatusTruth(smokePage, stack);
  const coordIdentity = await stack.client.authCoordIdentity({});
  await expect(smokePage.getByTestId("workbench-status-revision")).toHaveText(coordIdentity.gitSha.slice(0, 7));

  const sessions = smokePage.getByTestId("workbench-activity-sessions");
  const search = smokePage.getByTestId("workbench-activity-search");
  const files = smokePage.getByTestId("workbench-activity-files");
  const settings = smokePage.getByTestId("workbench-activity-settings");
  const help = smokePage.getByTestId("workbench-activity-help");
  await expect(sessions).toHaveAttribute("aria-current", "page");
  await expect(search).toHaveAttribute("href", "/search");
  await expect(files).toHaveAttribute("href", "/browse");
  await expect(settings).toHaveAttribute("href", "/settings/machines");
  await expect(smokePage.getByTestId("all-view")).toHaveCSS("overflow-x", "hidden");
  await expect(smokePage.getByTestId("sidebar-search")).toBeVisible();
  await expect(help).toHaveAttribute("href", "/help");

  const originalUrl = smokePage.url();
  const desktopSidebar = smokePage.getByTestId("sidebar-desktop");
  const terminalSlot = smokePage.getByTestId(`terminal-slot-${sessionId}`);
  const terminalPaneId = await terminalSlot.getAttribute("data-pane-id");
  if (!terminalPaneId) throw new Error("terminal slot did not expose a pane id");
  await smokePage.getByTestId("sidebar-search").focus();
  await pressPlatformShortcut(smokePage, "toggleSidebar", "b");
  await expect(smokePage).toHaveURL(originalUrl);
  await expect(desktopSidebar).toHaveAttribute("data-collapsed", "true");
  await expect(sessions).toBeFocused();
  await sessions.click();
  await expect(desktopSidebar).toHaveAttribute("data-collapsed", "false");

  const resizer = smokePage.getByTestId("sidebar-resizer");
  await resizer.focus();
  await pressPlatformShortcut(smokePage, "toggleSidebar", "b");
  await expect(smokePage).toHaveURL(originalUrl);
  await expect(desktopSidebar).toHaveAttribute("data-collapsed", "true");
  await expect(sessions).toBeFocused();
  await expect(terminalSlot).toBeVisible();
  await expect(terminalSlot).toHaveAttribute("data-pane-id", terminalPaneId);
  await expect(terminalSlot).toContainText(trustedMarker);
  await expectCollapsedSidebarGeometry(smokePage, sessionId);
  await sessions.click();
  await expect(desktopSidebar).toHaveAttribute("data-collapsed", "false");
  const initialWidth = Number(await resizer.getAttribute("aria-valuenow"));
  expect(initialWidth).toBe(SIDEBAR_WIDTH_DEFAULT);
  await resizer.focus();
  await smokePage.keyboard.press("ArrowRight");
  await expect(resizer).toHaveAttribute("aria-valuenow", String(initialWidth + 10));
  await smokePage.reload({ waitUntil: "domcontentloaded" });
  await navigateToSmokeSession(smokePage, sessionId);
  await expect(smokePage.getByTestId("sidebar-resizer")).toHaveAttribute("aria-valuenow", String(initialWidth + 10));
  await smokePage.getByTestId("sidebar-resizer").dblclick();
  await expect(smokePage.getByTestId("sidebar-resizer")).toHaveAttribute("aria-valuenow", String(SIDEBAR_WIDTH_DEFAULT));
  await smokePage.reload({ waitUntil: "domcontentloaded" });
  await navigateToSmokeSession(smokePage, sessionId);
  await expect(smokePage.getByTestId("sidebar-resizer")).toHaveAttribute("aria-valuenow", String(SIDEBAR_WIDTH_DEFAULT));

  await expect(smokePage.locator(".workbench-command-center")).toHaveCount(0);
  await pressPlatformShortcut(smokePage, "commandPalette", "k");
  await expect(smokePage.getByTestId("command-palette")).toBeVisible();
  await smokePage.keyboard.press("Escape");
  await expect(smokePage.getByTestId("command-palette")).toHaveCount(0);

  await search.click();
  await expect(smokePage).toHaveURL(`${stack.baseUrl}/search`);
  await expect(search).toHaveAttribute("aria-current", "page");
  await help.click();
  await expect(smokePage).toHaveURL(`${stack.baseUrl}/help`);
  await expect(smokePage.locator(".workbench-help")).toBeVisible();
  await navigateToSmokeSession(smokePage, sessionId);
});
test("desktop sidebar keeps Folders and Agents independently navigable", async ({
  multiWorkerSmokePage,
  stack,
  secondWorker,
}, testInfo) => {
  test.skip(testInfo.project.name !== "chromium-desktop", "Chromium desktop Folders/Agents contract");
  test.setTimeout(180_000);
  await exerciseSidebarAgents(multiWorkerSmokePage, stack, secondWorker);
});
test("narrow desktop keeps tab wrappers, overflow controls, and tile drops coherent", async ({
  smokePage,
  stack,
}, testInfo) => {
  test.skip(testInfo.project.name !== "chromium-desktop", "Chromium desktop tab/deck contract");
  await smokePage.addInitScript(() => localStorage.removeItem("roost.paneLayout.v1"));
  await smokePage.reload({ waitUntil: "domcontentloaded" });
  await smokePage.waitForFunction((workerFp) => !!window.__smoke?.state().workers[workerFp], stack.workerFp);
  await smokePage.setViewportSize(NARROW_VIEWPORT);
  const createdIds: string[] = [(await spawnSmokeShell(smokePage, stack.workerFp)).session_id];
  for (let index = 1; index < 6; index++) {
    createdIds.push((await spawnSmokeShell(smokePage, stack.workerFp)).session_id);
  }
  await navigateToSmokeSession(smokePage, createdIds[0]!);
  await expect(smokePage.locator("[data-pane-slot]")).toHaveCount(1);
  await typeTrustedMarker(smokePage, createdIds[0]!, `WB_NARROW_${crypto.randomUUID().replaceAll("-", "")}`);
  await expectDesktopGeometry(smokePage, createdIds[0]!, NARROW_VIEWPORT);
  await expectConnectedWorkbenchTabStrip(smokePage);

  const initial = await readRenderedLayout(smokePage, createdIds);
  const initialOrder = initial.panes[0]?.tabs ?? [];
  expect(initialOrder).toHaveLength(createdIds.length);
  expect(new Set(initialOrder)).toEqual(new Set(createdIds));
  for (const id of initialOrder) {
    const wrapper = tabWrapper(smokePage, id);
    await expect(wrapper).toHaveAttribute("data-testid", `tab-${id}`);
    await expect(wrapper.locator(".workbench-pane-tab__select")).toBeVisible();
  }
  const selectedId = initialOrder[1] ?? initialOrder[0]!;
  await tabWrapper(smokePage, selectedId).locator(".workbench-pane-tab__select").click();
  await expect(tabWrapper(smokePage, selectedId)).toHaveAttribute("data-active", "true");
  await expect(smokePage).toHaveURL(`${stack.baseUrl}/s/${selectedId}`);
  const closedId = initialOrder.at(-1)!;
  const closedTab = tabWrapper(smokePage, closedId);
  const closedTabClose = closedTab.locator(".df-tab-close");
  await closedTab.hover();
  await expect(closedTabClose).toBeVisible();
  await closedTabClose.click();
  await expect(closedTab).toHaveCount(0);
  await expect(smokePage).toHaveURL(`${stack.baseUrl}/s/${selectedId}`);
  const undo = smokePage.getByTestId("undo-snackbar-action");
  await expect(undo).toBeVisible();
  await undo.click();
  await expect(tabWrapper(smokePage, closedId)).toBeVisible();
  const beforeReorder = (await readRenderedLayout(smokePage, createdIds)).panes[0]?.tabs ?? [];
  const movedId = beforeReorder[0]!;
  const lastId = beforeReorder.at(-1)!;
  const lastBox = await tabWrapper(smokePage, lastId).boundingBox();
  if (!lastBox) throw new Error("tab reorder target has no bounds");
  await dragTab(smokePage, movedId, {
    x: lastBox.x + lastBox.width - 3,
    y: lastBox.y + lastBox.height / 2,
  });
  const expectedReorder = [...beforeReorder.slice(1), movedId];
  await expect.poll(async () =>
    (await readRenderedLayout(smokePage, createdIds)).panes[0]?.tabs ?? [],
  ).toEqual(expectedReorder);
  await expect.poll(() => readRenderedLayout(smokePage, createdIds)).toMatchObject({
    panes: [{ selected: selectedId }],
  });

  const overflow = smokePage.getByTestId("tab-overflow");
  await expect(overflow).toBeVisible();
  await overflow.click();
  const popup = smokePage.getByTestId("tab-list-popup");
  const filter = smokePage.getByTestId("tab-list-filter");
  await expect(popup).toBeVisible();
  const overflowRect = await overflow.boundingBox();
  const popupRect = await popup.boundingBox();
  if (!overflowRect || !popupRect) throw new Error("overflow menu lacks a right anchor");
  expect(Math.abs(popupRect.x + popupRect.width - (overflowRect.x + overflowRect.width))).toBeLessThanOrEqual(1);
  const firstLabel = (await popup.locator('[data-testid^="tab-list-item-"] .workbench-tab-list__item-label').first().textContent())?.trim();
  if (!firstLabel) throw new Error("overflow filter has no terminal label");
  await filter.fill("__roost_no_matching_terminal__");
  await expect(popup.locator(".workbench-tab-list__empty")).toHaveText("No matches");
  await expect(popup.locator('[data-testid^="tab-list-item-"]')).toHaveCount(0);
  await filter.press("Enter");
  await expect(popup).toBeVisible();
  await expect(smokePage).toHaveURL(`${stack.baseUrl}/s/${selectedId}`);
  await filter.fill(firstLabel);
  const matchingItem = popup.locator('[data-testid^="tab-list-item-"]').first();
  await expect(matchingItem).toBeVisible();
  await filter.press("ArrowDown");
  await filter.press("ArrowUp");
  const highlightedItem = popup.locator('[data-testid^="tab-list-item-"][data-highlighted="true"]');
  const chosenId = (await highlightedItem.getAttribute("data-testid"))?.slice("tab-list-item-".length);
  if (!chosenId) throw new Error("overflow item has no stable session id");
  await filter.press("Enter");
  await expect(popup).toHaveCount(0);
  await expect(tabWrapper(smokePage, chosenId).locator(".workbench-pane-tab__select")).toBeFocused();

  await overflow.click();
  await expect(popup).toBeVisible();
  await filter.press("Escape");
  await expect(popup).toHaveCount(0);
  await expect(overflow).toBeFocused();

  await pressPlatformShortcut(smokePage, "splitRight", "D");
  await expect(smokePage.locator("[data-pane-slot]")).toHaveCount(2);
  const splitSessionIds = await smokePage.locator("[data-pane-slot]").evaluateAll((slots) =>
    slots.flatMap((slot) => {
      const testId = slot.getAttribute("data-testid") ?? "";
      return testId.startsWith("terminal-slot-") ? [testId.slice("terminal-slot-".length)] : [];
    }),
  );
  for (const splitSessionId of splitSessionIds.filter((id) => !createdIds.includes(id))) {
    await smokePage.evaluate((id) => window.__smoke.trackCreatedSession(id), splitSessionId);
  }
  const splitLayout = await readRenderedLayout(smokePage, createdIds);
  const sourcePaneIndex = splitLayout.panes.findIndex((pane) => pane.tabs.length > 1);
  const sourcePane = sourcePaneIndex >= 0
    ? splitLayout.panes[sourcePaneIndex]
    : splitLayout.panes[0];
  const targetPaneIndex = splitLayout.panes.findIndex((pane) => pane !== sourcePane);
  const targetPane = targetPaneIndex >= 0 ? splitLayout.panes[targetPaneIndex] : undefined;
  if (!sourcePane || !targetPane || !sourcePane.tabs[0]) {
    throw new Error("split did not produce source and target tab panes");
  }
  const sourceId = sourcePane.tabs[0];
  const targetStrip = smokePage.locator("[data-pane-strip]").nth(targetPaneIndex);
  const targetStripBox = await targetStrip.boundingBox();
  if (!targetStripBox) throw new Error("tile target strip has no bounds");
  await dragTab(smokePage, sourceId, {
    x: targetStripBox.x + targetStripBox.width / 2,
    y: targetStripBox.y + targetStripBox.height + 2,
  });
  await expect(smokePage.locator(".workbench-pane-drop-overlay")).toHaveCount(0);
  const tiled = await readRenderedLayout(smokePage, createdIds);
  expect(tiled.panes.flatMap((pane) => pane.tabs)).toContain(sourceId);
  expect(tiled.panes.length).toBe(3);
  const sourcePaneAfter = tiled.panes.findIndex((pane) => pane.tabs.includes(sourceId));
  expect(sourcePaneAfter).not.toBe(sourcePaneIndex);
});
