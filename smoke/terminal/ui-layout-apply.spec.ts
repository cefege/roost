// Proves acknowledged layout application against two real browser tabs and Sync sockets.
// The coordinator client targets one reported tab while portable layout snapshots
// distinguish publication, committed application, rejection, and a lost target.

import type { Page } from "@playwright/test";
import { mkdirSync } from "node:fs";
import type { LayoutDocumentV1 } from "@roost/shared/layout-document";
import {
  layoutDocumentFromProto,
  layoutDocumentToProto,
} from "@roost/shared/layout-document-proto";
import { UiApplyLayoutOutcome } from "@roost/shared/proto/sync_pb";
import { test, expect } from "./fixtures.ts";
import {
  LAYOUT_STORAGE_KEY,
  expectRenderedLayout,
  readRenderedLayout,
  readRuntimeIds,
  waitForPersistedRuntimeIds,
  type RenderedLayoutSnapshot,
} from "./layout-document-snapshots.ts";
import { navigateToSmokeSession } from "./terminal-helpers.ts";
import { readTerminalStreamProbe } from "./terminal-probe-helpers.ts";
import type { TerminalTestStack } from "./stack.ts";

function singlePaneDocument(
  sessionIds: readonly string[],
  selectedSessionId: string,
): LayoutDocumentV1 {
  const selectedIndex = sessionIds.indexOf(selectedSessionId);
  if (selectedIndex < 0) throw new Error("selected session is not in the document");
  return {
    schema_version: 1,
    root: {
      kind: "leaf",
      leaf_key: "leaf-1",
      slot_keys: sessionIds.map((_, index) => `slot-${index + 1}`),
      selected_slot_key: `slot-${selectedIndex + 1}`,
    },
    focused_leaf_key: "leaf-1",
    bindings: sessionIds.map((sessionId, index) => ({
      slot_key: `slot-${index + 1}`,
      session_id: sessionId,
    })),
  };
}

function splitDocument(firstSessionId: string, secondSessionId: string): LayoutDocumentV1 {
  return {
    schema_version: 1,
    root: {
      kind: "split",
      direction: "row",
      ratio: 0.5,
      first: {
        kind: "leaf",
        leaf_key: "leaf-1",
        slot_keys: ["slot-1"],
        selected_slot_key: "slot-1",
      },
      second: {
        kind: "leaf",
        leaf_key: "leaf-2",
        slot_keys: ["slot-2"],
        selected_slot_key: "slot-2",
      },
    },
    focused_leaf_key: "leaf-2",
    bindings: [
      { slot_key: "slot-1", session_id: firstSessionId },
      { slot_key: "slot-2", session_id: secondSessionId },
    ],
  };
}

async function readTabId(page: Page): Promise<string> {
  const tabId = await page.evaluate(() => sessionStorage.getItem("roost.tabId"));
  if (!tabId) throw new Error("browser page did not claim roost.tabId");
  return tabId;
}

async function readCurrentSocketId(page: Page, sessionId: string): Promise<string> {
  let socketId = "";
  await expect.poll(async () => {
    const sync = (await readTerminalStreamProbe(page, sessionId)).browser.sync;
    socketId = sync.ready ? (sync.socket_id ?? "") : "";
    return socketId;
  }, { timeout: 30_000, intervals: [50, 100, 250] }).not.toBe("");
  return socketId;
}

async function expectReportedDocument(
  client: TerminalTestStack["client"],
  tabId: string,
  activePath: string,
  expectedDocument: LayoutDocumentV1,
): Promise<string> {
  let folderKey = "";
  await expect.poll(async () => {
    const tab = (await client.uiListStates({})).tabs.find((candidate) => candidate.tabId === tabId);
    const state = tab?.state;
    if (!state?.layoutDocument) return null;
    folderKey = state.folderKey;
    return {
      tabId: state.tabId,
      activePath: state.activePath,
      folderKey,
      document: layoutDocumentFromProto(state.layoutDocument),
    };
  }, { timeout: 30_000, intervals: [50, 100, 250] }).toEqual({
    tabId,
    activePath,
    folderKey: expect.stringMatching(/.+/),
    document: expectedDocument,
  });
  return folderKey;
}

