import { fileURLToPath } from "node:url";
import { expect, waitForConfirmedDashboardScope } from "./fixtures.ts";
import type { Page } from "@playwright/test";
import type { TerminalTestStack, TerminalTestWorker } from "./stack.ts";
import { dirname, join } from "node:path";
import {
  detectBrowserPlatform,
  matchesPlatformShortcut,
  type BrowserPlatform,
  type PlatformShortcutId,
} from "../../apps/web/src/lib/browserPlatform.ts";
import type {
  TerminalInputCapture,
  RecoverySmokeApi,
  RecoveryProbeResult,
} from "./terminal-smoke-api.ts";

export const fixturePath = join(dirname(fileURLToPath(import.meta.url)), "resize-tui.ts");

// Resolve shortcuts from the page's navigator and the product's authoritative
// matcher rather than assuming the runner OS or hard-coding one platform's chord.
const shortcutPlatformByPage = new WeakMap<Page, Promise<BrowserPlatform>>();

export function shortcutPlatform(page: Page): Promise<BrowserPlatform> {
  let detected = shortcutPlatformByPage.get(page);
  if (!detected) {
    detected = page.evaluate(() => {
      const nav = navigator as Navigator & { userAgentData?: { platform?: string } };
      return {
        userAgent: nav.userAgent,
        platform: nav.platform,
        userAgentData: { platform: nav.userAgentData?.platform },
      };
    }).then(detectBrowserPlatform);
    shortcutPlatformByPage.set(page, detected);
  }
  return detected;
}

export async function pressPlatformShortcut(
  page: Page,
  shortcut: PlatformShortcutId,
  key: string,
): Promise<void> {
  const platform = await shortcutPlatform(page);
  // Put the native primary modifier first so the first accepted combination is
  // the same idiomatic chord exposed by the product (Meta outside Windows).
  const modifierOrder = platform === "windows"
    ? ["Alt", "Control", "Shift", "Meta"] as const
    : ["Meta", "Control", "Alt", "Shift"] as const;
  for (let mask = 0; mask < 1 << modifierOrder.length; mask += 1) {
    const modifiers = modifierOrder.filter((_, index) => (mask & (1 << index)) !== 0);
    if (!matchesPlatformShortcut({
      key,
      ctrlKey: modifiers.includes("Control"),
      altKey: modifiers.includes("Alt"),
      shiftKey: modifiers.includes("Shift"),
      metaKey: modifiers.includes("Meta"),
      getModifierState: () => false,
    }, shortcut, platform)) continue;
    await page.keyboard.press([...modifiers, key].join("+"));
    return;
  }
  throw new Error(`No ${platform} keyboard chord matches ${shortcut} with key ${key}`);
}

function fixtureWorkerFolder(worker: TerminalTestWorker): string {
  return process.platform === "win32" ? worker.home.replaceAll("\\", "/") : worker.home;
}

export async function spawnPtyFixtureSession(page: Page, worker: TerminalTestWorker): Promise<string> {
  await waitForConfirmedDashboardScope(page);
  await page.waitForFunction((workerFp) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return !!smokeWindow.__smoke.state().workers[workerFp];
  }, worker.workerFp);
  return page.evaluate(async ({ workerFp, folder }) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return (await smokeWindow.__smoke.spawnShell(workerFp, folder)).session_id;
  }, { workerFp: worker.workerFp, folder: fixtureWorkerFolder(worker) });
}

export async function spawnSmokeShell(page: Page, workerFp: string, sessionId?: string) {
  await waitForConfirmedDashboardScope(page);
  return page.evaluate(async ({ workerFp: fp, sessionId: sid }) => {
    const smoke = (window as unknown as { __smoke: RecoverySmokeApi }).__smoke;
    const session = await smoke.spawnShell(fp, "/tmp", sid);
    await smoke.createWorkspace(fp, "/tmp", session.session_id);
    return session;
  }, { workerFp, sessionId });
}

export async function navigateToSmokeSession(page: Page, sessionId: string): Promise<void> {
  await page.goto(`${new URL(page.url()).origin}/s/${sessionId}`, { waitUntil: "domcontentloaded" });
  // 30s, not the config's 10s expect budget: this is the readiness gate every
  // spec crosses before its real assertions — a cold navigation must sync the
  // session into the store first, and a loaded CI runner overran 10s with the
  // SPA still showing "No session selected". Specs that assert navigation
  // LATENCY do it explicitly (perf.spec.ts); here a slow arrival is not a bug,
  // and a session that never arrives still fails.
  await expect(page.getByTestId(`terminal-slot-${sessionId}`)).toBeVisible({ timeout: 30_000 });
}

