// Browser proof for keyboard activation of the closed core-action catalog.
// The flow exercises route actions, queue-folder prefills, and a real sibling
// spawn without bypassing CommandPaletteBody's close-before-execute path.

import type { Page } from "@playwright/test";
import { test, expect } from "./fixtures.ts";
import {
  navigateToSmokeSession,
  pressPlatformShortcut,
  spawnSmokeShell,
} from "./terminal-helpers.ts";
import type { RecoverySmokeApi } from "./terminal-smoke-api.ts";

type CommandPaletteSmokeWindow = Window & {
  readonly __smoke: RecoverySmokeApi;
};

async function keyboardActivateCoreAction(
  page: Page,
  query: string,
  expectedLabel: string,
): Promise<void> {
  await pressPlatformShortcut(page, "commandPalette", "k");
  const palette = page.getByTestId("command-palette");
  await expect(palette).toBeVisible();
  await page.getByTestId("command-palette-input").fill(query);
  const results = page.getByTestId("command-palette-item");
  await expect(results).toHaveCount(1);
  await expect(results.first()).toContainText(expectedLabel);
  await page.keyboard.press("Enter");
  await expect(palette).toHaveCount(0);
}

test("command palette keyboard activates core navigation and folder actions", async ({
  smokePage,
  stack,
}, testInfo) => {
  test.skip(
    !testInfo.project.name.startsWith("chromium"),
    "desktop command-palette keyboard contract",
  );

  const originalSessionId = (await spawnSmokeShell(smokePage, stack.workerFp)).session_id;
  await navigateToSmokeSession(smokePage, originalSessionId);

  await keyboardActivateCoreAction(
    smokePage,
    "global search sessions workspaces workers git ports",
    "Search all sessions",
  );
  await expect(smokePage).toHaveURL(`${stack.baseUrl}/search`);
  await expect(smokePage.getByTestId(`terminal-slot-${originalSessionId}`)).toHaveCount(1);

  await smokePage.evaluate((sessionId) => {
    const smokeWindow = window as unknown as CommandPaletteSmokeWindow;
    smokeWindow.__smoke.navigate(`/s/${sessionId}`);
  }, originalSessionId);
  await expect(smokePage.getByTestId(`terminal-slot-${originalSessionId}`)).toBeVisible();

  await keyboardActivateCoreAction(
    smokePage,
    "attention blocked done unseen agents",
    "Open attention",
  );
  await expect(smokePage).toHaveURL(`${stack.baseUrl}/search?scope=attention`);

  await smokePage.evaluate((sessionId) => {
    const smokeWindow = window as unknown as CommandPaletteSmokeWindow;
    smokeWindow.__smoke.navigate(`/s/${sessionId}`);
  }, originalSessionId);
  await expect(smokePage.getByTestId(`terminal-slot-${originalSessionId}`)).toBeVisible();

  await keyboardActivateCoreAction(
    smokePage,
    "queue task /tmp",
    "Queue task for this folder",
  );
  const taskEditor = smokePage.getByTestId("task-editor");
  await expect(taskEditor).toBeVisible();
  await expect.poll(() => smokePage.getByTestId("task-editor-cwd").evaluate(
    (element) => {
      const valueHost = element as HTMLElement & { value: string };
      return String(valueHost.value);
    },
  )).toBe("/tmp");
  await expect.poll(() => smokePage.getByTestId("task-editor-worker").evaluate(
    (element) => {
      const valueHost = element as HTMLElement & { value: string };
      return String(valueHost.value);
    },
  )).toBe(stack.workerFp);
  await taskEditor.getByText("Cancel", { exact: true }).click();
  await expect(taskEditor).toHaveCount(0);

  await keyboardActivateCoreAction(
    smokePage,
    "new terminal sibling /tmp",
    "New sibling terminal",
  );
  await expect.poll(() => new URL(smokePage.url()).pathname).not.toBe(
    `/s/${originalSessionId}`,
  );
  const siblingSessionId = new URL(smokePage.url()).pathname.match(/^\/s\/([^/]+)$/)?.[1];
  if (!siblingSessionId) throw new Error("sibling action did not navigate to a session");
  await smokePage.evaluate((sessionId) => {
    const smokeWindow = window as unknown as CommandPaletteSmokeWindow;
    smokeWindow.__smoke.trackCreatedSession(sessionId);
  }, siblingSessionId);
  await expect(smokePage.getByTestId(`terminal-slot-${siblingSessionId}`)).toBeVisible({
    timeout: 30_000,
  });
});
