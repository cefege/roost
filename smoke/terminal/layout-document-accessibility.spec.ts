// Exercises pane-arrangement menus through real desktop and compact surfaces.
// The absence assertions prevent removed local document-transfer controls from
// returning through either presentation path.

import { mkdirSync } from "node:fs";
import { test, expect } from "./fixtures.ts";
import { navigateToSmokeSession } from "./terminal-helpers.ts";

test("pane arrangement menus omit local document-transfer controls", async ({
  smokePage,
  stack,
}, testInfo) => {
  test.skip(testInfo.project.name !== "chromium-desktop", "Chromium menu contract");
  const isolatedFolder = testInfo.outputPath("arrange-menu");
  mkdirSync(isolatedFolder, { recursive: true });
  const sessionId = await smokePage.evaluate(async ({ workerFp, cwd }) =>
    (await window.__smoke.spawnShell(workerFp, cwd)).session_id, {
      workerFp: stack.workerFp,
      cwd: isolatedFolder,
    });
  await navigateToSmokeSession(smokePage, sessionId);

  const arrangeTrigger = smokePage.getByTestId("arrange-btn");
  await arrangeTrigger.click();
  await expect(smokePage.getByRole("menu", { name: "Arrange pane layout" })).toBeVisible();
  await expect(smokePage.getByTestId("layout-copy")).toHaveCount(0);
  await expect(smokePage.getByTestId("layout-download")).toHaveCount(0);
  await expect(smokePage.getByTestId("layout-import")).toHaveCount(0);

  await smokePage.setViewportSize({ width: 390, height: 844 });
  await smokePage.getByTestId("mobile-tab-count").click();
  await expect(smokePage.getByTestId("workspace-tabs-sheet")).toBeVisible();
  await smokePage.getByTestId("workspace-tabs-menu").click();
  await expect(smokePage.getByTestId("workspace-tabs-menu-popup")).toBeVisible();
  await expect(smokePage.getByTestId("layout-copy")).toHaveCount(0);
  await expect(smokePage.getByTestId("layout-download")).toHaveCount(0);
  await expect(smokePage.getByTestId("layout-import")).toHaveCount(0);
});
