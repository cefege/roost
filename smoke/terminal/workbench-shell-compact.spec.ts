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
  const drawer = mobileSmokePage.getByTestId("sidebar-drawer");
  await mobileSmokePage.getByTestId("mobile-deck-bar-menu").tap();
  await expect(drawer).toHaveAttribute("data-open", "true");
  await expect(mobileSmokePage.getByTestId("sidebar-overlay")).toHaveAttribute("data-open", "true");
  const sidebarSearch = mobileSmokePage.getByTestId("sidebar-search");
  await expect(mobileSmokePage.getByTestId("sidebar-view-spaces")).toHaveAttribute("aria-pressed", "true");
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
