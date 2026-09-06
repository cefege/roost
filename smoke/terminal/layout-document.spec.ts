// Exercises portable pane-layout documents through Chromium's real download,
// file-picker, preview, and Apply surfaces against real PTYs.
// The terminal fixture owns stack/session cleanup; a same-context peer page
// proves browser-local persistence does not live-fold into open documents.

import type { Download, Page } from "@playwright/test";
import { mkdirSync } from "node:fs";
import type { LayoutDocumentLeaf, LayoutDocumentV1 } from "@roost/shared/layout-document";
import { test, expect } from "./fixtures.ts";
import { navigateToSmokeSession } from "./terminal-helpers.ts";
import {
  LAYOUT_STORAGE_KEY,
  expectRenderedLayout,
  readRenderedLayout,
  readRuntimeIds,
  waitForPersistedRuntimeIds,
  type PaneSnapshot,
  type RenderedLayoutSnapshot,
} from "./layout-document-snapshots.ts";

const DOWNLOAD_NAME = "roost-layout-v1.json";

function stableDocumentLeaf(index: number): LayoutDocumentLeaf {
  const suffix = String(index);
  return {
    kind: "leaf",
    leaf_key: `leaf-${suffix}`,
    slot_keys: [`slot-${suffix}`],
    selected_slot_key: `slot-${suffix}`,
  };
}

async function chooseArrangeItem(
  page: Page, testId: "arrange-grid" | "arrange-rows",
): Promise<void> {
  await page.getByTestId("arrange-btn").click();
  await expect(page.getByTestId("arrange-menu")).toBeVisible();
  await page.getByTestId(testId).click();
}

async function typeTrustedMarker(
  page: Page, sessionId: string, command: string, marker: string,
): Promise<void> {
  const slot = page.getByTestId(`terminal-slot-${sessionId}`);
  await slot.getByTestId("terminal-display").click();
  await expect(slot).toHaveAttribute("data-focused", "true");
  await expect.poll(() => page.evaluate((id) => {
    const smoke = window.__smoke;
    return smoke.paneFocused(id).focused;
  }, sessionId)).toBe(true);
  await page.keyboard.type(command);
  await page.keyboard.press("Enter");
  await expect(slot).toContainText(marker, { timeout: 30_000 });
}

async function expectMarkers(
  page: Page, sessionIds: readonly string[], markers: readonly string[],
): Promise<void> {
  for (let index = 0; index < sessionIds.length; index++) {
    await expect(page.getByTestId(`terminal-slot-${sessionIds[index]}`))
      .toContainText(markers[index]!, { timeout: 30_000 });
  }
}

async function readDownloadText(download: Download): Promise<string> {
  const stream = await download.createReadStream();
  if (!stream) throw new Error("layout download did not expose a readable stream");
  const chunks: Buffer[] = [];
  for await (const chunk of stream) chunks.push(Buffer.from(chunk));
  return Buffer.concat(chunks).toString("utf8");
}

