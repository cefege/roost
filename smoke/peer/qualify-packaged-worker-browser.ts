// Browser UI proof for the packaged-worker release driver.
// It enrolls ordinary production pages and drives visible terminal input without smoke hooks.
// The main driver owns stack lifecycle and uses these helpers for direct and Sync assertions.

import { expect, type Browser, type BrowserContext, type Page } from "@playwright/test";
import type { TerminalTestStack } from "../terminal/stack.ts";
import { installDisabledLoopbackProbe } from "../terminal/stack-browser-faults.ts";

const UI_TIMEOUT_MS = 60_000;

export type PackagedWorkerPage = {
  readonly page: Page;
  close(): Promise<void>;
};

export async function openPackagedWorkerPage(
  browser: Browser,
  stack: TerminalTestStack,
  options: { disableWebRtc: boolean },
): Promise<PackagedWorkerPage> {
  const context = await createProductionContext(
    browser,
    stack.disableLoopbackProbe,
    options.disableWebRtc,
  );
  const page = await context.newPage();
  try {
    const token = (await stack.client.authMintBootstrap({
      kind: "browser",
      label: options.disableWebRtc ? "roost-packaged-worker-sync" : "roost-packaged-worker-peer",
    })).token;
    await page.goto(`${stack.baseUrl}/#pair=${encodeURIComponent(token)}`, {
      waitUntil: "domcontentloaded",
      timeout: UI_TIMEOUT_MS,
    });
    await page.waitForFunction(() => location.hash === "", undefined, { timeout: UI_TIMEOUT_MS });
    await expect(page.getByTestId("folder-list")).toBeVisible({ timeout: UI_TIMEOUT_MS });
    await expect(page.getByTestId("error-boundary")).toHaveCount(0);
    return {
      page,
      close: async () => {
        await context.close();
      },
    };
  } catch (error) {
    await context.close();
    throw error;
  }
}

export async function openPackagedWorkerSession(
  page: Page,
  stack: TerminalTestStack,
  sessionId: string,
): Promise<void> {
  await page.goto(`${stack.baseUrl}/s/${sessionId}`, {
    waitUntil: "domcontentloaded",
    timeout: UI_TIMEOUT_MS,
  });
  const slot = page.getByTestId(`terminal-slot-${sessionId}`);
  await expect(slot).toBeVisible({ timeout: UI_TIMEOUT_MS });
  await expect(slot.getByTestId("terminal-display")).toBeVisible({ timeout: UI_TIMEOUT_MS });
}

export async function expectPackagedWorkerTransport(
  page: Page,
  sessionId: string,
  transport: "sync" | "webrtc",
  timeoutMs = UI_TIMEOUT_MS,
): Promise<void> {
  await expect(page.getByTestId(`tab-${sessionId}`)).toHaveAttribute(
    "data-terminal-transport",
    transport,
    { timeout: timeoutMs },
  );
}

export async function enterPackagedWorkerCommand(
  page: Page,
  sessionId: string,
  command: string,
  marker: string,
): Promise<void> {
  await page.bringToFront();
  const slot = page.getByTestId(`terminal-slot-${sessionId}`);
  await page.evaluate(() => {
    document.body.tabIndex = -1;
    document.body.focus();
  });
  await page.keyboard.type(command);
  await page.keyboard.press("Enter");
  await expect(slot).toContainText(marker, { timeout: UI_TIMEOUT_MS });
}

async function createProductionContext(
  browser: Browser,
  disableLoopbackProbe: boolean,
  disableWebRtc: boolean,
): Promise<BrowserContext> {
  const context = await browser.newContext();
  await context.addInitScript(() => {
    localStorage.setItem("roost.whatsNew.lastSeenVersion", "2.0.0");
  });
  if (disableLoopbackProbe) await installDisabledLoopbackProbe(context);
  if (disableWebRtc) {
    await context.addInitScript(() => {
      Object.defineProperty(window, "RTCPeerConnection", {
        configurable: true,
        value: undefined,
      });
    });
  }
  return context;
}