export async function switchToSmokeSession(page: Page, sessionId: string): Promise<void> {
  await page.evaluate(
    (id) => (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.navigate(`/s/${id}`),
    sessionId,
  );
  await expect(page.getByTestId(`terminal-slot-${sessionId}`)).toBeVisible();
}

export async function expectSmokeComposer(page: Page): Promise<void> {
  await expect(page.getByTestId("mobile-chat-input")).toBeVisible();
  await expect(page.getByTestId("chat-box")).toBeVisible();
  await expect(page.getByTestId("chat-input")).toBeVisible();
}

export async function readWorkerBytes(
  client: TerminalTestStack["client"],
  workerFp: string,
  path: string,
): Promise<Uint8Array> {
  const parts: Uint8Array[] = [];
  let offset = 0;
  for (;;) {
    const response = await client.filesReadChunk({
      workerFp,
      path,
      offset: BigInt(offset),
      len: 4 * 1024 * 1024,
    });
    if (response.data.length > 0) {
      parts.push(response.data);
      offset += response.data.length;
    }
    if (response.eof || response.data.length === 0) break;
  }
  const bytes = new Uint8Array(offset);
  let writeOffset = 0;
  for (const part of parts) {
    bytes.set(part, writeOffset);
    writeOffset += part.length;
  }
  return bytes;
}

export async function readStoredComposerDraft(
  page: Page,
  sessionId: string,
): Promise<{ present: boolean; value: string | null }> {
  return page.evaluate((id) => {
    try {
      const raw = localStorage.getItem("roost.composerDrafts.v1");
      const parsed: unknown = raw ? JSON.parse(raw) : {};
      if (!parsed || typeof parsed !== "object" || !Object.prototype.hasOwnProperty.call(parsed, id)) {
        return { present: false, value: null };
      }
      const value = Reflect.get(parsed, id);
      return { present: true, value: typeof value === "string" ? value : null };
    } catch {
      return { present: false, value: null };
    }
  }, sessionId);
}

export async function inputSmokeTerminal(page: Page, sessionId: string, text: string): Promise<void> {
  await page.evaluate(async ({ id, input }) => {
    // The smoke backdoor is injected only in smoke-enabled browser contexts.
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    await smokeWindow.__smoke.input(id, input);
  }, { id: sessionId, input: text });
}

export async function resetTerminalInputCapture(page: Page): Promise<void> {
  await page.evaluate(() => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    smokeWindow.__smoke.resetTerminalInputCapture();
  });
}

export async function readTerminalInputCapture(page: Page): Promise<TerminalInputCapture> {
  return page.evaluate(() => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return smokeWindow.__smoke.terminalInputCapture();
  });
}

export async function waitForStableCellFrames(page: Page, sessionId: string): Promise<void> {
  let previous = -1;
  let unchangedPolls = 0;
  await expect.poll(async () => {
    const current = await page.evaluate(
      (id) => (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.cellFrameCount(id),
      sessionId,
    );
    if (current === previous) unchangedPolls += 1;
    else {
      previous = current;
      unchangedPolls = 0;
    }
    return unchangedPolls;
  }, { timeout: 3_000, intervals: [50] }).toBeGreaterThanOrEqual(3);
}

export async function setRecoveryCanary(page: Page, canary: string): Promise<void> {
  await page.evaluate((value) => { document.documentElement.dataset.terminalStreamCanary = value; }, canary);
}

export async function recoveryProbe(page: Page, sessionId: string, prefix: string): Promise<RecoveryProbeResult> {
  return page.evaluate(({ sessionId: id, prefix: markerPrefix }) => ({
    canary: document.documentElement.dataset.terminalStreamCanary ?? null,
    scan: (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.markerScan(id, markerPrefix),
    atBottom: (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.renderProbe(id).atBottom,
  }), { sessionId, prefix });
}

export function expectCleanRecovery(
  result: RecoveryProbeResult,
  canary: string,
  min: number,
  max: number,
): void {
  expect(result.canary).toBe(canary);
  expect(result.scan).toMatchObject({
    min,
    max,
    duplicated: [],
    missing: 0,
    outOfOrder: 0,
  });
  expect(result.atBottom).toBe(true);
}

/** Trusted paste from the system clipboard.
 *  NOT keyboard.press("Meta+V"): on macOS the OS owns that accelerator, so
 *  Chromium under this driver delivers no paste event at all (the armed paint
 *  sample came back null on CI's macos-latest terminal job). CDP's `commands`
 *  is the hook Chromium itself uses to run editing accelerators, so it yields
 *  one trusted paste event on every platform. */
export async function pasteFromClipboard(page: Page): Promise<void> {
  const key = { key: "v", code: "KeyV", windowsVirtualKeyCode: 86, nativeVirtualKeyCode: 86 } as const;
  const cdp = await page.context().newCDPSession(page);
  try {
    await cdp.send("Input.dispatchKeyEvent", { type: "keyDown", modifiers: 4, commands: ["paste"], ...key });
    await cdp.send("Input.dispatchKeyEvent", { type: "keyUp", modifiers: 4, ...key });
  } finally {
    await cdp.detach();
  }
}
