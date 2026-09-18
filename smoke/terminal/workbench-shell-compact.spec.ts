// Exercises the compact workbench against a real coordinator, worker, keeper, and PTY.
// This spec owns drawer navigation and settings layout across compact viewport boundaries.
// It uses the terminal smoke fixtures and shared interaction primitives without synthetic clients.

import type { Page } from "@playwright/test";
import { test, expect } from "./fixtures.ts";
import {
  navigateToSmokeSession,
  spawnSmokeShell,
  switchToSmokeSession,
} from "./terminal-helpers.ts";
import { swipeFromEdge } from "./workbench-shell-interactions.ts";

const COMPACT_VIEWPORT = { width: 390, height: 844 } as const;
const COMPACT_LANDSCAPE_VIEWPORT = { width: 844, height: 390 } as const;
const SHORT_SIDE_BOUNDARY_VIEWPORT = { width: 844, height: 600 } as const;

async function typeTrustedMarker(page: Page, sessionId: string, marker: string): Promise<void> {
  const slot = page.getByTestId(`terminal-slot-${sessionId}`);
  await slot.getByTestId("terminal-display").click();
  await expect(slot).toHaveAttribute("data-focused", "true");
  await page.keyboard.type(`printf '%s\\n' ${marker}`); await page.keyboard.press("Enter");
  await expect.poll(() => slot.textContent(), { timeout: 30_000 }).toContain(marker);
}