test("portable layout document round-trips without replacing another open page", async ({
  smokePage,
  stack,
}, testInfo) => {
  test.skip(testInfo.project.name !== "chromium-desktop", "Chromium desktop layout-document contract");
  test.setTimeout(180_000);
  await smokePage.context().grantPermissions(["clipboard-read", "clipboard-write"]);

  const desktopViewport = smokePage.viewportSize();
  if (!desktopViewport) throw new Error("desktop project did not provide a viewport");
  const layoutFolder = testInfo.outputPath("layout-cwd");
  mkdirSync(layoutFolder, { recursive: true });
  const createdSessionIds = await smokePage.evaluate(async ({ workerFp, folder }) => {
    const smoke = window.__smoke;
    const ids: string[] = [];
    for (let index = 0; index < 3; index++) {
      ids.push((await smoke.spawnShell(workerFp, folder)).session_id);
    }
    return ids;
  }, { workerFp: stack.workerFp, folder: layoutFolder });
  await smokePage.waitForFunction((ids) => {
    const smoke = window.__smoke;
    return ids.every((id) => smoke.state().sessions[id]?.status === "open");
  }, createdSessionIds);
  const sessionIds = await smokePage.evaluate((ids) => {
    const sessions = window.__smoke.state().sessions;
    return [...ids].sort((left, right) =>
      sessions[left]!.created_at - sessions[right]!.created_at || left.localeCompare(right));
  }, createdSessionIds);
  const [firstSessionId, focusedSessionId, thirdSessionId] = sessionIds;
  if (!firstSessionId || !focusedSessionId || !thirdSessionId) {
    throw new Error("three real PTYs were not created");
  }

  const gridPanes: PaneSnapshot[] = [
    { tabs: [firstSessionId], selected: firstSessionId, focused: false },
    { tabs: [focusedSessionId], selected: focusedSessionId, focused: true },
    { tabs: [thirdSessionId], selected: thirdSessionId, focused: false },
  ];
  const gridLayout: RenderedLayoutSnapshot = {
    multiPane: "true",
    panes: gridPanes,
    dividerDirections: ["col", "row"],
    visibleSessionIds: sessionIds,
    focusedSessionIds: [focusedSessionId],
    pathname: `/s/${focusedSessionId}`,
  };
  const rowsLayout: RenderedLayoutSnapshot = {
    ...gridLayout,
    dividerDirections: ["col", "col"],
  };

  await navigateToSmokeSession(smokePage, firstSessionId);
  await expect.poll(
    () => readRenderedLayout(smokePage, sessionIds),
    { timeout: 30_000, intervals: [50, 100, 250] },
  ).toMatchObject({
    multiPane: "false",
    dividerDirections: [],
    visibleSessionIds: [firstSessionId],
    focusedSessionIds: [firstSessionId],
    pathname: `/s/${firstSessionId}`,
  });
  const initialLayout = await readRenderedLayout(smokePage, sessionIds);
  expect(initialLayout.panes).toHaveLength(1);
  expect(initialLayout.panes[0]).toMatchObject({
    selected: firstSessionId,
    focused: true,
  });
  expect(new Set(initialLayout.panes[0]!.tabs)).toEqual(new Set(sessionIds));
  const initialRuntimeIds = await readRuntimeIds(smokePage, sessionIds);
  await waitForPersistedRuntimeIds(smokePage, initialRuntimeIds.paneIds);

  const independentPage = await smokePage.context().newPage();
  try {
    await independentPage.goto(`${stack.baseUrl}/s/${firstSessionId}`, {
      waitUntil: "domcontentloaded",
    });
    await independentPage.waitForFunction(() => typeof window.__smoke === "object");
    await expectRenderedLayout(independentPage, sessionIds, initialLayout);
    await smokePage.bringToFront();

    const suffix = crypto.randomUUID().replaceAll("-", "").slice(0, 6);
    const markers = sessionIds.map((_, index) => `LD${index + 1}_${suffix}`);
    for (const index of [0, 2, 1]) {
      const sessionId = sessionIds[index]!;
      const marker = markers[index]!;
      const command = `printf '${marker}\\n'`;
      await smokePage.evaluate(async ({ id, input }) => {
        await window.__smoke.input(id, input);
      }, { id: sessionId, input: `${command}\r` });
    }
    await chooseArrangeItem(smokePage, "arrange-grid");
    await expectMarkers(smokePage, sessionIds, markers);
    const focusedSlot = smokePage.getByTestId(`terminal-slot-${focusedSessionId}`);
    await focusedSlot.getByTestId("terminal-display").click();
    await expect(focusedSlot).toHaveAttribute("data-focused", "true");
    await expect.poll(() => smokePage.evaluate(
      (id) => window.__smoke.paneFocused(id).focused,
      focusedSessionId,
    )).toBe(true);
    await expect(smokePage).toHaveURL(`${stack.baseUrl}/s/${focusedSessionId}`);
    await expectRenderedLayout(smokePage, sessionIds, gridLayout);
    await expectRenderedLayout(independentPage, sessionIds, initialLayout);

    const runtimeIds = await readRuntimeIds(smokePage, sessionIds);
    expect(runtimeIds.paneIds).toHaveLength(3);
    expect(new Set(runtimeIds.paneIds).size).toBe(3);
    expect(runtimeIds.splitIds).toHaveLength(2);

    const expectedDocument: LayoutDocumentV1 = {
      schema_version: 1,
      root: {
        kind: "split",
        direction: "row",
        ratio: 2 / 3,
        first: {
          kind: "split",
          direction: "col",
          ratio: 0.5,
          first: stableDocumentLeaf(1),
          second: stableDocumentLeaf(2),
        },
        second: stableDocumentLeaf(3),
      },
      focused_leaf_key: "leaf-2",
      bindings: [
        { slot_key: "slot-1", session_id: firstSessionId },
        { slot_key: "slot-2", session_id: focusedSessionId },
        { slot_key: "slot-3", session_id: thirdSessionId },
      ],
    };
    const expectedDocumentText = `${JSON.stringify(expectedDocument, null, 2)}\n`;
    await smokePage.getByTestId("arrange-btn").click();
    await smokePage.getByTestId("layout-copy").click();
    await expect.poll(
      () => smokePage.evaluate(() => navigator.clipboard.readText()),
    ).toBe(expectedDocumentText);

    await smokePage.getByTestId("arrange-btn").click();
    const [download] = await Promise.all([
      smokePage.waitForEvent("download"),
      smokePage.getByTestId("layout-download").click(),
    ]);
    expect(download.suggestedFilename()).toBe(DOWNLOAD_NAME);
    const downloadedText = await readDownloadText(download);
    expect(downloadedText).toBe(expectedDocumentText);
    expect(JSON.parse(downloadedText)).toStrictEqual(expectedDocument);
    for (const forbidden of [
      ...runtimeIds.paneIds,
      ...runtimeIds.splitIds,
      ...markers,
      "\"paneId\"",
      "\"focusedPaneId\"",
      "\"splitId\"",
      "\"channel\"",
      "\"cells\"",
      "\"viewport\"",
      "\"transcript\"",
    ]) expect(downloadedText).not.toContain(forbidden);

    await chooseArrangeItem(smokePage, "arrange-rows");
    await expectRenderedLayout(smokePage, sessionIds, rowsLayout);
    await expectRenderedLayout(independentPage, sessionIds, initialLayout);

    await smokePage.getByTestId("arrange-btn").click();
    const [fileChooser] = await Promise.all([
      smokePage.waitForEvent("filechooser"),
      smokePage.getByTestId("layout-import").click(),
    ]);
    expect(await fileChooser.element().getAttribute("data-testid"))
      .toBe("layout-document-file-input");
    await fileChooser.setFiles({
      name: DOWNLOAD_NAME,
      mimeType: "application/json",
      buffer: Buffer.from(downloadedText),
    });

    const preview = smokePage.getByTestId("layout-import-preview");
    await expect(preview).toBeVisible();
    await expect(preview).toContainText(DOWNLOAD_NAME);
    await expect(preview.locator("code")).toHaveText([
      "Split row · 67% first",
      "· Split col · 50% first",
      "· · leaf-1 · 1 session",
      `· · · slot-1 → ${firstSessionId} · selected`,
      "· · leaf-2 · 1 session · focused",
      `· · · slot-2 → ${focusedSessionId} · selected`,
      "· leaf-3 · 1 session",
      `· · slot-3 → ${thirdSessionId} · selected`,
    ]);
    await expectRenderedLayout(smokePage, sessionIds, rowsLayout);
    await smokePage.getByTestId("layout-import-apply").click();
    await expect(preview).toHaveCount(0);

    await expectRenderedLayout(smokePage, sessionIds, gridLayout);
    const importedRuntimeIds = await readRuntimeIds(smokePage, sessionIds);
    const exportedRuntimeIds = [...runtimeIds.paneIds, ...runtimeIds.splitIds];
    expect([...importedRuntimeIds.paneIds, ...importedRuntimeIds.splitIds]
      .every((runtimeId) => !exportedRuntimeIds.includes(runtimeId))).toBe(true);
    await expect(smokePage).toHaveURL(`${stack.baseUrl}/s/${focusedSessionId}`);
    for (const sessionId of sessionIds) {
      await expect(smokePage.getByTestId(`tab-${sessionId}`))
        .toHaveAttribute("data-active", "true");
    }
    await expectMarkers(smokePage, sessionIds, markers);

    const trustedMarker = `LT_${suffix}`;
    await typeTrustedMarker(
      smokePage,
      focusedSessionId,
      `printf '${trustedMarker}\\n'`,
      trustedMarker,
    );
    await expect(smokePage.getByTestId(`terminal-slot-${firstSessionId}`))
      .not.toContainText(trustedMarker);
    await expect(smokePage.getByTestId(`terminal-slot-${thirdSessionId}`))
      .not.toContainText(trustedMarker);
    await expectRenderedLayout(independentPage, sessionIds, initialLayout);

    await smokePage.setViewportSize({ width: 390, height: 844 });
    await expectRenderedLayout(smokePage, sessionIds, {
      multiPane: "false",
      panes: [],
      dividerDirections: [],
      visibleSessionIds: [focusedSessionId],
      focusedSessionIds: [focusedSessionId],
      pathname: `/s/${focusedSessionId}`,
    });
    await expect(smokePage.getByTestId("mobile-tab-count")).toHaveText("3");
    await smokePage.getByTestId("mobile-tab-count").click();
    const cards = smokePage.locator(".terminal-card");
    await expect(cards).toHaveCount(3);
    expect(await cards.evaluateAll((elements) => elements.map((element) =>
      (element.getAttribute("data-testid") ?? "").slice("terminal-card-".length))))
      .toEqual(sessionIds);
    await expect(smokePage.getByTestId(`terminal-card-${focusedSessionId}`))
      .toHaveAttribute("data-active", "true");
    await smokePage.getByTestId("workspace-tabs-back").click();
    await expect(smokePage.getByTestId("workspace-tabs-sheet")).toHaveCount(0);

    await smokePage.setViewportSize(desktopViewport);
    await expectRenderedLayout(smokePage, sessionIds, gridLayout);
    await expectMarkers(smokePage, sessionIds, markers);

    await waitForPersistedRuntimeIds(
      smokePage,
      [...importedRuntimeIds.paneIds, ...importedRuntimeIds.splitIds],
    );
    const persistedSource = await smokePage.evaluate((key) => localStorage.getItem(key), LAYOUT_STORAGE_KEY);
    expect(await independentPage.evaluate((key) => localStorage.getItem(key), LAYOUT_STORAGE_KEY))
      .toBe(persistedSource);
    await expectRenderedLayout(independentPage, sessionIds, initialLayout);

    await smokePage.reload({ waitUntil: "domcontentloaded" });
    await smokePage.waitForFunction(() => typeof window.__smoke === "object");
    await expectRenderedLayout(smokePage, sessionIds, gridLayout);
    await expect(smokePage).toHaveURL(`${stack.baseUrl}/s/${focusedSessionId}`);
    await expectMarkers(smokePage, sessionIds, markers);
    await expect(smokePage.getByTestId(`terminal-slot-${focusedSessionId}`))
      .toContainText(trustedMarker);
    await expectRenderedLayout(independentPage, sessionIds, initialLayout);
  } finally {
    await independentPage.close();
  }
});
