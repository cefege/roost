// Real-stack proof for the folder picker at /browse/:workerFp: its chrome
// survives every listing state, a rejected folder name is named in place
// instead of flashing a toast, a real mkdir lands on the worker, a file row
// opens the viewer, and the whole surface fits a phone.
//
// Depends on the terminal tier's stack (real coord + worker + keeper + PTYs)
// and the VITE_ROOST_SMOKE=1 bundle's window.__smoke backdoor.

import { mkdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { test, expect } from "./fixtures.ts";
import { navigateToSmokeSession } from "./terminal-helpers.ts";
import type { Page } from "@playwright/test";

const MIN_DESKTOP_DIALOG_WIDTH = 600;
const MIN_TOUCH_TARGET = 44;
// Two chrome bands plus their borders; a third band would blow this budget.
const MAX_CHROME_HEIGHT = 120;
const MAX_ROW_HEIGHT = 60;

async function seedPickerSession(
  page: Page,
  workerFp: string,
  cwd: string,
): Promise<string> {
  const sessionId = await page.evaluate(async ({ fp, folder }) => {
    return (await window.__smoke.spawnShell(fp, folder)).session_id;
  }, { fp: workerFp, folder: cwd });
  // The picker opens on the worker's newest session cwd, so the spawn is only
  // a usable fixture once that cwd has folded into store state.
  await page.waitForFunction((id) => !!window.__smoke.state().sessions[id]?.cwd, sessionId);
  return sessionId;
}

test("folder picker keeps its chrome and names every failure", async ({
  smokePage,
  stack,
}, testInfo) => {
  test.skip(!testInfo.project.name.startsWith("chromium"), "desktop folder-picker chrome contract");

  const suffix = crypto.randomUUID().slice(0, 8);
  const seededFolder = `picker-${suffix}`;
  const seededFile = `picker-note-${suffix}.txt`;
  const createdFolder = `picker-made-${suffix}`;
  mkdirSync(join(stack.workerHome, seededFolder), { recursive: true });
  writeFileSync(join(stack.workerHome, seededFile), "folder picker fixture\n");

  const seedSessionId = await seedPickerSession(smokePage, stack.workerFp, stack.workerHome);
  await navigateToSmokeSession(smokePage, seedSessionId);

  await smokePage.getByTestId("sidebar-new-terminal-button").click();
  await expect(smokePage).toHaveURL(`${stack.baseUrl}/browse/${stack.workerFp}`);

  // Chrome that used to appear only on a phone, only once loaded, or not at all.
  await expect(smokePage.getByTestId("browse-close")).toBeVisible();
  await expect(smokePage.getByTestId("browse-up")).toBeVisible();
  await expect(smokePage.getByTestId("browse-home")).toBeVisible();
  await expect(smokePage.getByTestId("browse-filter-toggle")).toBeVisible();
  const newFolderButton = smokePage.getByTestId("browse-new");
  await expect(newFolderButton).toBeVisible();
  // Icon-font ligature text rides along in textContent, so the label is a containment check.
  await expect(newFolderButton).toContainText("New folder");

  const dialogBox = await smokePage.locator('[role="dialog"].roost-dialog--browse').boundingBox();
  expect(dialogBox?.width ?? 0).toBeGreaterThan(MIN_DESKTOP_DIALOG_WIDTH);

  // The complaint this layout answers: permanent chrome must not eat the screen
  // before the entries, and entries must tile instead of stacking one per row.
  const layout = await smokePage.evaluate(() => {
    const page = document.querySelector('[data-testid="browse-page"]')!.getBoundingClientRect();
    const row = document.querySelector('[data-testid="browse-row"]')!;
    const area = row.closest(".df-browse-area")!.getBoundingClientRect();
    const grid = document.querySelector(".md-list--grid")!;
    return {
      chrome: Math.round(area.top - page.top),
      columns: getComputedStyle(grid).gridTemplateColumns.split(" ").filter(Boolean).length,
      rowHeight: Math.round(row.getBoundingClientRect().height),
    };
  });
  expect(layout.chrome).toBeLessThanOrEqual(MAX_CHROME_HEIGHT);
  expect(layout.columns).toBeGreaterThanOrEqual(3);
  expect(layout.rowHeight).toBeLessThanOrEqual(MAX_ROW_HEIGHT);

  const rows = smokePage.getByTestId("browse-row");
  await expect(rows.filter({ hasText: seededFolder })).toHaveCount(1);
  await smokePage.getByTestId("browse-filter-toggle").click();
  await smokePage.getByTestId("browse-filter").fill(seededFolder);
  await expect(rows).toHaveCount(1);
  await smokePage.getByTestId("browse-filter").fill(`${seededFolder}-nope`);
  await expect(smokePage.getByTestId("browse-no-matches")).toBeVisible();
  await smokePage.getByTestId("browse-clear-filter").click();
  await expect(smokePage.getByTestId("browse-crumbs")).toBeVisible();
  await expect(rows.filter({ hasText: seededFolder })).toHaveCount(1);

  const lastCrumb = smokePage.getByTestId("browse-crumb").last();
  await rows.filter({ hasText: seededFolder }).click();
  await expect(lastCrumb).toHaveAttribute("title", join(stack.workerHome, seededFolder));
  await smokePage.getByTestId("browse-up").click();
  await expect(lastCrumb).toHaveAttribute("title", stack.workerHome);

  await newFolderButton.click();
  const nameField = smokePage.getByTestId("newfolder-input");
  await nameField.fill("bad/name");
  await smokePage.getByTestId("newfolder-confirm").click();
  await expect(smokePage.getByText("Folder names can't contain / or \\.")).toBeVisible();
  await expect(nameField).toBeVisible();
  await expect(smokePage.getByTestId("toast")).toHaveCount(0);

  await nameField.fill(createdFolder);
  await smokePage.getByTestId("newfolder-confirm").click();
  await expect(nameField).toHaveCount(0);
  await expect(lastCrumb).toHaveAttribute("title", join(stack.workerHome, createdFolder));

  await smokePage.getByTestId("browse-up").click();
  await expect(lastCrumb).toHaveAttribute("title", stack.workerHome);
  await smokePage.getByTestId("browse-show-files").click();
  const fileRow = smokePage.getByTestId("browse-file-row").filter({ hasText: seededFile });
  await expect(fileRow).toHaveCount(1);
  await fileRow.click();
  await expect(smokePage).toHaveURL(new RegExp(`/file/${stack.workerFp}/.*${seededFile}$`));
  await expect(smokePage.getByTestId("file-viewer-sheet")).toBeVisible();

  await smokePage.goBack();
  await expect(smokePage.getByTestId("browse-close")).toBeVisible();
  await smokePage.getByTestId("browse-close").click();
  // Root restores the last visited route, so closing is proven by the picker
  // leaving the DOM and the browse route leaving the URL.
  await expect(smokePage.getByTestId("browse-page")).toHaveCount(0);
  await expect(smokePage).not.toHaveURL(/\/browse\//);
});

test("folder picker fits a phone", async ({ mobileSmokePage, stack }) => {
  const suffix = crypto.randomUUID().slice(0, 8);
  for (const name of [`phone-a-${suffix}`, `phone-b-${suffix}`, `phone-c-${suffix}`]) {
    mkdirSync(join(stack.workerHome, name), { recursive: true });
  }
  // Seeds the picker's start directory: it opens on the worker's newest session cwd.
  await seedPickerSession(mobileSmokePage, stack.workerFp, stack.workerHome);
  await mobileSmokePage.evaluate((fp) => {
    window.__smoke.navigate(`/browse/${fp}`);
  }, stack.workerFp);

  const newFolderButton = mobileSmokePage.getByTestId("browse-new");
  await expect(newFolderButton).toBeVisible();
  await expect(newFolderButton).toHaveAccessibleName("New folder");
  await expect(newFolderButton).toBeInViewport({ ratio: 1 });
  // The machine a tap would open a terminal on: hidden on compact before.
  await expect(mobileSmokePage.getByTestId("browse-machine")).toBeVisible();
  await expect(mobileSmokePage.getByTestId("browse-machine")).toBeInViewport({ ratio: 1 });

  // A phone shows two entries per row, and the chrome above them stays inside
  // its two-band budget.
  await expect(mobileSmokePage.getByTestId("browse-row").first()).toBeVisible();
  const phoneLayout = await mobileSmokePage.evaluate(() => {
    const page = document.querySelector('[data-testid="browse-page"]')!.getBoundingClientRect();
    const row = document.querySelector('[data-testid="browse-row"]')!;
    const area = row.closest(".df-browse-area")!.getBoundingClientRect();
    const grid = document.querySelector(".md-list--grid")!;
    return {
      chrome: Math.round(area.top - page.top),
      columns: getComputedStyle(grid).gridTemplateColumns.split(" ").filter(Boolean).length,
      rowHeight: Math.round(row.getBoundingClientRect().height),
    };
  });
  expect(phoneLayout.columns).toBe(2);
  expect(phoneLayout.chrome).toBeLessThanOrEqual(MAX_CHROME_HEIGHT);
  expect(phoneLayout.rowHeight).toBeLessThanOrEqual(MAX_ROW_HEIGHT);

  for (const testId of ["browse-up", "browse-home"]) {
    const control = mobileSmokePage.getByTestId(testId);
    await expect(control).toBeVisible();
    const box = await control.boundingBox();
    expect(box?.width ?? 0).toBeGreaterThanOrEqual(MIN_TOUCH_TARGET);
    expect(box?.height ?? 0).toBeGreaterThanOrEqual(MIN_TOUCH_TARGET);
  }

  await newFolderButton.click();
  const sheet = mobileSmokePage.locator('[role="dialog"]');
  await expect(sheet).toHaveClass(/roost-sheet--bottom/);
  const sheetBox = await sheet.boundingBox();
  const viewport = mobileSmokePage.viewportSize();
  expect(sheetBox?.width ?? Number.POSITIVE_INFINITY).toBeLessThanOrEqual(viewport?.width ?? 0);
});