test("compact workbench preserves drawer navigation and settings padding across boundary sizes", async ({
  mobileSmokePage,
  stack,
}) => {
  await mobileSmokePage.setViewportSize(COMPACT_VIEWPORT);
  await expect(mobileSmokePage.locator(".workbench-shell")).toHaveAttribute("data-compact", "true");
  const sessionId = (await spawnSmokeShell(mobileSmokePage, stack.workerFp)).session_id;
  await navigateToSmokeSession(mobileSmokePage, sessionId);
  await expect(mobileSmokePage.getByTestId("mobile-deck-bar")).toBeVisible();
  await expect(mobileSmokePage.getByTestId("mobile-topbar")).toHaveCount(0);
  await expect(mobileSmokePage.getByTestId("workbench-status-bar")).toHaveCount(0);
  await expect(mobileSmokePage.locator(".workbench-titlebar")).toHaveCount(0);
  await expect(mobileSmokePage.locator(".workbench-activity-bar")).toHaveCount(0);
  await expect(mobileSmokePage.getByTestId("mobile-deck-bar")).toContainText(/\S/);
  expect(await mobileSmokePage.evaluate(() => {
    const root = document.documentElement;
    const display = document.querySelector('[data-testid="terminal-display"]');
    return {
      documentScrollRange: root.scrollHeight - root.clientHeight,
      rootOverscrollY: getComputedStyle(root).overscrollBehaviorY,
      displayOverscrollY: display ? getComputedStyle(display).overscrollBehaviorY : null,
    };
  })).toEqual({
    documentScrollRange: 0,
    rootOverscrollY: "none",
    displayOverscrollY: "none",
  });
  const drawer = mobileSmokePage.getByTestId("sidebar-drawer");
  await mobileSmokePage.getByTestId("mobile-deck-bar-menu").tap();
  await expect(drawer).toHaveAttribute("data-open", "true");
  await expect(mobileSmokePage.getByTestId("sidebar-overlay")).toHaveAttribute("data-open", "true");
  const sidebarSearch = mobileSmokePage.getByTestId("sidebar-search");
  await expect(mobileSmokePage.getByTestId("sidebar-view-folders")).toHaveAttribute("aria-pressed", "true");
  await expect(sidebarSearch).toBeVisible();
  await expect(mobileSmokePage.getByTestId("folder-list")).toBeVisible();
  await sidebarSearch.fill("/tmp");
  await expect(mobileSmokePage.locator(
    `[data-testid="sidebar-session-row"][data-session-id="${sessionId}"]`,
  )).toBeVisible();
  await expect(mobileSmokePage.locator('[data-testid^="folder-row-"]')).toHaveCount(0);
  await sidebarSearch.fill("");
  await expect.poll(() => mobileSmokePage.locator('[data-testid^="folder-row-"]').count())
    .toBeGreaterThan(0);
  await expect(mobileSmokePage.getByTestId("sidebar-session-row")).toHaveCount(0);
  await expect(mobileSmokePage.getByTestId("folder-list")).toBeVisible();
  // The action bar lives in the sidebar grid's bottom row, so scrolling the
  // folders list must not move it and the scroller must not contain it.
  const actionBar = mobileSmokePage.getByTestId("sidebar-new-terminal");
  const foldersScroller = mobileSmokePage.getByTestId("all-view");
  await expect(foldersScroller.locator('[data-testid="sidebar-new-terminal"]')).toHaveCount(0);
  await expect(actionBar).toBeVisible();
  const pinnedBox = await actionBar.boundingBox();
  if (!pinnedBox) throw new Error("sidebar action bar has no layout box");
  for (const scrollTop of ["bottom", "top"] as const) {
    await foldersScroller.evaluate((element, edge) => {
      element.scrollTop = edge === "bottom" ? element.scrollHeight : 0;
    }, scrollTop);
    await expect(actionBar).toBeVisible();
    const scrolledBox = await actionBar.boundingBox();
    if (!scrolledBox) throw new Error("sidebar action bar lost its layout box while scrolling");
    expect(Math.abs(scrolledBox.y - pinnedBox.y)).toBeLessThanOrEqual(1);
  }
  // Compact rows drop the machine name and pane-count chip; the leading
  // MachineIdentityMark already names the machine for an online worker.
  const firstFolderRow = mobileSmokePage.locator('[data-testid^="folder-row-"]').first();
  await expect(firstFolderRow.locator(".df-flat-server")).toHaveCount(0);
  await expect(firstFolderRow.locator(".df-flat-path")).toHaveCount(0);
  await mobileSmokePage.getByTestId("brand-row-collapse").tap();
  await expect(drawer).toHaveAttribute("data-open", "false");
  await swipeFromEdge(mobileSmokePage, 1, 250, 420);
  await expect(drawer).toHaveAttribute("data-open", "true");
  await mobileSmokePage.getByTestId("brand-row-settings").tap();
  await expect(mobileSmokePage).toHaveURL(`${stack.baseUrl}/settings/devices`);
  await expect(drawer).toHaveAttribute("data-open", "false");
  await mobileSmokePage.goto(`${stack.baseUrl}/settings`, { waitUntil: "domcontentloaded" });
  const settingsRoot = mobileSmokePage.locator(".settings-mobile__main");
  await expect(mobileSmokePage.locator(".settings-rail")).toHaveCount(0);
  await expect(settingsRoot).toHaveCount(1);
  await expect(settingsRoot.locator(".settings-topbar")).toHaveCount(1);
  await expect(mobileSmokePage.getByTestId("mobile-topbar")).toHaveCount(0);
  const firstSettingsRow = settingsRoot.locator(".settings-mobile__row").first();
  await expect(firstSettingsRow).toBeVisible();
  await firstSettingsRow.click();
  await expect(mobileSmokePage).toHaveURL(/\/settings\/[^/]+$/);
  await expect(settingsRoot.locator(".settings-topbar")).toHaveCount(1);
  await mobileSmokePage.getByTestId("settings-detail-back").click();
  await expect(mobileSmokePage).toHaveURL(`${stack.baseUrl}/settings`);
  await expect(settingsRoot.locator(".settings-topbar")).toHaveCount(1);
  await switchToSmokeSession(mobileSmokePage, sessionId);
  await typeTrustedMarker(mobileSmokePage, sessionId, `WB_COMPACT_${crypto.randomUUID().replaceAll("-", "")}`);
  await mobileSmokePage.setViewportSize(COMPACT_LANDSCAPE_VIEWPORT);
  await expect(mobileSmokePage.locator(".workbench-shell")).toHaveAttribute("data-compact", "true");
  await navigateToSmokeSession(mobileSmokePage, sessionId);
  await expect(mobileSmokePage.getByTestId(`terminal-slot-${sessionId}`)).toBeVisible();
  await mobileSmokePage.goto(`${stack.baseUrl}/settings/devices`, { waitUntil: "domcontentloaded" });
  await expect(settingsRoot).toBeVisible();
  const compactSettingsContent = settingsRoot.locator(".settings-content");
  expect(await compactSettingsContent.evaluate((element) => {
    const style = getComputedStyle(element);
    return [style.paddingTop, style.paddingRight, style.paddingBottom, style.paddingLeft]
      .every((padding) => padding === style.getPropertyValue("--md-space-4").trim());
  })).toBe(true);
  await mobileSmokePage.setViewportSize({ width: 700, height: 700 });
  await expect(mobileSmokePage.locator(".workbench-shell")).toHaveAttribute("data-compact", "false");
  await expect(mobileSmokePage.getByTestId("workbench-activity-sessions")).toBeVisible();
  await expect(mobileSmokePage.getByTestId("sidebar-desktop")).toBeVisible();
  await expect(mobileSmokePage.getByTestId("folder-list")).toBeVisible();
  await expect(firstFolderRow.locator(".df-flat-server")).toHaveCount(1);
  await expect(firstFolderRow.locator(".df-flat-path")).toHaveCount(1);
  await mobileSmokePage.setViewportSize(SHORT_SIDE_BOUNDARY_VIEWPORT);
  await expect(mobileSmokePage.locator(".workbench-shell")).toHaveAttribute("data-compact", "false");
  await mobileSmokePage.goto(`${stack.baseUrl}/settings/machines`, { waitUntil: "domcontentloaded" });
  await expect(settingsRoot).toHaveCount(0);
  const desktopSettingsContent = mobileSmokePage.locator(".settings-main .settings-content");
  await expect(desktopSettingsContent).toBeVisible();
  expect(await desktopSettingsContent.evaluate((element) => {
    const style = getComputedStyle(element);
    const blockPadding = style.getPropertyValue("--md-space-6").trim();
    const inlinePadding = style.getPropertyValue("--md-space-7").trim();
    return [style.paddingTop, style.paddingBottom].every((padding) => padding === blockPadding)
      && [style.paddingRight, style.paddingLeft].every((padding) => padding === inlinePadding);
  })).toBe(true);
});