test("acknowledged layout apply targets one live browser generation", async ({
  smokePage,
  stack,
}, testInfo) => {
  test.skip(testInfo.project.name !== "chromium-desktop", "Chromium desktop layout RPC contract");
  test.setTimeout(180_000);

  const currentFolder = testInfo.outputPath("ui-layout-current");
  const foreignFolder = testInfo.outputPath("ui-layout-foreign");
  mkdirSync(currentFolder, { recursive: true });
  mkdirSync(foreignFolder, { recursive: true });
  const createdSessionIds = await smokePage.evaluate(async ({ workerFp, folders }) => {
    const smoke = window.__smoke;
    const ids = [
      (await smoke.spawnShell(workerFp, folders.current)).session_id,
      (await smoke.spawnShell(workerFp, folders.current)).session_id,
      (await smoke.spawnShell(workerFp, folders.foreign)).session_id,
    ];
    return ids;
  }, {
    workerFp: stack.workerFp,
    folders: { current: currentFolder, foreign: foreignFolder },
  });
  await smokePage.waitForFunction((ids) => ids.every(
    (sessionId) => window.__smoke.state().sessions[sessionId]?.status === "open",
  ), createdSessionIds);
  const currentSessionIds = await smokePage.evaluate((ids) => {
    const sessions = window.__smoke.state().sessions;
    return ids.slice(0, 2).sort((left, right) =>
      sessions[left]!.created_at - sessions[right]!.created_at || left.localeCompare(right));
  }, createdSessionIds);
  const [firstSessionId, secondSessionId] = currentSessionIds;
  const foreignSessionId = createdSessionIds[2];
  if (!firstSessionId || !secondSessionId || !foreignSessionId) {
    throw new Error("three real PTYs were not created");
  }

  await navigateToSmokeSession(smokePage, firstSessionId);
  await expect.poll(
    () => readRenderedLayout(smokePage, currentSessionIds),
    { timeout: 30_000, intervals: [50, 100, 250] },
  ).toMatchObject({
    multiPane: "false",
    visibleSessionIds: [firstSessionId],
    focusedSessionIds: [firstSessionId],
    pathname: `/s/${firstSessionId}`,
  });
  const initialLayout = await readRenderedLayout(smokePage, currentSessionIds);
  expect(initialLayout.panes).toHaveLength(1);
  expect(initialLayout.panes[0]!.tabs).toEqual(currentSessionIds);
  const initialDocument = singlePaneDocument(currentSessionIds, firstSessionId);
  const controlRuntimeIds = await readRuntimeIds(smokePage, currentSessionIds);

  const targetPage = await smokePage.context().newPage();
  let targetClosed = false;
  try {
    await targetPage.goto(`${stack.baseUrl}/s/${firstSessionId}`, { waitUntil: "domcontentloaded" });
    await targetPage.waitForFunction(() => typeof window.__smoke === "object");
    await expect(targetPage.getByTestId(`terminal-slot-${firstSessionId}`))
      .toBeVisible({ timeout: 30_000 });
    await expectRenderedLayout(targetPage, currentSessionIds, initialLayout);

    const [controlTabId, targetTabId] = await Promise.all([
      readTabId(smokePage),
      readTabId(targetPage),
    ]);
    expect(targetTabId).not.toBe(controlTabId);
    const [controlSocketId, targetSocketId] = await Promise.all([
      readCurrentSocketId(smokePage, firstSessionId),
      readCurrentSocketId(targetPage, firstSessionId),
    ]);
    expect(targetSocketId).not.toBe(controlSocketId);
    const [controlFolderKey, targetFolderKey] = await Promise.all([
      expectReportedDocument(
        stack.client, controlTabId, `/s/${firstSessionId}`, initialDocument,
      ),
      expectReportedDocument(
        stack.client, targetTabId, `/s/${firstSessionId}`, initialDocument,
      ),
    ]);
    expect(targetFolderKey).toBe(controlFolderKey);
    const targetFingerprint = (await stack.client.uiListStates({})).tabs
      .find((candidate) => candidate.tabId === targetTabId)?.fp;
    if (!targetFingerprint) throw new Error("target tab report has no browser fingerprint");


    const legacyPublication = await stack.client.uiDispatch({
      targetTabId: `missing-${crypto.randomUUID()}`,
      command: {
        command: { case: "selectTab", value: { sessionId: secondSessionId } },
      },
    });
    expect(legacyPublication.delivered).toBe(2);
    // Publication has no completion signal; allow both live sockets one delivery turn.
    await targetPage.waitForTimeout(1_000);
    await expectRenderedLayout(targetPage, currentSessionIds, initialLayout);
    await expectRenderedLayout(smokePage, currentSessionIds, initialLayout);
    const targetRuntimeBeforeApply = await readRuntimeIds(targetPage, currentSessionIds);

    const appliedDocument = splitDocument(firstSessionId, secondSessionId);
    const applyResponse = await stack.client.uiApplyLayout({
      targetTabId,
      targetFingerprint,
      document: layoutDocumentToProto(appliedDocument),
    });
    expect(applyResponse.outcome).toBe(UiApplyLayoutOutcome.APPLIED);
    expect(applyResponse.correlationId).not.toBe("");
    expect(applyResponse.reason).toBeUndefined();
    const appliedLayout: RenderedLayoutSnapshot = {
      multiPane: "true",
      panes: [
        { tabs: [firstSessionId], selected: firstSessionId, focused: false },
        { tabs: [secondSessionId], selected: secondSessionId, focused: true },
      ],
      dividerDirections: ["row"],
      visibleSessionIds: currentSessionIds,
      focusedSessionIds: [secondSessionId],
      pathname: `/s/${secondSessionId}`,
    };
    await expectRenderedLayout(targetPage, currentSessionIds, appliedLayout);
    const targetRuntimeAfterApply = await readRuntimeIds(targetPage, currentSessionIds);
    const priorTargetRuntimeIds = new Set([
      ...targetRuntimeBeforeApply.paneIds,
      ...targetRuntimeBeforeApply.splitIds,
    ]);
    expect([
      ...targetRuntimeAfterApply.paneIds,
      ...targetRuntimeAfterApply.splitIds,
    ].every((runtimeId) => !priorTargetRuntimeIds.has(runtimeId))).toBe(true);
    await waitForPersistedRuntimeIds(targetPage, [
      ...targetRuntimeAfterApply.paneIds,
      ...targetRuntimeAfterApply.splitIds,
    ]);
    await expectRenderedLayout(smokePage, currentSessionIds, initialLayout);
    expect(await readRuntimeIds(smokePage, currentSessionIds)).toEqual(controlRuntimeIds);
    expect(await readCurrentSocketId(targetPage, secondSessionId)).toBe(targetSocketId);
    await expectReportedDocument(
      stack.client, targetTabId, `/s/${secondSessionId}`, appliedDocument,
    );

    const targetRuntimeBeforeReject = targetRuntimeAfterApply;
    const persistedBeforeReject = await targetPage.evaluate(
      (key) => localStorage.getItem(key), LAYOUT_STORAGE_KEY,
    );
    const rejectResponse = await stack.client.uiApplyLayout({
      targetTabId,
      targetFingerprint,
      document: layoutDocumentToProto(singlePaneDocument([foreignSessionId], foreignSessionId)),
    });
    expect(rejectResponse.outcome).toBe(UiApplyLayoutOutcome.REJECTED);
    expect(rejectResponse.correlationId).not.toBe("");
    expect(rejectResponse.correlationId).not.toBe(applyResponse.correlationId);
    expect(rejectResponse.reason)
      .toBe("The layout document is invalid for the current folder.");
    await expectRenderedLayout(targetPage, currentSessionIds, appliedLayout);
    expect(await readRuntimeIds(targetPage, currentSessionIds)).toEqual(targetRuntimeBeforeReject);
    expect(await targetPage.evaluate((key) => localStorage.getItem(key), LAYOUT_STORAGE_KEY))
      .toBe(persistedBeforeReject);
    await expectRenderedLayout(smokePage, currentSessionIds, initialLayout);

    expect((await stack.client.uiListStates({})).tabs.some(
      (candidate) => candidate.tabId === targetTabId,
    )).toBe(true);
    await targetPage.close();
    targetClosed = true;
    const goneResponse = await stack.client.uiApplyLayout({
      targetTabId,
      targetFingerprint,
      document: layoutDocumentToProto(appliedDocument),
    });
    expect(goneResponse.outcome).toBe(UiApplyLayoutOutcome.TARGET_GONE);
    expect(goneResponse.correlationId).not.toBe("");
    expect([
      applyResponse.correlationId,
      rejectResponse.correlationId,
    ]).not.toContain(goneResponse.correlationId);
    expect(goneResponse.reason).toBe("target acknowledgement unavailable");
    expect((await stack.client.uiListStates({})).tabs.some(
      (candidate) => candidate.tabId === targetTabId,
    )).toBe(true);
    await expectRenderedLayout(smokePage, currentSessionIds, initialLayout);
  } finally {
    if (!targetClosed) await targetPage.close();
  }
});
