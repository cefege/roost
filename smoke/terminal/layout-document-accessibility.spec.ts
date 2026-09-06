// Exercises portable-layout controls through real keyboard and compact surfaces.
// One live PTY pins one-session availability, menu focus semantics, and the
// compact workspace-sheet path into the same local import preview.

import { test, expect } from "./fixtures.ts";
import { navigateToSmokeSession } from "./terminal-helpers.ts";

test("layout transfer controls are keyboard and compact reachable", async ({
  smokePage,
  stack,
}, testInfo) => {
  test.skip(testInfo.project.name !== "chromium-desktop", "Chromium layout control contract");
  await smokePage.context().grantPermissions(["clipboard-read", "clipboard-write"]);
  const sessionId = await smokePage.evaluate(async (workerFp) =>
    (await window.__smoke.spawnShell(workerFp, "/tmp")).session_id, stack.workerFp);
  await navigateToSmokeSession(smokePage, sessionId);
  const focusIsOutsideMenu = () => smokePage.evaluate(() =>
    document.activeElement?.closest('[role="menu"]') === null);

  const trigger = smokePage.getByTestId("arrange-btn");
  await expect(trigger).toHaveAttribute("aria-haspopup", "menu");
  await expect(trigger).toHaveAttribute("aria-expanded", "false");
  await trigger.focus();
  await smokePage.keyboard.press("ArrowDown");
  await expect(smokePage.getByRole("menu", { name: "Arrange or transfer pane layout" })).toBeVisible();
  await expect(smokePage.getByTestId("layout-copy")).toBeFocused();
  await smokePage.keyboard.press("End");
  await expect(smokePage.getByTestId("layout-import")).toBeFocused();
  await smokePage.keyboard.press("Home");
  await expect(smokePage.getByTestId("layout-copy")).toBeFocused();
  await smokePage.keyboard.press("ArrowUp");
  await expect(smokePage.getByTestId("layout-import")).toBeFocused();
  await smokePage.keyboard.press("ArrowDown");
  await expect(smokePage.getByTestId("layout-copy")).toBeFocused();
  await smokePage.keyboard.press("Escape");
  await expect(smokePage.getByTestId("arrange-menu")).toHaveCount(0);
  await expect(trigger).toBeFocused();
  await expect(trigger).toHaveAttribute("aria-expanded", "false");

  await trigger.focus();
  await smokePage.keyboard.press("ArrowDown");
  await expect(smokePage.getByTestId("layout-copy")).toBeFocused();
  await smokePage.keyboard.press("Tab");
  await expect(smokePage.getByTestId("arrange-menu")).toHaveCount(0);
  await expect.poll(focusIsOutsideMenu).toBe(true);
  await trigger.focus();
  await smokePage.keyboard.press("ArrowUp");
  await expect(smokePage.getByTestId("layout-import")).toBeFocused();
  await smokePage.keyboard.press("Shift+Tab");
  await expect(smokePage.getByTestId("arrange-menu")).toHaveCount(0);
  await expect.poll(focusIsOutsideMenu).toBe(true);
  await trigger.focus();

  await smokePage.keyboard.press("Enter");
  await expect(smokePage.getByTestId("layout-copy")).toBeFocused();
  await smokePage.keyboard.press("Space");
  await expect(smokePage.getByTestId("arrange-menu")).toHaveCount(0);
  await expect.poll(() => smokePage.evaluate(() => navigator.clipboard.readText()))
    .toContain('"schema_version": 1');
  await trigger.focus();
  await smokePage.keyboard.press("ArrowDown");
  await smokePage.keyboard.press("Enter");
  await expect(smokePage.getByTestId("arrange-menu")).toHaveCount(0);

  const documentText = await smokePage.evaluate(() => navigator.clipboard.readText());
  await smokePage.setViewportSize({ width: 390, height: 844 });
  await expect(smokePage.getByTestId("arrange-btn")).toHaveCount(0);
  await smokePage.getByTestId("mobile-tab-count").click();
  await expect(smokePage.getByTestId("workspace-tabs-sheet")).toBeVisible();
  const workspaceMenuTrigger = smokePage.getByTestId("workspace-tabs-menu");
  await expect(workspaceMenuTrigger).toHaveAttribute("data-aria-label", "More options");
  await expect(workspaceMenuTrigger).toHaveAttribute("data-aria-haspopup", "menu");
  await expect(workspaceMenuTrigger).toHaveAttribute("data-aria-expanded", "false");
  await expect(workspaceMenuTrigger)
    .toHaveAttribute("aria-controls", "workspace-tabs-menu-popup");
  await workspaceMenuTrigger.focus();
  await smokePage.keyboard.press("ArrowDown");
  await expect(workspaceMenuTrigger).toHaveAttribute("data-aria-expanded", "true");
  await expect(smokePage.getByTestId("workspace-tabs-close-all")).toBeFocused();
  await smokePage.keyboard.press("Tab");
  await expect(smokePage.getByTestId("workspace-tabs-menu-popup")).toHaveCount(0);
  await expect(workspaceMenuTrigger).toHaveAttribute("data-aria-expanded", "false");
  await expect.poll(focusIsOutsideMenu).toBe(true);
  await workspaceMenuTrigger.focus();
  await smokePage.keyboard.press("ArrowUp");
  await expect(smokePage.getByTestId("layout-import")).toBeFocused();
  await smokePage.keyboard.press("Shift+Tab");
  await expect(smokePage.getByTestId("workspace-tabs-menu-popup")).toHaveCount(0);
  await expect.poll(focusIsOutsideMenu).toBe(true);
  await workspaceMenuTrigger.focus();
  await smokePage.keyboard.press("ArrowDown");
  await expect(smokePage.getByTestId("workspace-tabs-close-all")).toBeFocused();
  await smokePage.keyboard.press("End");
  await expect(smokePage.getByTestId("layout-import")).toBeFocused();
  await smokePage.keyboard.press("Escape");
  await expect(smokePage.getByTestId("workspace-tabs-menu-popup")).toHaveCount(0);
  await expect(workspaceMenuTrigger).toBeFocused();
  await smokePage.keyboard.press("ArrowDown");
  await expect(smokePage.getByTestId("layout-copy")).toBeVisible();
  await expect(smokePage.getByTestId("layout-download")).toBeVisible();
  await expect(smokePage.getByTestId("layout-import")).toBeVisible();

  const [fileChooser] = await Promise.all([
    smokePage.waitForEvent("filechooser"),
    smokePage.getByTestId("layout-import").click(),
  ]);
  await expect(smokePage.getByTestId("workspace-tabs-sheet")).toHaveCount(0);
  await fileChooser.setFiles({
    name: "roost-layout-v1.json",
    mimeType: "application/json",
    buffer: Buffer.from(documentText),
  });
  await expect(smokePage.getByTestId("layout-import-preview")).toBeVisible();
  await expect(smokePage.getByTestId("layout-import-apply")).toBeEnabled();
  await smokePage.locator("md-dialog")
    .filter({ has: smokePage.getByTestId("layout-import-preview") })
    .getByText("Cancel", { exact: true }).click();
  await expect(smokePage.getByTestId("layout-import-preview")).toHaveCount(0);
});