test("notification dock rides above the compact composer", async ({ mobileSmokePage, stack }) => {
  await mobileSmokePage.setViewportSize(COMPACT_VIEWPORT);
  const sessionId = (await spawnSmokeShell(mobileSmokePage, stack.workerFp)).session_id;
  await navigateToSmokeSession(mobileSmokePage, sessionId);
  const slot = mobileSmokePage.getByTestId(`terminal-slot-${sessionId}`);
  await slot.getByTestId("terminal-display").click();
  await expect(slot).toHaveAttribute("data-focused", "true");
  // The viewport composer mounts only for a focused compact pane, and the
  // pane-placement composer shares the class, so pin the placement.
  const composerSelector = '.term-chat__dock[data-placement="viewport"]';
  await expect(mobileSmokePage.locator(composerSelector)).toBeVisible();
  // The dock is always mounted and zero-height while empty, so its bottom edge
  // — where a card's bottom edge lands — is read straight off layout instead of
  // through visibility-gated boundingBox().
  const geometry = await mobileSmokePage.evaluate((selector) => {
    const dock = document.querySelector('[data-testid="notification-dock"]');
    const composer = document.querySelector(selector);
    if (!dock || !composer) return null;
    const dockRect = dock.getBoundingClientRect();
    const composerRect = composer.getBoundingClientRect();
    return {
      dockBottom: dockRect.bottom,
      dockWidth: Math.round(dockRect.width),
      composerTop: composerRect.top,
      composerHeight: Math.round(composerRect.height),
    };
  }, composerSelector);
  if (!geometry) throw new Error("dock or composer is not mounted on the compact terminal route");
  expect(geometry.composerHeight).toBeGreaterThan(0);
  expect(geometry.dockBottom).toBeLessThanOrEqual(geometry.composerTop + 1);
  // 24 = 2 * --md-space-3, the dock's own inline gutters.
  expect(geometry.dockWidth).toBe(COMPACT_VIEWPORT.width - 24);
});

test("compact deck badge counts the active terminal's position", async ({ mobileSmokePage, stack }) => {
  await mobileSmokePage.setViewportSize(COMPACT_VIEWPORT);
  const first = (await spawnSmokeShell(mobileSmokePage, stack.workerFp)).session_id;
  await navigateToSmokeSession(mobileSmokePage, first);

  // A swipe can mount a second deck bar, so the badge is scoped to the primary
  // strip instead of matching whichever bar happens to render first.
  const strip = mobileSmokePage.getByTestId("mobile-strip-wrap");
  const badge = strip.getByTestId("mobile-tab-count");
  await expect(badge).toHaveText("1");

  const existingIds = await mobileSmokePage.evaluate(() => Object.keys(window.__smoke.state().sessions));
  await strip.getByTestId("tab-new").tap();
  await mobileSmokePage.waitForFunction(
    (existing) => Object.keys(window.__smoke.state().sessions).some((id) => !existing.includes(id)),
    existingIds,
    { timeout: 30_000 },
  );
  const siblingId = await mobileSmokePage.evaluate(
    (existing) => Object.keys(window.__smoke.state().sessions).find((id) => !existing.includes(id))!,
    existingIds,
  );
  // Hand the spawn to the fixture's cleanup; the product spawned it, not the harness.
  await mobileSmokePage.evaluate((id) => window.__smoke.trackCreatedSession(id), siblingId);

  // The sibling lands last in flatTabs order and is the painted terminal.
  await expect(badge).toHaveText("2/2");
  await switchToSmokeSession(mobileSmokePage, first);
  await expect(badge).toHaveText("1/2");
  await expect(badge).toHaveAttribute("aria-label", "Open terminal grid — terminal 1 of 2 in this workspace");
});
