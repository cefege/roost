import { mkdirSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { setTimeout as delay } from "node:timers/promises";
import { fromBinary } from "@bufbuild/protobuf";
import { test, expect } from "./fixtures.ts";
import type { Page } from "@playwright/test";
import type { TerminalTestStack } from "./stack.ts";
import { dirname, join } from "node:path";
import { FilesListDirRequestSchema } from "../../apps/shared/src/gen/roost/v1/coordinator_pb.ts";
import { encodeFolderPath } from "../../apps/web/src/lib/terminalHref.ts";
import {
  detectBrowserPlatform,
  matchesPlatformShortcut,
  type BrowserPlatform,
  type PlatformShortcutId,
} from "../../apps/web/src/lib/browserPlatform.ts";

const fixturePath = join(dirname(fileURLToPath(import.meta.url)), "resize-tui.ts");

// Playwright's Desktop Chrome profile currently advertises a Windows browser
// even when the test runner itself is hosted elsewhere. Resolve shortcuts from
// the page's navigator and the product's authoritative matcher rather than
// assuming the runner OS or hard-coding one platform's chord.
const shortcutPlatformByPage = new WeakMap<Page, Promise<BrowserPlatform>>();

function shortcutPlatform(page: Page): Promise<BrowserPlatform> {
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

async function pressPlatformShortcut(
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

interface RecoveryMarkerScan {
  total: number; unique: number; min: number; max: number;
  duplicated: number[]; missing: number; outOfOrder: number; firstInversion: number;
}

interface TerminalInputCapture {
  batches: Array<{ sessionId: string; data: number[] }>;
  droppedBatches: number;
}

interface SmokeSessionProjection {
  id: string;
  worker_fp: string;
  cwd?: string;
  spawn_cwd?: string;
}

interface RecoverySmokeApi {
  spawnShell(workerFp: string, folder: string, sessionId?: string): Promise<{ session_id: string; channel_id: number }>;
  state(): { sessions: Record<string, SmokeSessionProjection> };
  createWorkspace(workerFp: string, folder: string, sessionId: string): Promise<{ id: string; channel: number }>;
  navigate(href: string): void;
  input(sessionId: string, text: string): Promise<void>;
  terminalInputCapture(): TerminalInputCapture;
  resetTerminalInputCapture(): void;
  dropNextCellFrame(sessionId: string): void;
  droppedCellFrameCount(sessionId: string): number;
  cellFrameCount(sessionId: string): number;
  cellFullFrameCount(sessionId: string): number;
  cellGridEpoch(sessionId: string): string;
  lastFullFrameSbRows(sessionId: string): number;
  syncWsGeneration(): number;
  pauseSyncTransport(): void;
  resumeSyncTransport(): void;
  forceSyncRetryExhausted(): void;
  viewportText(sessionId: string): string;
  markerScan(sessionId: string, prefix: string): RecoveryMarkerScan;
  renderProbe(sessionId: string): { atBottom: boolean };
}

interface RecoveryProbeResult {
  canary: string | null;
  scan: RecoveryMarkerScan;
  atBottom: boolean;
}

async function spawnSmokeShell(page: Page, workerFp: string, sessionId?: string) {
  return page.evaluate(async ({ workerFp: fp, sessionId: sid }) => {
    const smoke = (window as unknown as { __smoke: RecoverySmokeApi }).__smoke;
    const session = await smoke.spawnShell(fp, "/tmp", sid);
    await smoke.createWorkspace(fp, "/tmp", session.session_id);
    return session;
  }, { workerFp, sessionId });
}

async function navigateToSmokeSession(page: Page, sessionId: string): Promise<void> {
  await page.goto(`${new URL(page.url()).origin}/s/${sessionId}`, { waitUntil: "domcontentloaded" });
  await expect(page.getByTestId(`terminal-slot-${sessionId}`)).toBeVisible();
}

async function switchToSmokeSession(page: Page, sessionId: string): Promise<void> {
  await page.evaluate(
    (id) => (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.navigate(`/s/${id}`),
    sessionId,
  );
  await expect(page.getByTestId(`terminal-slot-${sessionId}`)).toBeVisible();
}

async function expectSmokeComposer(page: Page): Promise<void> {
  await expect(page.getByTestId("mobile-chat-input")).toBeVisible();
  await expect(page.getByTestId("chat-box")).toBeVisible();
  await expect(page.getByTestId("chat-input")).toBeVisible();
}

async function readWorkerBytes(
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

async function readStoredComposerDraft(
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
async function inputSmokeTerminal(page: Page, sessionId: string, text: string): Promise<void> {
  await page.evaluate(async ({ id, input }) => {
    // The smoke backdoor is injected only in smoke-enabled browser contexts.
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    await smokeWindow.__smoke.input(id, input);
  }, { id: sessionId, input: text });
}

async function resetTerminalInputCapture(page: Page): Promise<void> {
  await page.evaluate(() => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    smokeWindow.__smoke.resetTerminalInputCapture();
  });
}

async function readTerminalInputCapture(page: Page): Promise<TerminalInputCapture> {
  return page.evaluate(() => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return smokeWindow.__smoke.terminalInputCapture();
  });
}


async function waitForStableCellFrames(page: Page, sessionId: string): Promise<void> {
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

async function setRecoveryCanary(page: Page, canary: string): Promise<void> {
  await page.evaluate((value) => { document.documentElement.dataset.terminalStreamCanary = value; }, canary);
}

async function recoveryProbe(page: Page, sessionId: string, prefix: string): Promise<RecoveryProbeResult> {
  return page.evaluate(({ sessionId: id, prefix: markerPrefix }) => ({
    canary: document.documentElement.dataset.terminalStreamCanary ?? null,
    scan: (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.markerScan(id, markerPrefix),
    atBottom: (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.renderProbe(id).atBottom,
  }), { sessionId, prefix });
}

function expectCleanRecovery(
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
test("browser smoke flow creates and cleans its resources", async ({ smokePage }) => {
  const result = await smokePage.evaluate(async () => {
    const smoke = (window as unknown as Window & { __smoke: { runFlow(): Promise<{ steps: Array<{ pass: boolean }>; summary: string }> } }).__smoke;
    return smoke.runFlow();
  });
  expect(result.steps.filter((step) => !step.pass)).toEqual([]);
});

test("new-terminal server switch resets browse path before listing and spawning", async ({
  multiWorkerSmokePage,
  stack,
  secondWorker,
}, testInfo) => {
  test.skip(testInfo.project.name !== "chromium-desktop", "desktop multi-worker browse contract");

  const suffix = crypto.randomUUID().slice(0, 8);
  const aChildName = `a-only-${suffix}`;
  const aChildPath = join(stack.workerHome, aChildName);
  const bDefaultPath = join(secondWorker.home, `b-default-${suffix}`);
  mkdirSync(aChildPath, { recursive: true });
  mkdirSync(bDefaultPath, { recursive: true });

  const bSeedSessionId = await multiWorkerSmokePage.evaluate(async ({ workerFp, cwd }) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return (await smokeWindow.__smoke.spawnShell(workerFp, cwd)).session_id;
  }, { workerFp: secondWorker.workerFp, cwd: bDefaultPath });
  await multiWorkerSmokePage.waitForFunction((sessionId) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return !!smokeWindow.__smoke.state().sessions[sessionId]?.cwd;
  }, bSeedSessionId);

  // FlatNewTerminal chooses the globally newest session. Cross a timestamp
  // boundary, then seed A so the sidebar-scoped plus deterministically opens A.
  await delay(10);
  const aSeedSessionId = await multiWorkerSmokePage.evaluate(async ({ workerFp, cwd }) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return (await smokeWindow.__smoke.spawnShell(workerFp, cwd)).session_id;
  }, { workerFp: stack.workerFp, cwd: stack.workerHome });
  await multiWorkerSmokePage.waitForFunction((sessionId) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return !!smokeWindow.__smoke.state().sessions[sessionId]?.cwd;
  }, aSeedSessionId);

  const seedCwds = await multiWorkerSmokePage.evaluate(({ aId, bId }) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    const sessions = smokeWindow.__smoke.state().sessions;
    return { a: sessions[aId]?.cwd, b: sessions[bId]?.cwd };
  }, { aId: aSeedSessionId, bId: bSeedSessionId });
  expect(seedCwds).toEqual({ a: stack.workerHome, b: bDefaultPath });

  const listRequests: Array<{ workerFp: string; path: string }> = [];
  const decodeErrors: string[] = [];
  multiWorkerSmokePage.on("request", (request) => {
    if (!new URL(request.url()).pathname.endsWith("/roost.v1.CoordinatorService/FilesListDir")) return;
    const body = request.postDataBuffer();
    if (!body) {
      decodeErrors.push("FilesListDir request had no body");
      return;
    }
    try {
      const decoded = fromBinary(FilesListDirRequestSchema, body);
      listRequests.push({ workerFp: decoded.workerFp, path: decoded.path });
    } catch (error) {
      decodeErrors.push(String(error));
    }
  });

  await multiWorkerSmokePage
    .getByTestId("folder-list")
    .getByTestId("flat-new-terminal-button")
    .click();
  await expect(multiWorkerSmokePage).toHaveURL(`${stack.baseUrl}/browse/${stack.workerFp}`);
  await expect(multiWorkerSmokePage.getByTestId("browse-server")).toHaveAttribute("title", "roost-terminal-test");
  await expect(multiWorkerSmokePage.getByTestId("browse-crumb").last()).toHaveAttribute("title", stack.workerHome);

  const aFolder = multiWorkerSmokePage
    .locator('[data-testid="browse-tile"], [data-testid="browse-row"]')
    .filter({ hasText: aChildName });
  await expect(aFolder).toHaveCount(1);
  await aFolder.click();
  await expect(multiWorkerSmokePage.getByTestId("browse-crumb").last()).toHaveAttribute("title", aChildPath);
  await expect(multiWorkerSmokePage.getByTestId("browse-back")).toBeEnabled();

  await multiWorkerSmokePage.getByTestId("browse-server").click();
  await multiWorkerSmokePage
    .getByTestId("browse-server-option")
    .filter({ hasText: secondWorker.label })
    .click();
  await expect(multiWorkerSmokePage).toHaveURL(`${stack.baseUrl}/browse/${secondWorker.workerFp}`);
  await expect(multiWorkerSmokePage.getByTestId("browse-server")).toHaveAttribute("title", secondWorker.label);
  await expect(multiWorkerSmokePage.getByTestId("browse-crumb").last()).toHaveAttribute("title", bDefaultPath);
  await expect(multiWorkerSmokePage.getByTestId("browse-back")).toBeDisabled();

  await expect.poll(
    () => listRequests.some((request) =>
      request.workerFp === secondWorker.workerFp && request.path === bDefaultPath),
  ).toBe(true);
  expect(decodeErrors).toEqual([]);
  expect(listRequests).not.toContainEqual({ workerFp: secondWorker.workerFp, path: aChildPath });

  await multiWorkerSmokePage.getByTestId("browse-open").click();
  await expect(multiWorkerSmokePage).toHaveURL(
    `${stack.baseUrl}/t/${secondWorker.workerFp}/${encodeFolderPath(bDefaultPath)}`,
  );
  await expect.poll(() => multiWorkerSmokePage.evaluate(({ workerFp, cwd, seedId }) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    const sessions = smokeWindow.__smoke.state().sessions;
    return Object.values(sessions).filter((session) =>
      session.id !== seedId
      && session.worker_fp === workerFp
      && session.cwd === cwd
      && session.spawn_cwd === cwd
    ).length;
  }, { workerFp: secondWorker.workerFp, cwd: bDefaultPath, seedId: bSeedSessionId })).toBe(1);
});

test("dropped initial full frame reclaims immediately on the first delta", async ({ smokePage, stack }, testInfo) => {
  test.skip(testInfo.project.name !== "chromium-desktop", "desktop cell recovery contract");
  const sessionId = crypto.randomUUID();
  const canary = `initial-full-${sessionId}`;
  const spawned = await smokePage.evaluate(async ({ workerFp, sessionId: id }) => {
    const smoke = (window as unknown as { __smoke: RecoverySmokeApi }).__smoke;
    smoke.dropNextCellFrame(id);
    return smoke.spawnShell(workerFp, "/tmp", id);
  }, { workerFp: stack.workerFp, sessionId });
  expect(spawned.session_id).toBe(sessionId);
  await navigateToSmokeSession(smokePage, sessionId);
  await expect.poll(() => smokePage.evaluate(
    (id) => (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.droppedCellFrameCount(id),
    sessionId,
  )).toBe(1);
  await setRecoveryCanary(smokePage, canary);

  await smokePage.evaluate(
    async (id) => (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.input(id, "printf 'INITIAL-RECOVER-%03d\\n' 1\r"),
    sessionId,
  );
  await expect.poll(() => smokePage.evaluate(
    (id) => (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.viewportText(id),
    sessionId,
  ), { timeout: 10_000, intervals: [50] }).toContain("INITIAL-RECOVER-001");
  expectCleanRecovery(await recoveryProbe(smokePage, sessionId, "INITIAL-RECOVER-"), canary, 1, 1);
});

test("dropped streaming delta recovers before the producer goes quiet", async ({ smokePage, stack }, testInfo) => {
  test.skip(testInfo.project.name !== "chromium-desktop", "desktop cell recovery contract");
  const sessionId = (await spawnSmokeShell(smokePage, stack.workerFp)).session_id;
  await navigateToSmokeSession(smokePage, sessionId);
  await expect.poll(() => smokePage.evaluate(
    (id) => (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.cellFullFrameCount(id),
    sessionId,
  )).toBeGreaterThan(0);
  const canary = `middle-delta-${sessionId}`;
  await setRecoveryCanary(smokePage, canary);

  await smokePage.evaluate(async (id) => {
    const smoke = (window as unknown as { __smoke: RecoverySmokeApi }).__smoke;
    smoke.dropNextCellFrame(id);
    await smoke.input(
      id,
      "i=1; while [ \"$i\" -le 80 ]; do printf 'STREAM-RECOVER-%03d\\n' \"$i\"; i=$((i+1)); sleep 0.03; done\r",
    );
  }, sessionId);
  await expect.poll(() => smokePage.evaluate(
    (id) => (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.droppedCellFrameCount(id),
    sessionId,
  )).toBe(1);
  await expect.poll(() => smokePage.evaluate(
    (id) => (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.markerScan(id, "STREAM-RECOVER-").max,
    sessionId,
  ), { timeout: 10_000, intervals: [50] }).toBeGreaterThanOrEqual(20);
  const earlyMax = await smokePage.evaluate(
    (id) => (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.markerScan(id, "STREAM-RECOVER-").max,
    sessionId,
  );
  expect(earlyMax).toBeLessThan(80);
  await expect.poll(() => smokePage.evaluate(
    (id) => (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.markerScan(id, "STREAM-RECOVER-").max,
    sessionId,
  ), { timeout: 15_000, intervals: [50] }).toBe(80);
  expectCleanRecovery(await recoveryProbe(smokePage, sessionId, "STREAM-RECOVER-"), canary, 1, 80);
});

test("dropped final frame is repaired by the applied-sequence heartbeat", async ({ smokePage, stack }, testInfo) => {
  test.skip(testInfo.project.name !== "chromium-desktop", "desktop cell recovery contract");
  test.setTimeout(75_000);
  const sessionId = (await spawnSmokeShell(smokePage, stack.workerFp)).session_id;
  await navigateToSmokeSession(smokePage, sessionId);
  await smokePage.evaluate(
    async (id) => (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.input(id, "stty -echo; printf 'HEARTBEAT-READY-%03d\\n' 1\r"),
    sessionId,
  );
  await expect.poll(() => smokePage.evaluate(
    (id) => (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.viewportText(id),
    sessionId,
  )).toContain("HEARTBEAT-READY-001");
  await expect.poll(() => smokePage.evaluate((id) => {
    // The test bootstrap installs this typed in-process harness on window.
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return /HEARTBEAT-READY-001bash-\d+(?:\.\d+)+\$/.test(
      smokeWindow.__smoke.viewportText(id),
    );
  }, sessionId)).toBe(true);
  await waitForStableCellFrames(smokePage, sessionId);
  await smokePage.evaluate(
    async (id) => (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.input(
      id,
      "read _; printf 'HEARTBEAT-RECOVER-%03d\\n' 1; read _\r",
    ),
    sessionId,
  );
  await waitForStableCellFrames(smokePage, sessionId);
  const canary = `heartbeat-${sessionId}`;
  await setRecoveryCanary(smokePage, canary);
  const before = await smokePage.evaluate((id) => ({
    frames: (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.cellFrameCount(id),
    fullFrames: (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.cellFullFrameCount(id),
    wsGeneration: (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.syncWsGeneration(),
  }), sessionId);

  await smokePage.evaluate(async (id) => {
    const smoke = (window as unknown as { __smoke: RecoverySmokeApi }).__smoke;
    smoke.dropNextCellFrame(id);
    await smoke.input(id, "go\r");
  }, sessionId);
  await expect.poll(() => smokePage.evaluate(
    (id) => (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.droppedCellFrameCount(id),
    sessionId,
  )).toBe(1);
  expect(await smokePage.evaluate(
    (id) => (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.viewportText(id),
    sessionId,
  )).not.toContain("HEARTBEAT-RECOVER-001");

  await expect.poll(() => smokePage.evaluate(
    (id) => (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.viewportText(id),
    sessionId,
  ), { timeout: 45_000, intervals: [250] }).toContain("HEARTBEAT-RECOVER-001");
  const after = await smokePage.evaluate((id) => ({
    fullFrames: (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.cellFullFrameCount(id),
    wsGeneration: (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.syncWsGeneration(),
  }), sessionId);
  expect(after.fullFrames).toBe(before.fullFrames + 1);
  expect(after.wsGeneration).toBe(before.wsGeneration);
  expectCleanRecovery(await recoveryProbe(smokePage, sessionId, "HEARTBEAT-RECOVER-"), canary, 1, 1);
  await smokePage.evaluate(async (id) => {
    const smoke = (window as unknown as { __smoke: RecoverySmokeApi }).__smoke;
    await smoke.input(id, "go\r");
    await smoke.input(id, "stty echo\r");
  }, sessionId);
});

test("parking a selection-held pane flushes its latest folded frame", async ({ smokePage, stack }, testInfo) => {
  test.skip(testInfo.project.name !== "chromium-desktop", "desktop paint-hold contract");
  const sessionId = (await spawnSmokeShell(smokePage, stack.workerFp)).session_id;
  await navigateToSmokeSession(smokePage, sessionId);
  await expect.poll(() => smokePage.evaluate(
    (id) => (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.cellFullFrameCount(id),
    sessionId,
  )).toBeGreaterThan(0);
  const canary = `selection-hold-${sessionId}`;
  await setRecoveryCanary(smokePage, canary);
  const selected = await smokePage.evaluate((id) => {
    const slot = document.querySelector(`[data-testid="terminal-slot-${id}"]`);
    const row = Array.from(slot?.querySelectorAll(".cell-row") ?? [])
      .find((candidate) => (candidate.textContent ?? "").length > 0);
    if (!row) return false;
    const walker = document.createTreeWalker(row, NodeFilter.SHOW_TEXT);
    const text = walker.nextNode();
    if (!text || !text.textContent) return false;
    const range = document.createRange();
    range.setStart(text, 0);
    range.setEnd(text, 1);
    const selection = window.getSelection();
    selection?.removeAllRanges();
    selection?.addRange(range);
    document.dispatchEvent(new Event("selectionchange"));
    return !!selection && !selection.isCollapsed;
  }, sessionId);
  expect(selected).toBe(true);
  const beforeFrames = await smokePage.evaluate(
    (id) => (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.cellFrameCount(id),
    sessionId,
  );
  await smokePage.evaluate(
    async (id) => (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.input(id, "printf 'HOLD-RECOVER-%03d\\n' 1\r"),
    sessionId,
  );
  await expect.poll(() => smokePage.evaluate(
    (id) => (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.cellFrameCount(id),
    sessionId,
  )).toBeGreaterThan(beforeFrames);
  expect(await smokePage.getByTestId(`terminal-slot-${sessionId}`).textContent())
    .not.toContain("HOLD-RECOVER-001");

  const otherSessionId = (await spawnSmokeShell(smokePage, stack.workerFp)).session_id;
  await switchToSmokeSession(smokePage, otherSessionId);
  await switchToSmokeSession(smokePage, sessionId);
  await expect.poll(() => smokePage.evaluate(
    (id) => (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.viewportText(id),
    sessionId,
  )).toContain("HOLD-RECOVER-001");
  expect(await smokePage.evaluate(() => window.getSelection()?.isCollapsed ?? true)).toBe(true);
  expectCleanRecovery(await recoveryProbe(smokePage, sessionId, "HOLD-RECOVER-"), canary, 1, 1);
});

test("terminal replay and Ctrl keys stay owned by the PTY", async ({ smokePage, stack }, testInfo) => {
  test.skip(testInfo.project.name !== "chromium-desktop", "desktop keyboard contract");
  const sessionId = await smokePage.evaluate(async (workerFp) => {
    const smoke = (window as unknown as Window & { __smoke: { spawnShell(worker: string, folder: string): Promise<{ session_id: string }> } }).__smoke;
    return (await smoke.spawnShell(workerFp, "/tmp")).session_id;
  }, stack.workerFp);
  await smokePage.goto(`${stack.baseUrl}/s/${sessionId}`);
  const slot = smokePage.getByTestId(`terminal-slot-${sessionId}`);
  await expect(slot).toBeVisible();
  const composer = slot.getByTestId("mobile-chat-input");
  await expect(composer).toBeVisible();
  await expect(composer.getByTestId("chat-box")).toBeVisible();
  await expect(composer.getByTestId("chat-input")).not.toBeFocused();

  await smokePage.evaluate(() => {
    document.body.tabIndex = -1;
    document.body.focus();
  });
  const marker = `NATIVE_FOCUS_${crypto.randomUUID().replace(/-/g, "").slice(0, 8)}`;
  await smokePage.keyboard.type(`printf '%s\\n' ${marker}`);
  await smokePage.keyboard.press("Enter");
  await expect.poll(() => slot.textContent()).toContain(marker);

  const sidebar = smokePage.getByTestId("sidebar-desktop");
  const collapsed = await sidebar.getAttribute("data-collapsed");
  await smokePage.keyboard.type("cat -v");
  await smokePage.keyboard.press("Enter");
  await smokePage.evaluate(() => {
    document.body.tabIndex = -1;
    document.body.focus();
  });

  await smokePage.keyboard.down("Control");
  await smokePage.keyboard.press("b");
  await smokePage.keyboard.press("f");
  await smokePage.keyboard.up("Control");

  await smokePage.evaluate(() => {
    document.body.tabIndex = -1;
    document.body.focus();
  });

  await smokePage.keyboard.down("Control");
  await smokePage.keyboard.press("k");
  await smokePage.keyboard.up("Control");
  await smokePage.keyboard.press("Enter");
  await expect.poll(() => slot.textContent()).toContain("^B^F^K");
  await expect(sidebar).toHaveAttribute("data-collapsed", collapsed ?? "false");
  await expect(smokePage.getByTestId("sidebar-search")).toHaveCount(0);
  await expect(smokePage.getByTestId("command-palette")).toHaveCount(0);
  await smokePage.keyboard.press("Control+C");
});

test("desktop active terminal keeps a permanent reserved composer after send", async ({ smokePage, stack }, testInfo) => {
  test.skip(testInfo.project.name !== "chromium-desktop", "desktop composer contract");
  const sessionId = (await spawnSmokeShell(smokePage, stack.workerFp)).session_id;
  await navigateToSmokeSession(smokePage, sessionId);

  const slot = smokePage.getByTestId(`terminal-slot-${sessionId}`);
  const display = slot.getByTestId("terminal-display");
  const dock = slot.getByTestId("mobile-chat-input");
  const box = dock.getByTestId("chat-box");
  const input = box.getByTestId("chat-input");
  const mic = box.getByTestId("mobile-voice-input");
  const send = box.getByTestId("chat-send");

  await expect(dock).toBeVisible();
  await expect(display).toBeVisible();
  await expect(box).toBeVisible();
  await expect(input).toBeVisible();
  await expect(input).not.toBeFocused();
  await expect(mic).toBeVisible();
  await expect(mic.getByTestId("voice-mic")).toBeVisible();
  await expect(send).toBeVisible();

  // The only voice control is the inline mic in the permanent bar. Desktop no
  // longer exposes the old corner toggle, standalone voice FAB, or nav pad.
  await expect(smokePage.getByTestId("mobile-voice-input")).toHaveCount(1);
  await expect(smokePage.getByTestId("voice-mic")).toHaveCount(1);
  await expect(smokePage.getByTestId("terminal-chat-toggle")).toHaveCount(0);
  await expect(smokePage.getByTestId("terminal-nav-toggle")).toHaveCount(0);
  await expect(smokePage.getByTestId("terminal-nav-buttons")).toHaveCount(0);

  await box.evaluate(async (el) => {
    await Promise.all(el.getAnimations().map((animation) => animation.finished));
  });
  const readGeometry = () => smokePage.evaluate((id) => {
    const slot = document.querySelector(`[data-testid="terminal-slot-${id}"]`);
    const terminal = slot?.querySelector('[data-testid="terminal-display"]');
    const composer = slot?.querySelector('[data-testid="mobile-chat-input"]');
    if (!(terminal instanceof HTMLElement) || !(composer instanceof HTMLElement)) return null;

    const terminalRect = terminal.getBoundingClientRect();
    const composerRect = composer.getBoundingClientRect();
    const sidebar = document.querySelector('[data-testid="sidebar-desktop"]');
    const sidebarRect = sidebar?.getBoundingClientRect();
    const viewportBottom = window.visualViewport
      ? window.visualViewport.offsetTop + window.visualViewport.height
      : window.innerHeight;
    return {
      terminalBottom: terminalRect.bottom,
      terminalHeight: terminalRect.height,
      composerTop: composerRect.top,
      composerHeight: composerRect.height,
      bottomGap: viewportBottom - composerRect.bottom,
      composerLeft: composerRect.left,
      sidebarRight: sidebarRect?.right ?? 0,
    };
  }, sessionId);

  await expect.poll(async () => {
    const geometry = await readGeometry();
    return geometry !== null
      && geometry.terminalHeight > 0
      && geometry.composerHeight > 0
      && geometry.terminalBottom <= geometry.composerTop;
  }, { message: "desktop terminal content must end above the composer dock" }).toBe(true);
  await expect.poll(async () => {
    const geometry = await readGeometry();
    return geometry ? Math.abs(geometry.bottomGap - 8) : Number.POSITIVE_INFINITY;
  }, { message: "desktop composer must rest about 8px above the viewport bottom" }).toBeLessThanOrEqual(4);
  await expect.poll(async () => {
    const geometry = await readGeometry();
    return geometry ? geometry.composerLeft - geometry.sidebarRight : Number.NEGATIVE_INFINITY;
  }, { message: "desktop composer must stay inside the terminal pane" }).toBeGreaterThanOrEqual(4);

  const marker = `DESKTOP_COMPOSER_${crypto.randomUUID().replace(/-/g, "").slice(0, 8)}`;
  await input.fill(`printf '%s\\n' ${marker}`);
  await expect(input).toBeFocused();
  await send.click();
  await expect.poll(() => slot.textContent()).toContain(marker);
  await expect(input).toHaveValue("");
  await expect(dock).toBeVisible();
  await expect(box).toBeVisible();
  await expect(box).toHaveCount(1);
});

test("desktop split panes each own and route their composer", async ({ smokePage, stack }, testInfo) => {
  test.skip(testInfo.project.name !== "chromium-desktop", "desktop pane composer contract");
  const initialSessionId = (await spawnSmokeShell(smokePage, stack.workerFp)).session_id;
  await navigateToSmokeSession(smokePage, initialSessionId);

  await pressPlatformShortcut(smokePage, "splitRight", "D");

  const paneSlots = smokePage.locator("[data-pane-slot]");
  await expect(paneSlots).toHaveCount(2);
  await expect(paneSlots.nth(0)).toBeVisible();
  await expect(paneSlots.nth(1)).toBeVisible();

  const composers = smokePage.getByTestId("mobile-chat-input");
  await expect(composers).toHaveCount(2);
  await expect(composers.nth(0)).toBeVisible();
  await expect(composers.nth(1)).toBeVisible();

  const sessionIds = await paneSlots.evaluateAll((slots) => slots.map((slot) => {
    const testId = slot.getAttribute("data-testid") ?? "";
    return testId.startsWith("terminal-slot-")
      ? testId.slice("terminal-slot-".length)
      : "";
  }));
  expect(sessionIds).toHaveLength(2);
  expect(sessionIds).toContain(initialSessionId);
  expect(sessionIds.every(Boolean)).toBe(true);
  expect(new Set(sessionIds).size).toBe(2);

  const sessionA = sessionIds[0]!;
  const sessionB = sessionIds[1]!;
  const slotA = smokePage.getByTestId(`terminal-slot-${sessionA}`);
  const slotB = smokePage.getByTestId(`terminal-slot-${sessionB}`);
  const dockA = slotA.getByTestId("mobile-chat-input");
  const dockB = slotB.getByTestId("mobile-chat-input");
  const inputA = dockA.getByTestId("chat-input");
  const inputB = dockB.getByTestId("chat-input");
  const sendA = dockA.getByTestId("chat-send");
  const sendB = dockB.getByTestId("chat-send");

  await expect(dockA).toHaveAttribute("data-placement", "pane");
  await expect(dockB).toHaveAttribute("data-placement", "pane");
  await expect(dockA).toBeVisible();
  await expect(dockB).toBeVisible();

  const suffix = crypto.randomUUID().replace(/-/g, "").slice(0, 8);
  const markerA = `SPLIT_COMPOSER_A_${suffix}`;
  const markerB = `SPLIT_COMPOSER_B_${suffix}`;

  await inputA.click();
  await expect(inputA).toBeFocused();
  await expect(slotA).toHaveAttribute("data-focused", "true");
  await inputA.fill(`printf '%s\\n' ${markerA}`);
  await sendA.click();
  await expect.poll(() => slotA.textContent()).toContain(markerA);
  await expect(slotB).not.toContainText(markerA);
  await expect(slotA).not.toContainText(markerB);
  await expect(slotB).not.toContainText(markerB);
  await expect(dockA).toBeVisible();
  await expect(dockB).toBeVisible();

  await inputB.click();
  await expect(inputB).toBeFocused();
  await expect(slotB).toHaveAttribute("data-focused", "true");
  await expect(dockA).toBeVisible();
  await expect(dockB).toBeVisible();
  await inputB.fill(`printf '%s\\n' ${markerB}`);
  await sendB.click();
  await expect.poll(() => slotB.textContent()).toContain(markerB);
  await expect(slotA).not.toContainText(markerB);
  await expect(slotB).not.toContainText(markerA);
  await expect(dockA).toBeVisible();
  await expect(dockB).toBeVisible();

  const focusSlotABox = await slotA.boundingBox();
  const focusSlotBBox = await slotB.boundingBox();
  expect(focusSlotABox).not.toBeNull();
  expect(focusSlotBBox).not.toBeNull();
  const targetSlot = focusSlotABox!.x < focusSlotBBox!.x ? slotB : slotA;
  const walkKey = focusSlotABox!.x < focusSlotBBox!.x
    ? "ArrowRight"
    : "ArrowLeft";

  // Keyboard pane navigation overrides a composer focused in the pane being
  // left. The unsent draft stays there; subsequent native input belongs to the
  // newly focused PTY.
  const heldDraft = `held draft ${suffix}`;
  await inputA.click();
  await inputA.fill(heldDraft);
  await pressPlatformShortcut(smokePage, "paneFocus", walkKey);
  await expect(targetSlot).toHaveAttribute("data-focused", "true");
  const focusedMarker = `SPLIT_COMPOSER_FOCUS_${suffix}`;
  await smokePage.keyboard.type(`printf '%s\\n' ${focusedMarker}`);
  await smokePage.keyboard.press("Enter");
  await expect.poll(() => targetSlot.textContent()).toContain(focusedMarker);
  await expect(inputA).toHaveValue(heldDraft);
  await inputA.click();
  await inputA.fill("");

  // A pointer-only send claims before the deck's ancestor handler, but must
  // release again when focus remained in the hidden PTY textarea.
  await slotA.getByTestId("terminal-display").click();
  await sendA.click();
  await pressPlatformShortcut(smokePage, "paneFocus", walkKey);
  await expect(targetSlot).toHaveAttribute("data-focused", "true");
  const keyboardMarker = `SPLIT_COMPOSER_KEYBOARD_${suffix}`;
  await smokePage.keyboard.type(`printf '%s\\n' ${keyboardMarker}`);
  await smokePage.keyboard.press("Enter");
  await expect.poll(() => targetSlot.textContent()).toContain(keyboardMarker);

  // Releasing outside an inactive pane's send control still ends its transient
  // pointer claim. The window-level pointer-end watcher sees the off-dock release.
  await slotA.getByTestId("terminal-display").click();
  const sendBBox = await sendB.boundingBox();
  const displayABox = await slotA.getByTestId("terminal-display").boundingBox();
  expect(sendBBox).not.toBeNull();
  expect(displayABox).not.toBeNull();
  await smokePage.mouse.move(
    sendBBox!.x + sendBBox!.width / 2,
    sendBBox!.y + sendBBox!.height / 2,
  );
  await smokePage.mouse.down();
  await smokePage.mouse.move(
    displayABox!.x + displayABox!.width / 2,
    displayABox!.y + displayABox!.height / 2,
    { steps: 4 },
  );
  await smokePage.mouse.up();
  await expect(slotB).toHaveAttribute("data-focused", "true");
  const dragMarker = `SPLIT_COMPOSER_DRAG_${suffix}`;
  await smokePage.keyboard.type(`printf '%s\\n' ${dragMarker}`);
  await smokePage.keyboard.press("Enter");
  await expect.poll(() => slotB.textContent()).toContain(dragMarker);


  // A legal 10% divider position must reflow, not paint one session's buttons
  // into its neighbor where they would become wrong-session hit targets.
  const deckBox = await smokePage.getByTestId("terminal-deck").boundingBox();
  const divider = smokePage.locator('[data-testid^="pane-divider-"]').first();
  const dividerBox = await divider.boundingBox();
  expect(deckBox).not.toBeNull();
  expect(dividerBox).not.toBeNull();
  expect(await divider.getAttribute("data-dir")).toBe("row");
  await smokePage.mouse.move(
    dividerBox!.x + dividerBox!.width / 2,
    dividerBox!.y + dividerBox!.height / 2,
  );
  await smokePage.mouse.down();
  await smokePage.mouse.move(
    deckBox!.x + deckBox!.width * 0.1,
    dividerBox!.y + dividerBox!.height / 2,
    { steps: 6 },
  );
  await smokePage.mouse.up();

  const slotABox = await slotA.boundingBox();
  const slotBBox = await slotB.boundingBox();
  expect(slotABox).not.toBeNull();
  expect(slotBBox).not.toBeNull();
  const narrowIsA = slotABox!.width < slotBBox!.width;
  const narrowSlot = narrowIsA ? slotA : slotB;
  const wideSlot = narrowIsA ? slotB : slotA;
  const narrowDock = narrowSlot.getByTestId("mobile-chat-input");
  const escapedControls = await narrowDock.evaluate((dock) => {
    const slot = dock.closest("[data-pane-slot]");
    if (!(slot instanceof HTMLElement)) return ["missing pane slot"];
    const bounds = slot.getBoundingClientRect();
    const controls = dock.querySelectorAll(
      '[data-testid="chat-attach"], [data-testid="chat-input"], [data-testid="voice-mic"], [data-testid="chat-send"]',
    );
    return Array.from(controls).flatMap((control) => {
      const rect = control.getBoundingClientRect();
      return rect.left < bounds.left - 1 || rect.right > bounds.right + 1
        ? [control.getAttribute("data-testid") ?? control.tagName]
        : [];
    });
  });
  expect(escapedControls).toEqual([]);

  const narrowInput = narrowDock.getByTestId("chat-input");
  const markerNarrow = `SPLIT_COMPOSER_NARROW_${suffix}`;
  await narrowInput.fill(`printf '%s\\n' ${markerNarrow}`);
  await narrowDock.getByTestId("chat-send").click();
  await expect.poll(() => narrowSlot.textContent()).toContain(markerNarrow);

  // A keyboard new-tab command overrides the focused composer but preserves its
  // parked draft; native keystrokes must belong to the spawned terminal.
  const parkedDraft = `parked new-tab draft ${suffix}`;
  await wideSlot.getByTestId("terminal-display").click();
  await expect(wideSlot).toHaveAttribute("data-focused", "true");
  await narrowInput.evaluate((input) => input.focus());
  await narrowInput.fill(parkedDraft);
  await expect(narrowSlot).toHaveAttribute("data-focused", "true");
  await pressPlatformShortcut(smokePage, "newTerminal", "T");
  await expect.poll(() => paneSlots.evaluateAll((slots) => slots.map((slot) =>
    (slot.getAttribute("data-testid") ?? "").replace(/^terminal-slot-/, ""),
  ).filter(Boolean).length)).toBe(3);
  const spawnedSessionId = await paneSlots.evaluateAll((slots, existingIds) =>
    slots.map((slot) =>
      (slot.getAttribute("data-testid") ?? "").replace(/^terminal-slot-/, ""),
    ).find((id) => id && !existingIds.includes(id)),
  [sessionA, sessionB]);
  expect(spawnedSessionId).toBeTruthy();
  const spawnedSlot = smokePage.getByTestId(`terminal-slot-${spawnedSessionId!}`);
  await expect(spawnedSlot).toHaveAttribute("data-focused", "true");
  const spawnedMarker = `SPLIT_COMPOSER_NEWTAB_${suffix}`;
  await smokePage.keyboard.type(`printf '%s\\n' ${spawnedMarker}`);
  await smokePage.keyboard.press("Enter");
  await expect.poll(() => spawnedSlot.textContent()).toContain(spawnedMarker);
  await expect(narrowInput).toHaveValue(parkedDraft);
});

test("desktop composer attaches exact files in order without submitting", async ({ smokePage, stack }, testInfo) => {
  test.skip(testInfo.project.name !== "chromium-desktop", "desktop native attachment contract");
  const sessionId = (await spawnSmokeShell(smokePage, stack.workerFp)).session_id;
  await navigateToSmokeSession(smokePage, sessionId);

  const suffix = crypto.randomUUID().replaceAll("-", "").slice(0, 8);
  const firstName = `attach-first-${suffix}.bin`;
  const secondName = `attach-second-${suffix}.txt`;
  const firstBytes = Buffer.alloc(4 * 1024 * 1024 + 31);
  for (let i = 0; i < firstBytes.length; i++) firstBytes[i] = (i * 17 + suffix.charCodeAt(i % suffix.length)) & 0xff;
  const secondBytes = Buffer.from(`second-${suffix}\nline-two\0tail`, "utf8");

  const slot = smokePage.getByTestId(`terminal-slot-${sessionId}`);
  const dock = slot.getByTestId("mobile-chat-input");
  const box = dock.getByTestId("chat-box");
  const attach = box.getByTestId("chat-attach");
  const input = box.getByTestId("chat-input");
  await expect(attach).toBeVisible();
  await expect(attach).toHaveAttribute("aria-label", "Attach files");
  const [attachBox, inputBox] = await Promise.all([attach.boundingBox(), input.boundingBox()]);
  expect(attachBox).not.toBeNull();
  expect(inputBox).not.toBeNull();
  expect(attachBox!.x + attachBox!.width).toBeLessThanOrEqual(inputBox!.x);

  await input.fill("draft remains untouched");
  await expect(input).toBeFocused();
  const [slotBefore, dockBefore] = await Promise.all([slot.boundingBox(), dock.boundingBox()]);
  expect(slotBefore).not.toBeNull();
  expect(dockBefore).not.toBeNull();

  await resetTerminalInputCapture(smokePage);

  const [chooser] = await Promise.all([
    smokePage.waitForEvent("filechooser"),
    attach.click(),
  ]);
  await chooser.setFiles([
    { name: firstName, mimeType: "application/octet-stream", buffer: firstBytes },
    { name: secondName, mimeType: "text/plain", buffer: secondBytes },
  ]);

  await expect(input).toBeFocused();
  await expect(input).toHaveValue("draft remains untouched");
  await expect.poll(async () => {
    const response = await stack.client.listAttachments({ sessionId });
    return [firstName, secondName].every((name) => response.entries.some((entry) => entry.filename === name));
  }, { timeout: 20_000 }).toBe(true);

  const entries = (await stack.client.listAttachments({ sessionId })).entries;
  const firstEntry = entries.find((entry) => entry.filename === firstName);
  const secondEntry = entries.find((entry) => entry.filename === secondName);
  expect(firstEntry).toBeDefined();
  expect(secondEntry).toBeDefined();
  expect(firstEntry!.sizeBytes).toBe(BigInt(firstBytes.length));
  expect(secondEntry!.sizeBytes).toBe(BigInt(secondBytes.length));

  const [storedFirst, storedSecond] = await Promise.all([
    readWorkerBytes(stack.client, stack.workerFp, firstEntry!.absPath),
    readWorkerBytes(stack.client, stack.workerFp, secondEntry!.absPath),
  ]);
  expect(storedFirst).toEqual(new Uint8Array(firstBytes));
  expect(storedSecond).toEqual(new Uint8Array(secondBytes));

  const expectedInput = `${firstEntry!.absPath} ${secondEntry!.absPath} `;
  const expectedBytes = Array.from(new TextEncoder().encode(expectedInput));
  await expect.poll(async () => {
    const capture = await readTerminalInputCapture(smokePage);
    return capture.batches.reduce((total, batch) => total + batch.data.length, 0);
  }).toBe(expectedBytes.length);
  const inputCapture = await readTerminalInputCapture(smokePage);
  expect(inputCapture.droppedBatches).toBe(0);
  expect(inputCapture.batches.every((batch) => batch.sessionId === sessionId)).toBe(true);
  const injected = inputCapture.batches.flatMap((batch) => batch.data);
  expect(injected).toEqual(expectedBytes);
  expect(new TextDecoder().decode(Uint8Array.from(injected))).toBe(expectedInput);
  expect(injected).not.toContain(13);
  expect(injected).not.toContain(10);

  const [slotAfter, dockAfter] = await Promise.all([slot.boundingBox(), dock.boundingBox()]);
  expect(slotAfter).not.toBeNull();
  expect(dockAfter).not.toBeNull();
  for (const key of ["x", "y", "width", "height"] as const) {
    expect(Math.abs(slotAfter![key] - slotBefore![key]), `terminal ${key}`).toBeLessThanOrEqual(1);
    expect(Math.abs(dockAfter![key] - dockBefore![key]), `composer ${key}`).toBeLessThanOrEqual(1);
  }
});

test("desktop composer submits Enter and grows above a stable terminal deck", async ({ smokePage, stack }, testInfo) => {
  test.skip(testInfo.project.name !== "chromium-desktop", "desktop composer keyboard and geometry contract");
  const sessionId = (await spawnSmokeShell(smokePage, stack.workerFp)).session_id;
  await navigateToSmokeSession(smokePage, sessionId);

  const deck = smokePage.getByTestId("terminal-deck");
  const slot = smokePage.getByTestId(`terminal-slot-${sessionId}`);
  const dock = slot.getByTestId("mobile-chat-input");
  const box = dock.getByTestId("chat-box");
  const input = dock.getByTestId("chat-input");
  await expect(deck).toBeVisible();
  await expect(dock).toBeVisible();
  await expect(box).toBeVisible();
  await expect(input).toBeVisible();
  await box.evaluate(async (el) => {
    await Promise.all(el.getAnimations().map((animation) => animation.finished));
  });

  const readGeometry = () => smokePage.evaluate((id) => {
    const deckEl = document.querySelector('[data-testid="terminal-deck"]');
    const slotEl = document.querySelector(`[data-testid="terminal-slot-${id}"]`);
    const terminalEl = slotEl?.querySelector('[data-testid="terminal-display"]');
    const composerEl = slotEl?.querySelector('[data-testid="mobile-chat-input"]');
    if (
      !(deckEl instanceof HTMLElement)
      || !(slotEl instanceof HTMLElement)
      || !(terminalEl instanceof HTMLElement)
      || !(composerEl instanceof HTMLElement)
    ) return null;

    const slotRect = slotEl.getBoundingClientRect();
    const terminalRect = terminalEl.getBoundingClientRect();
    const composerRect = composerEl.getBoundingClientRect();
    return {
      deckClientHeight: deckEl.clientHeight,
      slotHeight: slotRect.height,
      terminalBottom: terminalRect.bottom,
      composerTop: composerRect.top,
      composerHeight: composerRect.height,
    };
  }, sessionId);

  await input.fill("one-row draft");
  await expect(input).toBeFocused();
  const baseline = await readGeometry();
  if (!baseline) throw new Error("desktop composer geometry was unavailable at one row");
  expect(baseline.deckClientHeight).toBeGreaterThan(0);
  expect(baseline.slotHeight).toBeGreaterThan(0);
  expect(baseline.composerHeight).toBeGreaterThan(0);

  const growthDraft = [
    "first composer row",
    "second composer row",
    "third composer row",
    "fourth composer row",
    "fifth composer row",
  ].join("\n");
  await input.fill(growthDraft);
  await expect(input).toHaveValue(growthDraft);
  await expect.poll(async () => {
    const geometry = await readGeometry();
    return geometry && geometry.composerHeight > baseline.composerHeight + 1
      ? geometry.terminalBottom - geometry.composerTop
      : Number.POSITIVE_INFINITY;
  }, {
    message: "grown desktop composer must move terminal content above its top",
  }).toBeLessThanOrEqual(0);

  const grown = await readGeometry();
  if (!grown) throw new Error("desktop composer geometry was unavailable after growth");
  expect(grown.composerHeight).toBeGreaterThan(baseline.composerHeight + 1);
  expect(
    Math.abs(grown.deckClientHeight - baseline.deckClientHeight),
    "composer growth must not resize TerminalDeck",
  ).toBeLessThanOrEqual(1);
  expect(
    Math.abs(grown.slotHeight - baseline.slotHeight),
    "composer growth must not resize the terminal slot",
  ).toBeLessThanOrEqual(1);
  expect(grown.terminalBottom, "terminal visual bottom must stay at or above the composer").toBeLessThanOrEqual(
    grown.composerTop,
  );

  const shiftMarker = `DESKTOP_SHIFT_${crypto.randomUUID().replace(/-/g, "").slice(0, 8)}`;
  const shiftCommand = `printf '%s\\n' ${shiftMarker}`;
  await input.fill(shiftCommand);
  await input.press("Shift+Enter");
  await expect(input).toHaveValue(`${shiftCommand}\n`);

  // A command sent directly after Shift+Enter is a PTY ordering barrier: once
  // it is visible, a mistakenly submitted Shift+Enter command would be visible too.
  const barrier = `DESKTOP_SHIFT_BARRIER_${crypto.randomUUID().replace(/-/g, "").slice(0, 8)}`;
  await inputSmokeTerminal(smokePage, sessionId, `printf '%s\\n' ${barrier}\n`);
  await expect.poll(() => slot.textContent()).toContain(barrier);
  expect((await slot.textContent()) ?? "").not.toContain(shiftMarker);

  const enterMarker = `DESKTOP_ENTER_${crypto.randomUUID().replace(/-/g, "").slice(0, 8)}`;
  await input.fill(`printf '%s\\n' ${enterMarker}`);
  await expect(input).toBeFocused();
  await input.press("Enter");
  await expect.poll(() => slot.textContent()).toContain(enterMarker);
  await expect(input).toHaveValue("");
  await expect(input).toBeFocused();
  await expect(dock).toBeVisible();
  await expect(box).toBeVisible();
  await expect(box).toHaveCount(1);
});

test("mobile composer starts unfocused and reserves terminal space through rotation", async ({ mobileSmokePage, stack }) => {
  const sessionId = (await spawnSmokeShell(mobileSmokePage, stack.workerFp)).session_id;
  await navigateToSmokeSession(mobileSmokePage, sessionId);

  const dock = mobileSmokePage.getByTestId("mobile-chat-input");
  const input = mobileSmokePage.getByTestId("chat-input");
  await expect(dock).toBeVisible();
  await expect(input).toBeVisible();
  await expect(input).not.toBeFocused();
  await mobileSmokePage.getByTestId("chat-box")
    .evaluate(async (el) => { await Promise.all(el.getAnimations().map((animation) => animation.finished)); });

  const initialViewport = mobileSmokePage.viewportSize();
  expect(initialViewport).not.toBeNull();

  const readGeometry = () => mobileSmokePage.evaluate((id) => {
    const terminal = document.querySelector(`[data-testid="terminal-slot-${id}"]`);
    const composer = document.querySelector('[data-testid="mobile-chat-input"]');
    if (!(terminal instanceof HTMLElement) || !(composer instanceof HTMLElement)) return null;

    const safeAreaProbe = document.createElement("div");
    safeAreaProbe.style.cssText = [
      "position:fixed",
      "visibility:hidden",
      "pointer-events:none",
      "padding-bottom:env(safe-area-inset-bottom, 0px)",
    ].join(";");
    document.body.append(safeAreaProbe);
    const measuredSafeArea = Number.parseFloat(getComputedStyle(safeAreaProbe).paddingBottom);
    safeAreaProbe.remove();

    const terminalRect = terminal.getBoundingClientRect();
    const composerRect = composer.getBoundingClientRect();
    const keyboardOffset = Number.parseFloat(
      getComputedStyle(document.documentElement).getPropertyValue("--kb-offset"),
    );
    const viewportBottom = window.visualViewport
      ? window.visualViewport.offsetTop + window.visualViewport.height
      : window.innerHeight;
    return {
      terminalBottom: terminalRect.bottom,
      terminalHeight: terminalRect.height,
      composerTop: composerRect.top,
      composerHeight: composerRect.height,
      bottomGap: viewportBottom - composerRect.bottom,
      safeAreaBottom: Number.isFinite(measuredSafeArea) ? measuredSafeArea : 0,
      keyboardOffset: Number.isFinite(keyboardOffset) ? keyboardOffset : 0,
    };
  }, sessionId);

  const expectReservedGeometry = async (label: string) => {
    await expect.poll(async () => {
      const geometry = await readGeometry();
      if (!geometry || geometry.terminalHeight <= 0 || geometry.composerHeight <= 0) {
        return Number.POSITIVE_INFINITY;
      }
      return Math.abs(geometry.terminalBottom - geometry.composerTop);
    }, { message: `${label}: terminal bottom must meet the composer top` }).toBeLessThanOrEqual(2);
    await expect.poll(async () => (await readGeometry())?.keyboardOffset ?? Number.POSITIVE_INFINITY, {
      message: `${label}: keyboard offset must clear without a simulated keyboard`,
    }).toBe(0);
    await expect.poll(async () => {
      const geometry = await readGeometry();
      return geometry
        ? Math.abs(geometry.bottomGap - geometry.safeAreaBottom - 8)
        : Number.POSITIVE_INFINITY;
    }, {
      message: `${label}: resting dock must sit about 8px above the measured safe area`,
    }).toBeLessThanOrEqual(4);
    const geometry = await readGeometry();
    expect(geometry, `${label}: visible terminal and composer geometry`).not.toBeNull();
    return geometry!;
  };

  const initial = await expectReservedGeometry("initial portrait");
  const portrait = initialViewport!;
  await mobileSmokePage.setViewportSize({ width: portrait.height, height: portrait.width });
  await expectReservedGeometry("landscape");
  await mobileSmokePage.setViewportSize(portrait);
  await expectReservedGeometry("restored portrait");

  // Crossing the real compact boundary swaps the body portal for this pane's
  // inline instance. Instance-token cleanup must not clear the replacement's
  // focus owner or leave the viewport reserve stuck on desktop.
  await mobileSmokePage.setViewportSize({ width: 1024, height: 700 });
  const slot = mobileSmokePage.getByTestId(`terminal-slot-${sessionId}`);
  const paneDock = slot.getByTestId("mobile-chat-input");
  await expect(paneDock).toBeVisible();
  await expect(paneDock).toHaveAttribute("data-placement", "pane");
  await expect.poll(() => paneDock.evaluate((el) => getComputedStyle(el).position)).toBe("relative");
  await mobileSmokePage.setViewportSize(portrait);
  await expect(mobileSmokePage.getByTestId("mobile-chat-input")).toHaveAttribute("data-placement", "viewport");
  await expectReservedGeometry("portrait after desktop handoff");
  await expect.poll(async () => {
    const geometry = await readGeometry();
    return geometry ? Math.abs(geometry.terminalHeight - initial.terminalHeight) : Number.POSITIVE_INFINITY;
  }, { message: "portrait terminal height must recover after rotation" }).toBeLessThanOrEqual(2);

  await expect(input).not.toBeFocused();
  await input.tap();
  await expect(input).toBeFocused();
});

// Compact composition is structurally two rows from its first paint. The field
// always owns the full first row; its content never decides whether the actions
// consume message width. Explicit lines and ordinary wrapping therefore share
// one stable wrap width while only the top of the fixed dock moves.
test("mobile composer gives multiline drafts the full message width", async ({ mobileSmokePage, stack }) => {
  const sessionId = (await spawnSmokeShell(mobileSmokePage, stack.workerFp)).session_id;
  await navigateToSmokeSession(mobileSmokePage, sessionId);

  const box = mobileSmokePage.getByTestId("chat-box");
  const input = mobileSmokePage.getByTestId("chat-input");

  const readComposerGeometry = () => mobileSmokePage.evaluate(() => {
    const dockEl = document.querySelector('[data-testid="mobile-chat-input"]');
    const boxEl = document.querySelector('[data-testid="chat-box"]');
    const inputEl = document.querySelector('[data-testid="chat-input"]');
    const micEl = document.querySelector('[data-testid="voice-mic"]');
    const sendEl = document.querySelector('[data-testid="chat-send"]');
    if (
      !(dockEl instanceof HTMLElement)
      || !(boxEl instanceof HTMLElement)
      || !(inputEl instanceof HTMLTextAreaElement)
      || !(micEl instanceof HTMLElement)
      || !(sendEl instanceof HTMLElement)
    ) return null;

    const rect = (el: HTMLElement) => {
      const value = el.getBoundingClientRect();
      return {
        x: value.x,
        y: value.y,
        top: value.top,
        right: value.right,
        bottom: value.bottom,
        left: value.left,
        width: value.width,
        height: value.height,
      };
    };
    const px = (value: string) => {
      const parsed = Number.parseFloat(value);
      return Number.isFinite(parsed) ? parsed : 0;
    };
    const resolveMaxHeight = (value: string) => {
      const probe = document.createElement("div");
      probe.style.cssText = [
        "position:fixed",
        "visibility:hidden",
        "pointer-events:none",
        "box-sizing:border-box",
        "height:10000px",
      ].join(";");
      probe.style.maxHeight = value;
      document.body.append(probe);
      const height = probe.getBoundingClientRect().height;
      probe.remove();
      return height;
    };

    const boxRect = rect(boxEl);
    const boxStyle = getComputedStyle(boxEl);
    const inputStyle = getComputedStyle(inputEl);
    const paddingLeft = px(boxStyle.paddingLeft);
    const paddingRight = px(boxStyle.paddingRight);
    return {
      dock: rect(dockEl),
      box: boxRect,
      input: rect(inputEl),
      mic: rect(micEl),
      send: rect(sendEl),
      innerLeft: boxRect.left + paddingLeft,
      innerRight: boxRect.right - paddingRight,
      paddingTop: px(boxStyle.paddingTop),
      paddingBottom: px(boxStyle.paddingBottom),
      rowGap: px(boxStyle.rowGap),
      columnGap: px(boxStyle.columnGap),
      inputClientHeight: inputEl.clientHeight,
      inputScrollHeight: inputEl.scrollHeight,
      lineHeight: px(inputStyle.lineHeight),
      overflowY: inputStyle.overflowY,
      maxHeight: resolveMaxHeight(inputStyle.maxHeight),
      contractMaxHeight: resolveMaxHeight("min(192px, 30dvh)"),
    };
  });

  const geometry = async () => {
    const value = await readComposerGeometry();
    expect(value, "compact composer geometry").not.toBeNull();
    return value!;
  };
  const near = (actual: number, expected: number, tolerance: number, label: string) =>
    expect(Math.abs(actual - expected), label).toBeLessThanOrEqual(tolerance);

  // The entrance animation translates the pill. Finish it before recording the
  // fixed footer coordinates used throughout the growth assertions.
  await box.evaluate(async (el) => {
    await Promise.all(el.getAnimations().map((animation) => animation.finished));
  });
  await input.fill("short");
  const baseline = await geometry();

  const expectFullWidthRows = (
    current: typeof baseline,
    label: string,
  ) => {
    const innerWidth = current.innerRight - current.innerLeft;
    near(current.input.left, current.innerLeft, 1, `${label}: textarea left`);
    near(current.input.width, innerWidth, 1, `${label}: textarea uses the box inner width`);
    near(current.input.right, current.innerRight, 1, `${label}: textarea right`);
    expect(
      current.input.right - current.mic.left,
      `${label}: textarea continues across the mic and send columns`,
    ).toBeGreaterThanOrEqual(current.mic.width + current.send.width - 2);
    expect(
      current.input.right - current.send.left,
      `${label}: textarea continues across the send column`,
    ).toBeGreaterThanOrEqual(current.send.width - 2);

    near(current.mic.top, current.send.top, 1, `${label}: action row top`);
    near(current.mic.bottom, current.send.bottom, 1, `${label}: action row bottom`);
    near(current.send.right, current.innerRight, 1, `${label}: footer is right-aligned`);
    near(
      current.send.left - current.mic.right,
      current.columnGap,
      1,
      `${label}: action gap`,
    );
    near(
      current.mic.top - current.input.bottom,
      current.rowGap,
      1,
      `${label}: textarea-to-footer gap`,
    );
    expect(current.mic.top, `${label}: mic is below the textarea`).toBeGreaterThan(
      current.input.bottom,
    );
  };
  const expectStableFooter = (
    current: typeof baseline,
    label: string,
  ) => {
    for (const control of ["mic", "send"] as const) {
      for (const dimension of ["x", "y", "width", "height"] as const) {
        near(
          current[control][dimension],
          baseline[control][dimension],
          1,
          `${label}: stable ${control}.${dimension}`,
        );
      }
    }
    near(current.box.left, baseline.box.left, 1, `${label}: stable box left`);
    near(current.box.width, baseline.box.width, 1, `${label}: stable box width`);
    near(current.input.left, baseline.input.left, 1, `${label}: stable textarea left`);
    near(current.input.width, baseline.input.width, 1, `${label}: stable textarea width`);
  };

  expectFullWidthRows(baseline, "resting");
  near(baseline.input.height, 40, 1, "resting textarea height");
  near(baseline.mic.height, 44, 1, "resting mic height");
  near(baseline.send.height, 44, 1, "resting send height");
  near(baseline.rowGap, 4, 0.5, "resting row gap");
  near(baseline.paddingTop + baseline.paddingBottom, 8, 1, "resting outer padding");
  near(baseline.box.height, 96, 1, "settled compact box height");
  near(baseline.dock.height, 96, 1, "settled compact dock height");

  const explicitLines = [
    "first explicit line",
    "second explicit line",
    "third explicit line",
    "fourth explicit line",
  ].join("\n");
  await input.fill(explicitLines);
  await expect(input).toHaveValue(explicitLines);
  const multiline = await geometry();
  expectFullWidthRows(multiline, "explicit newlines");
  expectStableFooter(multiline, "explicit newlines");
  expect(multiline.input.height, "explicit newlines grow the textarea").toBeGreaterThan(
    baseline.input.height + multiline.lineHeight,
  );
  expect(multiline.input.height, "explicit newlines remain below the cap").toBeLessThan(
    multiline.maxHeight - 1,
  );
  expect(multiline.dock.height, "explicit newlines grow the dock").toBeGreaterThan(
    baseline.dock.height + 1,
  );
  expect(multiline.dock.top, "the growing dock moves upward").toBeLessThan(
    baseline.dock.top - 1,
  );
  near(multiline.dock.bottom, baseline.dock.bottom, 1, "explicit newlines: stable dock bottom");

  // Deliberately abundant prose guarantees overflow without choosing a string
  // near any wrap threshold; text width never selects a different layout.
  const wrappingDraft = (
    "ordinary wrapping keeps every line as wide as the message surface "
  ).repeat(80);
  await input.fill(wrappingDraft);
  await expect(input).toHaveValue(wrappingDraft);
  const capped = await geometry();
  expectFullWidthRows(capped, "long wrapping draft");
  expectStableFooter(capped, "long wrapping draft");
  near(capped.maxHeight, capped.contractMaxHeight, 1, "compact textarea CSS max-height");
  near(capped.input.height, capped.maxHeight, 1, "long draft is capped at CSS max-height");
  expect(
    capped.inputScrollHeight - capped.inputClientHeight,
    "capped textarea has internally scrollable overflow",
  ).toBeGreaterThan(capped.lineHeight);
  expect(capped.overflowY).toBe("auto");
  expect(capped.dock.height, "wrapped text grows the dock to its cap").toBeGreaterThan(
    multiline.dock.height + 1,
  );
  expect(capped.dock.top, "capped dock grows upward").toBeLessThan(multiline.dock.top - 1);
  near(capped.dock.bottom, baseline.dock.bottom, 1, "long wrapping draft: stable dock bottom");

  await input.fill("short");
  const restored = await geometry();
  expectFullWidthRows(restored, "shortened draft");
  for (const surface of ["dock", "box", "input", "mic", "send"] as const) {
    for (const dimension of ["x", "y", "width", "height"] as const) {
      near(
        restored[surface][dimension],
        baseline[surface][dimension],
        1,
        `shortened draft restores ${surface}.${dimension}`,
      );
    }
  }
});

// The permanent compact composer keeps its field and all three direct actions
// mounted. Outside taps, Escape, and attachment selection must retain its draft.
test("mobile composer is permanent field + attachment + mic + send", async ({ mobileSmokePage, stack }) => {
  const sessionId = (await spawnSmokeShell(mobileSmokePage, stack.workerFp)).session_id;
  await navigateToSmokeSession(mobileSmokePage, sessionId);

  const suffix = crypto.randomUUID().replaceAll("-", "").slice(0, 8);
  const fileName = `compact-attach-${suffix}.txt`;
  const fileBytes = Buffer.from(`compact attachment ${suffix}\n`, "utf8");
  const draft = `retained compact draft ${suffix}`;
  const box = mobileSmokePage.getByTestId("chat-box");
  const input = mobileSmokePage.getByTestId("chat-input");
  const attach = box.getByTestId("chat-attach");
  const slot = mobileSmokePage.getByTestId(`terminal-slot-${sessionId}`);
  await expect(box).toBeVisible();
  await expect(attach).toBeVisible();
  await expect(attach).toHaveAttribute("aria-label", "Attach files");
  await expect(box.getByTestId("mobile-voice-input")).toBeVisible();
  await expect(box.getByTestId("voice-mic")).toBeVisible();
  await expect(box.getByTestId("chat-send")).toBeVisible();
  await expect(mobileSmokePage.getByTestId("chat-lead")).toHaveCount(0);
  await expect(mobileSmokePage.getByTestId("chat-add-menu")).toHaveCount(0);

  await input.fill(draft);
  await expect(input).toBeFocused();
  await slot.click({ position: { x: 8, y: 8 } });
  await expect(box).toBeVisible();
  await expect(input).not.toBeFocused();
  await expect(input).toHaveValue(draft);

  await input.tap();
  await expect(input).toBeFocused();
  await input.press("Escape");
  await expect(box).toBeVisible();
  await expect(input).not.toBeFocused();
  await expect(input).toHaveValue(draft);

  await resetTerminalInputCapture(mobileSmokePage);

  const [chooser] = await Promise.all([
    mobileSmokePage.waitForEvent("filechooser"),
    attach.click(),
  ]);
  await chooser.setFiles([{ name: fileName, mimeType: "text/plain", buffer: fileBytes }]);

  await expect(input).toHaveValue(draft);
  await expect.poll(async () => {
    const response = await stack.client.listAttachments({ sessionId });
    return response.entries.some((entry) => entry.filename === fileName);
  }, { timeout: 20_000 }).toBe(true);

  const entry = (await stack.client.listAttachments({ sessionId })).entries.find(
    (candidate) => candidate.filename === fileName,
  );
  expect(entry).toBeDefined();
  expect(entry!.sizeBytes).toBe(BigInt(fileBytes.length));

  const expectedInput = Array.from(new TextEncoder().encode(`${entry!.absPath} `));
  await expect.poll(async () =>
    (await readTerminalInputCapture(mobileSmokePage)).batches.flatMap((batch) => batch.data)
  ).toEqual(expectedInput);
  const inputCapture = await readTerminalInputCapture(mobileSmokePage);
  expect(inputCapture.droppedBatches).toBe(0);
  expect(inputCapture.batches.every((batch) => batch.sessionId === sessionId)).toBe(true);
  expect(expectedInput).not.toContain(13);
  expect(expectedInput).not.toContain(10);
  await expect(input).toHaveValue(draft);
});

// The three compact actions share one flat 44dp M3 control anatomy.
test("inline composer controls share one compact M3 anatomy", async ({ mobileSmokePage, stack }) => {
  const sessionId = (await spawnSmokeShell(mobileSmokePage, stack.workerFp)).session_id;
  await navigateToSmokeSession(mobileSmokePage, sessionId);

  const box = mobileSmokePage.getByTestId("chat-box");
  const anatomy = (testId: string) => box.getByTestId(testId).evaluate((el) => {
    const style = getComputedStyle(el);
    return {
      width: style.width,
      height: style.height,
      borderRadius: style.borderRadius,
      boxShadow: style.boxShadow,
    };
  });

  await expect(box).toBeVisible();
  const [attach, mic, send] = await Promise.all([
    anatomy("chat-attach"),
    anatomy("voice-mic"),
    anatomy("chat-send"),
  ]);
  for (const [label, control] of [
    ["chat-attach", attach],
    ["voice-mic", mic],
    ["chat-send", send],
  ] as const) {
    expect(control.width, label).toBe("44px");
    expect(control.height, label).toBe("44px");
    expect(control.borderRadius, label).toBe(send.borderRadius);
    expect(control.boxShadow, label).toBe("none");
  }
});

test("mobile terminal keyboard toggles and dispatches special keys", async ({ mobileSmokePage, stack }) => {
  const sessionId = (await spawnSmokeShell(mobileSmokePage, stack.workerFp)).session_id;
  await navigateToSmokeSession(mobileSmokePage, sessionId);
  await expectSmokeComposer(mobileSmokePage);

  const dock = mobileSmokePage.getByTestId("mobile-chat-input");
  const box = mobileSmokePage.getByTestId("chat-box");
  const input = mobileSmokePage.getByTestId("chat-input");
  const toggle = mobileSmokePage.getByTestId("terminal-nav-toggle");
  const panel = mobileSmokePage.getByTestId("terminal-nav-buttons");
  const paneFocused = () => mobileSmokePage.evaluate((id) => {
    const smoke = (window as unknown as Window & {
      __smoke: { paneFocused(sessionId: string): { focused: boolean } };
    }).__smoke;
    return smoke.paneFocused(id).focused;
  }, sessionId);

  expect(await box.evaluate((el) => Array.from(el.children).map((child) =>
    child instanceof HTMLElement ? child.dataset.testid ?? null : null,
  ))).toEqual(["chat-attach", "chat-input", "mobile-voice-input", "chat-send"]);
  await expect(toggle).toBeVisible();
  await expect(toggle).toHaveAttribute("data-open", "false");
  await expect(toggle).toHaveAttribute("aria-label", "Show terminal keys");
  await expect(toggle).toHaveCSS("position", "fixed");
  await expect(toggle).toHaveCSS("width", "56px");
  await expect(toggle).toHaveCSS("height", "56px");
  await expect(panel).toHaveCount(0);
  await expect(dock.getByTestId("terminal-nav-toggle")).toHaveCount(0);
  await expect(box.getByTestId("terminal-nav-toggle")).toHaveCount(0);

  const readGeometry = () => mobileSmokePage.evaluate((id) => {
    const terminal = document.querySelector(`[data-testid="terminal-slot-${id}"]`);
    const composerDock = document.querySelector('[data-testid="mobile-chat-input"]');
    const chatBox = document.querySelector('[data-testid="chat-box"]');
    const keyToggle = document.querySelector('[data-testid="terminal-nav-toggle"]');
    const keyPanel = document.querySelector('[data-testid="terminal-nav-buttons"]');
    if (
      !(terminal instanceof HTMLElement)
      || !(composerDock instanceof HTMLElement)
      || !(chatBox instanceof HTMLElement)
      || !(keyToggle instanceof HTMLElement)
      || (keyPanel !== null && !(keyPanel instanceof HTMLElement))
    ) return null;

    const readRect = (element: HTMLElement) => {
      const rect = element.getBoundingClientRect();
      return {
        left: rect.left,
        top: rect.top,
        right: rect.right,
        bottom: rect.bottom,
        width: rect.width,
        height: rect.height,
      };
    };
    const terminalRect = readRect(terminal);
    const dockRect = readRect(composerDock);
    const chatRect = readRect(chatBox);
    const toggleRect = readRect(keyToggle);
    const panelRect = keyPanel ? readRect(keyPanel) : null;
    const viewport = window.visualViewport;
    const viewportLeft = viewport?.offsetLeft ?? 0;
    const viewportTop = viewport?.offsetTop ?? 0;
    const viewportRight = viewportLeft + (viewport?.width ?? window.innerWidth);
    const viewportBottom = viewportTop + (viewport?.height ?? window.innerHeight);
    const panelStyle = keyPanel ? getComputedStyle(keyPanel) : null;
    return {
      terminal: terminalRect,
      dock: dockRect,
      chat: chatRect,
      toggle: toggleRect,
      panel: panelRect,
      detached: !composerDock.contains(keyToggle)
        && !chatBox.contains(keyToggle)
        && (!keyPanel || (!composerDock.contains(keyPanel) && !chatBox.contains(keyPanel))),
      togglePosition: getComputedStyle(keyToggle).position,
      panelPosition: panelStyle?.position ?? null,
      panelAboveToggle: panelRect ? panelRect.bottom <= toggleRect.top + 1 : null,
      surfacesInside: [toggleRect, chatRect, ...(panelRect ? [panelRect] : [])].every((rect) =>
        rect.left >= viewportLeft - 1
        && rect.top >= viewportTop - 1
        && rect.right <= viewportRight + 1
        && rect.bottom <= viewportBottom + 1),
      panelOverflowY: panelStyle?.overflowY ?? null,
      panelScrollHeight: keyPanel?.scrollHeight ?? null,
      panelClientHeight: keyPanel?.clientHeight ?? null,
    };
  }, sessionId);
  const rectDelta = (
    actual: { left: number; top: number; right: number; bottom: number; width: number; height: number },
    expected: { left: number; top: number; right: number; bottom: number; width: number; height: number },
  ) => Math.max(
    Math.abs(actual.left - expected.left),
    Math.abs(actual.top - expected.top),
    Math.abs(actual.right - expected.right),
    Math.abs(actual.bottom - expected.bottom),
    Math.abs(actual.width - expected.width),
    Math.abs(actual.height - expected.height),
  );
  const expectStableRect = (
    actual: { left: number; top: number; right: number; bottom: number; width: number; height: number },
    expected: { left: number; top: number; right: number; bottom: number; width: number; height: number },
    label: string,
  ) => {
    const delta = rectDelta(actual, expected);
    expect(delta, label).toBeLessThanOrEqual(2);
  };

  const draft = "unsent navigation draft";
  await input.fill(draft);
  await mobileSmokePage.evaluate(() => {
    document.body.tabIndex = -1;
    document.body.focus();
  });
  await box.evaluate(async (el) => {
    await Promise.all(el.getAnimations().map((animation) => animation.finished));
  });
  const closedGeometry = await readGeometry();
  expect(closedGeometry, "closed floating keyboard geometry").not.toBeNull();
  expect(closedGeometry!.detached).toBe(true);
  expect(closedGeometry!.togglePosition).toBe("fixed");
  expect(closedGeometry!.surfacesInside).toBe(true);
  expect(closedGeometry!.toggle.width).toBeCloseTo(56, 0);
  expect(closedGeometry!.toggle.height).toBeCloseTo(56, 0);

  await toggle.tap();
  await expect(input).toHaveValue(draft);
  await expect(toggle).toHaveAttribute("data-open", "true");
  await expect(toggle).toHaveAttribute("aria-label", "Hide terminal keys");
  await expect(panel).toBeVisible();
  await expect(panel).toHaveCSS("position", "fixed");
  await expect(dock.getByTestId("terminal-nav-buttons")).toHaveCount(0);
  await expect(box.getByTestId("terminal-nav-buttons")).toHaveCount(0);
  await panel.evaluate(async (el) => {
    await Promise.all(el.getAnimations().map((animation) => animation.finished));
  });

  const expectOpenGeometry = async (label: string) => {
    await expect.poll(async () => {
      const geometry = await readGeometry();
      if (!geometry?.panel) return false;
      return geometry.detached
        && geometry.togglePosition === "fixed"
        && geometry.panelPosition === "fixed"
        && geometry.panelAboveToggle === true
        && geometry.surfacesInside;
    }, { message: `${label}: detached fixed panel and toggle must remain inside the viewport` }).toBe(true);
    const geometry = await readGeometry();
    expect(geometry?.panel, `${label}: open floating keyboard geometry`).not.toBeNull();
    return geometry!;
  };

  const initialPortrait = await expectOpenGeometry("initial portrait");
  expectStableRect(initialPortrait.dock, closedGeometry!.dock, "opening must not move or resize the composer dock");
  expectStableRect(initialPortrait.terminal, closedGeometry!.terminal, "opening must not change terminal geometry");

  const directKeys = [
    ["nav-esc", [0x1b]],
    ["nav-tab", [0x09]],
    ["nav-backspace", [0x7f]],
    ["nav-home", [0x1b, 0x5b, 0x48]],
    ["nav-up", [0x1b, 0x5b, 0x41]],
    ["nav-end", [0x1b, 0x5b, 0x46]],
    ["nav-pgup", [0x1b, 0x5b, 0x35, 0x7e]],
    ["nav-left", [0x1b, 0x5b, 0x44]],
    ["nav-down", [0x1b, 0x5b, 0x42]],
    ["nav-right", [0x1b, 0x5b, 0x43]],
    ["nav-pgdn", [0x1b, 0x5b, 0x36, 0x7e]],
    ["nav-enter", [0x0d]],
  ] as const;
  const expectedDirectBytes = directKeys.flatMap(([, data]) => [...data]);
  for (const [testId] of directKeys) await expect(mobileSmokePage.getByTestId(testId)).toBeVisible();
  await expect(mobileSmokePage.getByTestId("nav-ctrl")).toBeVisible();
  await expect(mobileSmokePage.getByTestId("nav-mouse")).toBeVisible();

  await resetTerminalInputCapture(mobileSmokePage);

  await mobileSmokePage.evaluate(() => {
    document.body.tabIndex = -1;
    document.body.focus();
  });
  expect(await mobileSmokePage.evaluate(() => document.activeElement === document.body)).toBe(true);
  await expect.poll(paneFocused).toBe(false);
  for (const [testId] of directKeys) {
    await mobileSmokePage.getByTestId(testId).tap();
    expect(await paneFocused(), `${testId} must not focus the hidden terminal textarea`).toBe(false);
  }
  await expect.poll(async () => {
    const capture = await readTerminalInputCapture(mobileSmokePage);
    return capture.batches.reduce((total, inputBatch) => total + inputBatch.data.length, 0);
  }).toBe(expectedDirectBytes.length);
  const directInputCapture = await readTerminalInputCapture(mobileSmokePage);
  expect(directInputCapture.droppedBatches).toBe(0);
  expect(directInputCapture.batches.every((inputBatch) => inputBatch.sessionId === sessionId)).toBe(true);
  expect(directInputCapture.batches.flatMap((inputBatch) => inputBatch.data)).toEqual(expectedDirectBytes);

  const mouse = mobileSmokePage.getByTestId("nav-mouse");
  await expect(mouse).toHaveAttribute("aria-pressed", "false");
  await mouse.tap();
  await expect(mouse).toHaveAttribute("aria-pressed", "true");
  expect(await paneFocused()).toBe(false);
  await mouse.tap();
  await expect(mouse).toHaveAttribute("aria-pressed", "false");
  expect(await paneFocused()).toBe(false);

  const ctrl = mobileSmokePage.getByTestId("nav-ctrl");
  await ctrl.tap();
  await expect(ctrl).toHaveAttribute("aria-pressed", "true");
  const openPanelBox = await panel.boundingBox();
  expect(openPanelBox).not.toBeNull();
  await mobileSmokePage.mouse.click(8, Math.max(8, openPanelBox!.y / 2));
  await expect.poll(paneFocused).toBe(true);
  await mobileSmokePage.keyboard.type("c");
  await expect.poll(async () => {
    const capture = await readTerminalInputCapture(mobileSmokePage);
    return capture.batches.reduce((total, inputBatch) => total + inputBatch.data.length, 0);
  }).toBe(expectedDirectBytes.length + 1);
  const ctrlInputCapture = await readTerminalInputCapture(mobileSmokePage);
  expect(ctrlInputCapture.droppedBatches).toBe(0);
  expect(ctrlInputCapture.batches.every((inputBatch) => inputBatch.sessionId === sessionId)).toBe(true);
  expect(ctrlInputCapture.batches.flatMap((inputBatch) => inputBatch.data).at(-1)).toBe(0x03);
  await expect(mobileSmokePage.getByTestId("nav-ctrl")).toHaveAttribute("aria-pressed", "false");

  const portrait = mobileSmokePage.viewportSize();
  expect(portrait).not.toBeNull();
  await mobileSmokePage.setViewportSize({ width: portrait!.height, height: portrait!.width });
  const landscape = await expectOpenGeometry("landscape");
  expect(landscape.panelOverflowY).toBe("auto");
  expect(landscape.panelScrollHeight!).toBeGreaterThan(landscape.panelClientHeight!);
  await panel.evaluate((el) => { el.scrollTop = el.scrollHeight; });
  await expect.poll(() => panel.evaluate((el) => el.scrollTop)).toBeGreaterThan(0);
  await mobileSmokePage.setViewportSize(portrait!);
  await expectOpenGeometry("restored portrait");
  await expect.poll(async () => {
    const restored = await readGeometry();
    if (!restored?.panel) return Number.POSITIVE_INFINITY;
    return Math.max(
      rectDelta(restored.dock, closedGeometry!.dock),
      rectDelta(restored.terminal, closedGeometry!.terminal),
      rectDelta(restored.toggle, initialPortrait.toggle),
      rectDelta(restored.panel, initialPortrait.panel!),
    );
  }, { message: "portrait terminal and detached floating keypad geometry must recover" })
    .toBeLessThanOrEqual(2);

  await input.tap();
  await expect(input).toBeFocused();
  await expect(panel).toBeVisible();
  await expect(toggle).toHaveAttribute("data-open", "true");
  await expect(input).toHaveValue(draft);
  expect(await paneFocused()).toBe(false);

  await mobileSmokePage.getByTestId("nav-ctrl").tap();
  await expect(mobileSmokePage.getByTestId("nav-ctrl")).toHaveAttribute("aria-pressed", "true");
  await toggle.tap();
  await expect(panel).toHaveCount(0);
  await expect(toggle).toHaveAttribute("data-open", "false");
  await expect(input).toHaveValue(draft);
  expect(await paneFocused()).toBe(false);
  await toggle.tap();
  await expect(panel).toBeVisible();
  await expect(toggle).toHaveAttribute("data-open", "true");
  await expect(mobileSmokePage.getByTestId("nav-ctrl")).toHaveAttribute("aria-pressed", "false");
  await expect(input).toHaveValue(draft);
  expect(await paneFocused()).toBe(false);
});

// A mic that cannot start used to leave the button red and data-state="listening"
// forever, with the reason rendered as an unreadable ~124x390 ribbon anchored to
// the 44px mic wrapper. Both halves are asserted here: the recording ENDS, and
// the caption spans the composer bar.
test("a mic that cannot start ends the recording and says why", async ({ mobileSmokePage, stack }) => {
  await stack.client.transcriptionSetConfig({ deepgramKey: "smoke-test-key", deepgramLanguage: "en" });
  await mobileSmokePage.addInitScript(() => {
    Object.defineProperty(navigator, "mediaDevices", {
      configurable: true,
      value: {
        getUserMedia: () => {
          const e = new Error("denied");
          e.name = "NotAllowedError";
          return Promise.reject(e);
        },
      },
    });
  });
  // Re-runs the module-scope transcription-config fetch with the key now set.
  await mobileSmokePage.reload({ waitUntil: "domcontentloaded" });
  await mobileSmokePage.waitForFunction(() => typeof (window as unknown as Window & { __smoke?: unknown }).__smoke === "object");
  const sessionId = (await spawnSmokeShell(mobileSmokePage, stack.workerFp)).session_id;
  await navigateToSmokeSession(mobileSmokePage, sessionId);
  const toggle = mobileSmokePage.getByTestId("terminal-nav-toggle");
  const panel = mobileSmokePage.getByTestId("terminal-nav-buttons");
  await expect(toggle).toHaveAttribute("data-open", "false");
  await toggle.tap();
  await expect(panel).toBeVisible();

  const voice = mobileSmokePage.getByTestId("mobile-voice-input");
  await expect(voice).toHaveAttribute("data-engine", "deepgram");
  const mic = mobileSmokePage.getByTestId("voice-mic");
  await mic.tap();
  await expect(panel).toBeVisible();
  await expect(toggle).toHaveAttribute("data-open", "true");

  await expect(voice).toHaveAttribute("data-state", "idle");
  await expect(mic).toHaveAttribute("data-recording", "false");
  const caption = mobileSmokePage.getByTestId("voice-caption");
  await expect(caption).toContainText("Mic blocked");
  const box = (await caption.boundingBox())!;
  expect(box.width).toBeGreaterThan(300);
  expect(box.x).toBeGreaterThanOrEqual(0);
});

// The reported bug that no error path covers: getUserMedia RESOLVES, the graph
// builds, and not one audio frame ever arrives (iOS suspends the AudioContext
// out from under the recording; AudioWorklet ships dead on some iPhone builds).
// That used to be a red mic forever with nothing recorded and nothing logged.
// The stub below is a complete Web Audio graph that never fires a callback, so
// BOTH the worklet and the ScriptProcessor repair path stay silent.
test("a mic that opens but delivers no audio ends the recording and says why", async ({ mobileSmokePage, stack }) => {
  await stack.client.transcriptionSetConfig({ deepgramKey: "smoke-test-key", deepgramLanguage: "en" });
  await mobileSmokePage.addInitScript(() => {
    class DeadPort { onmessage: unknown = null; close() {} }
    class DeadWorkletNode { port = new DeadPort(); connect() {} disconnect() {} }
    class DeadContext {
      state = "running";
      sampleRate = 48000;
      destination = {};
      audioWorklet = { addModule: () => Promise.resolve() };
      createMediaStreamSource() { return { connect() {}, disconnect() {} }; }
      createScriptProcessor() { return { onaudioprocess: null, connect() {}, disconnect() {} }; }
      resume() { return Promise.resolve(); }
      close() { return Promise.resolve(); }
    }
    const win = window as unknown as Record<string, unknown>;
    win.AudioContext = DeadContext;
    win.webkitAudioContext = DeadContext;
    win.AudioWorkletNode = DeadWorkletNode;
    Object.defineProperty(navigator, "mediaDevices", {
      configurable: true,
      value: { getUserMedia: () => Promise.resolve({ getTracks: () => [{ stop() {} }] }) },
    });
    // The hermetic stack has no internet, so the real Deepgram socket would
    // drop and claim the caption before the silence deadline. Only that ONE
    // host is faked (a permanently-open, send-swallowing socket); the SPA's own
    // Sync WS must keep using the real implementation.
    const RealWebSocket = window.WebSocket;
    const PatchedWebSocket = function (url: string, protocols?: string | string[]) {
      if (!String(url).includes("api.deepgram.com")) return new RealWebSocket(url, protocols);
      const sock = {
        url, readyState: 0,
        onopen: null as (() => void) | null, onmessage: null, onerror: null, onclose: null,
        send() {}, close() { sock.readyState = 3; },
      };
      setTimeout(() => { sock.readyState = 1; sock.onopen?.(); }, 10);
      return sock;
    } as unknown as typeof WebSocket;
    Object.assign(PatchedWebSocket, { CONNECTING: 0, OPEN: 1, CLOSING: 2, CLOSED: 3 });
    win.WebSocket = PatchedWebSocket;
  });
  await mobileSmokePage.reload({ waitUntil: "domcontentloaded" });
  await mobileSmokePage.waitForFunction(() => typeof (window as unknown as Window & { __smoke?: unknown }).__smoke === "object");
  const sessionId = (await spawnSmokeShell(mobileSmokePage, stack.workerFp)).session_id;
  await navigateToSmokeSession(mobileSmokePage, sessionId);

  const voice = mobileSmokePage.getByTestId("mobile-voice-input");
  await expect(voice).toHaveAttribute("data-engine", "deepgram");
  const mic = mobileSmokePage.getByTestId("voice-mic");
  await mic.tap();
  // The recording is genuinely live first — this is not a start failure.
  await expect(voice).toHaveAttribute("data-state", "listening");

  // Two silence windows (2.5s each): one rebuild attempt, then give up.
  await expect(voice).toHaveAttribute("data-state", "idle", { timeout: 20_000 });
  await expect(mic).toHaveAttribute("data-recording", "false");
  await expect(mobileSmokePage.getByTestId("voice-caption")).toContainText("sent no audio");
});

// Measured on the reporter's iPhone (iOS 18.7, installed PWA): the recording ran
// with ctx_state "suspended", frames 0, chunks 0, while the Deepgram socket was
// open — starting the mic switches the OS audio session and re-suspends a
// context created before the stream, and resume() does not bring it back. The
// stub reproduces exactly that: healthy until getUserMedia resolves, suspended
// afterwards, resume() a no-op. A pipeline that cannot render must fail the tap,
// not record silence.
test("an audio session suspended by the mic start fails the tap instead of recording silence", async ({ mobileSmokePage, stack }) => {
  await stack.client.transcriptionSetConfig({ deepgramKey: "smoke-test-key", deepgramLanguage: "en" });
  await mobileSmokePage.addInitScript(() => {
    const win = window as unknown as Record<string, unknown>;
    let live: { state: string } | null = null;
    class IosContext {
      state = "running";
      sampleRate = 48000;
      destination = {};
      audioWorklet = { addModule: () => Promise.resolve() };
      constructor() { live = this; }
      createMediaStreamSource() { return { connect() {}, disconnect() {} }; }
      createScriptProcessor() { return { onaudioprocess: null, connect() {}, disconnect() {} }; }
      resume() { return Promise.resolve(); } // iOS: outside a gesture this does nothing
      close() { return Promise.resolve(); }
    }
    class DeadPort { onmessage: unknown = null; close() {} }
    win.AudioContext = IosContext;
    win.webkitAudioContext = IosContext;
    win.AudioWorkletNode = class { port = new DeadPort(); connect() {} disconnect() {} };
    Object.defineProperty(navigator, "mediaDevices", {
      configurable: true,
      value: {
        getUserMedia: () => {
          if (live) live.state = "suspended"; // the OS audio-session switch
          return Promise.resolve({ getTracks: () => [{ stop() {} }] });
        },
      },
    });
  });
  await mobileSmokePage.reload({ waitUntil: "domcontentloaded" });
  await mobileSmokePage.waitForFunction(() => typeof (window as unknown as Window & { __smoke?: unknown }).__smoke === "object");
  const sessionId = (await spawnSmokeShell(mobileSmokePage, stack.workerFp)).session_id;
  await navigateToSmokeSession(mobileSmokePage, sessionId);

  const voice = mobileSmokePage.getByTestId("mobile-voice-input");
  await expect(voice).toHaveAttribute("data-engine", "deepgram");
  const mic = mobileSmokePage.getByTestId("voice-mic");
  await mic.tap();

  await expect(voice).toHaveAttribute("data-state", "idle");
  await expect(mic).toHaveAttribute("data-recording", "false");
  // Names the real cause, and fails immediately instead of after two silence
  // windows — which is how you tell this apart from generic dead capture.
  await expect(mobileSmokePage.getByTestId("voice-caption")).toContainText("audio session stayed suspended");
});

// iOS can leave the activation-primer resume pending/rejected while
// getUserMedia switches the OS audio session. That first resume is not the
// liveness verdict: once the stream exists, the wired graph gets one
// authoritative resume and must send real PCM before Deepgram can transcribe.
test("mobile mic retries AudioContext resume after the input session opens", async ({ mobileSmokePage, stack }) => {
  await stack.client.transcriptionSetConfig({ deepgramKey: "smoke-test-key", deepgramLanguage: "en" });
  await mobileSmokePage.addInitScript(() => {
    let inputSessionOpen = false;
    let liveContext: { state: string } | null = null;
    let liveNode: { port: { onmessage: ((e: { data: Float32Array }) => void) | null } } | null = null;

    class RetryContext {
      state = "suspended";
      sampleRate = 48000;
      destination = {};
      audioWorklet = { addModule: () => Promise.resolve() };
      resumeCalls = 0;
      constructor() { liveContext = this; }
      createMediaStreamSource() { return { connect() {}, disconnect() {} }; }
      createScriptProcessor() { return { onaudioprocess: null, connect() {}, disconnect() {} }; }
      resume() {
        this.resumeCalls++;
        if (this.resumeCalls === 1) {
          return Promise.reject(new Error("the audio session did not respond in time — tap the mic again"));
        }
        if (!inputSessionOpen) return Promise.reject(new Error("the input session is not open"));
        this.state = "running";
        return Promise.resolve();
      }
      close() {
        this.state = "closed";
        return Promise.resolve();
      }
    }
    class LiveWorkletNode {
      port = { onmessage: null as ((e: { data: Float32Array }) => void) | null, close() {} };
      constructor() { liveNode = this; }
      connect() {}
      disconnect() {}
    }

    const win = window as unknown as Record<string, unknown>;
    win.AudioContext = RetryContext;
    win.webkitAudioContext = RetryContext;
    win.AudioWorkletNode = LiveWorkletNode;
    const liveFrame = new Float32Array(2048).fill(0.25);
    setInterval(() => {
      if (inputSessionOpen && liveContext?.state === "running") {
        liveNode?.port.onmessage?.({ data: liveFrame });
      }
    }, 20);
    Object.defineProperty(navigator, "mediaDevices", {
      configurable: true,
      value: {
        getUserMedia: () => {
          inputSessionOpen = true;
          return Promise.resolve({ getTracks: () => [{ stop() {} }] });
        },
      },
    });

    const RealWebSocket = window.WebSocket;
    const PatchedWebSocket = function (url: string, protocols?: string | string[]) {
      if (!String(url).includes("api.deepgram.com")) return new RealWebSocket(url, protocols);
      let sentPcm = false;
      let announcedPcm = false;
      const sock = {
        url, readyState: 0,
        onopen: null as (() => void) | null,
        onmessage: null as ((e: { data: string }) => void) | null,
        onerror: null, onclose: null,
        send(payload: unknown) {
          if (payload instanceof ArrayBuffer) {
            sentPcm ||= payload.byteLength > 0;
            if (sentPcm && !announcedPcm) {
              announcedPcm = true;
              setTimeout(() => {
                sock.onmessage?.({
                  data: JSON.stringify({
                    type: "Results", is_final: false,
                    channel: { alternatives: [{ transcript: "PCM ready" }] },
                  }),
                });
              }, 0);
            }
            return;
          }
          if (payload !== '{"type":"Finalize"}') return;
          setTimeout(() => {
            sock.onmessage?.({
              data: JSON.stringify({
                type: "Results", is_final: true, from_finalize: true,
                channel: {
                  alternatives: [{
                    transcript: sentPcm ? "post-stream resume delivered audio" : "",
                  }],
                },
              }),
            });
          }, 0);
        },
        close() { sock.readyState = 3; },
      };
      setTimeout(() => { sock.readyState = 1; sock.onopen?.(); }, 10);
      return sock;
    } as unknown as typeof WebSocket;
    Object.assign(PatchedWebSocket, { CONNECTING: 0, OPEN: 1, CLOSING: 2, CLOSED: 3 });
    win.WebSocket = PatchedWebSocket;
  });

  await mobileSmokePage.reload({ waitUntil: "domcontentloaded" });
  await mobileSmokePage.waitForFunction(() => typeof (window as unknown as Window & { __smoke?: unknown }).__smoke === "object");
  const sessionId = (await spawnSmokeShell(mobileSmokePage, stack.workerFp)).session_id;
  await navigateToSmokeSession(mobileSmokePage, sessionId);

  const voice = mobileSmokePage.getByTestId("mobile-voice-input");
  await expect(voice).toHaveAttribute("data-engine", "deepgram");
  const mic = mobileSmokePage.getByTestId("voice-mic");
  const input = mobileSmokePage.getByTestId("chat-input");
  await mic.tap();
  await expect(voice).toHaveAttribute("data-state", "listening");
  await expect(input).toHaveValue("PCM ready");

  await mic.tap();
  await expect(voice).toHaveAttribute("data-state", "idle", { timeout: 15_000 });
  await expect(input).toHaveValue("post-stream resume delivered audio");
  await expect(mobileSmokePage.getByTestId("voice-caption")).toHaveCount(0);
});

// The report this whole class comes from: on a phone the FIRST recording works
// and every one after it is dead — the mic never leaves `listening`, or the stop
// wedges in `finalizing`, or the caption reads "Deepgram connection dropped".
// The graph here is genuinely LIVE (frames really flow) and the faked Deepgram
// socket really answers a Finalize, so nothing but the engine's own state can
// end a recording. The 4.5 s gap is longer than the mobile idle release
// (micIdle.releaseMs = 4 s on touch), so tap #2 pays a COLD device open exactly
// like the phone does instead of reusing a warm pipeline.
test("a second recording works exactly like the first", async ({ mobileSmokePage, stack }) => {
  await stack.client.transcriptionSetConfig({ deepgramKey: "smoke-test-key", deepgramLanguage: "en" });
  await mobileSmokePage.addInitScript(() => {
    let liveNode: { port: { onmessage: ((e: { data: Float32Array }) => void) | null } } | null = null;
    class LiveWorkletNode {
      port = { onmessage: null as ((e: { data: Float32Array }) => void) | null, close() {} };
      constructor() { liveNode = this; }
      connect() {}
      disconnect() {}
    }
    class LiveContext {
      state = "running";
      sampleRate = 48000;
      destination = {};
      audioWorklet = { addModule: () => Promise.resolve() };
      createMediaStreamSource() { return { connect() {}, disconnect() {} }; }
      createScriptProcessor() { return { onaudioprocess: null, connect() {}, disconnect() {} }; }
      resume() { return Promise.resolve(); }
      close() { return Promise.resolve(); }
    }
    const win = window as unknown as Record<string, unknown>;
    win.AudioContext = LiveContext;
    win.webkitAudioContext = LiveContext;
    win.AudioWorkletNode = LiveWorkletNode;
    let emitInterim = false;
    win.__emitVoiceInterim = () => { emitInterim = true; };
    // 48 kHz × 40 ms = 1920 samples; 2048 clears the resampler's emit threshold,
    // so every tick delivers a real PCM chunk to the attached sink.
    setInterval(() => liveNode?.port.onmessage?.({ data: new Float32Array(2048).fill(0.3) }), 40);
    Object.defineProperty(navigator, "mediaDevices", {
      configurable: true,
      value: { getUserMedia: () => Promise.resolve({ getTracks: () => [{ stop() {} }] }) },
    });
    // The hermetic stack has no internet, so the real Deepgram socket would drop
    // and claim the caption. Only that ONE host is faked; the SPA's own Sync WS
    // must keep using the real implementation. This one ANSWERS a Finalize, so a
    // stop delivers a transcript instead of timing out into an empty send.
    const RealWebSocket = window.WebSocket;
    const PatchedWebSocket = function (url: string, protocols?: string | string[]) {
      if (!String(url).includes("api.deepgram.com")) return new RealWebSocket(url, protocols);
      const sock = {
        url, readyState: 0,
        onopen: null as (() => void) | null,
        onmessage: null as ((e: { data: string }) => void) | null,
        onerror: null, onclose: null,
        send(payload: unknown) {
          if (payload !== '{"type":"Finalize"}') return;
          setTimeout(() => {
            sock.onmessage?.({
              data: JSON.stringify({
                type: "Results", is_final: true, from_finalize: true,
                channel: { alternatives: [{ transcript: "hello from the mic" }] },
              }),
            });
          }, 0);
        },
        close() { sock.readyState = 3; },
      };
      setTimeout(() => {
        sock.readyState = 1;
        sock.onopen?.();
        if (emitInterim) {
          sock.onmessage?.({
            data: JSON.stringify({
              type: "Results", is_final: false,
              channel: { alternatives: [{ transcript: "still recording" }] },
            }),
          });
        }
      }, 10);
      return sock;
    } as unknown as typeof WebSocket;
    Object.assign(PatchedWebSocket, { CONNECTING: 0, OPEN: 1, CLOSING: 2, CLOSED: 3 });
    win.WebSocket = PatchedWebSocket;
  });
  await mobileSmokePage.reload({ waitUntil: "domcontentloaded" });
  await mobileSmokePage.waitForFunction(() => typeof (window as unknown as Window & { __smoke?: unknown }).__smoke === "object");
  const sessionId = (await spawnSmokeShell(mobileSmokePage, stack.workerFp)).session_id;
  await navigateToSmokeSession(mobileSmokePage, sessionId);

  const voice = mobileSmokePage.getByTestId("mobile-voice-input");
  await expect(voice).toHaveAttribute("data-engine", "deepgram");
  const mic = mobileSmokePage.getByTestId("voice-mic");
  const input = mobileSmokePage.getByTestId("chat-input");

  const send = mobileSmokePage.getByTestId("chat-send");
  await expect(input).not.toBeFocused();
  await mic.tap();
  await expect(voice).toHaveAttribute("data-state", "listening");
  await mic.tap();
  await expect(voice).toHaveAttribute("data-state", "idle", { timeout: 15_000 });
  await expect(input).toHaveValue("hello from the mic");
  await expect(input).not.toBeFocused();

  await send.click();
  await expect(input).toHaveValue("");
  await expect(input).not.toBeFocused();
  await expect.poll(() => readStoredComposerDraft(mobileSmokePage, sessionId)).toEqual({
    present: false,
    value: null,
  });

  // Longer than the 4 s mobile idle release: recording two opens the device
  // cold, exactly like the reporter's second attempt.
  await mobileSmokePage.waitForTimeout(4_500);
  await mic.tap();
  await expect(voice).toHaveAttribute("data-state", "listening");
  await expect(input).toHaveValue("");
  expect(await readStoredComposerDraft(mobileSmokePage, sessionId)).toEqual({
    present: false,
    value: null,
  });
  await mic.tap();
  await expect(voice).toHaveAttribute("data-state", "idle", { timeout: 15_000 });
  await expect(input).toHaveValue("hello from the mic");
  await expect(input).not.toBeFocused();

  // Preserve the unsent-recordings contract: another finalized recording
  // appends instead of replacing the still-unsent second transcript.
  await mobileSmokePage.waitForTimeout(4_500);
  await mic.tap();
  await expect(voice).toHaveAttribute("data-state", "listening");
  await mic.tap();
  await expect(voice).toHaveAttribute("data-state", "idle", { timeout: 15_000 });
  await expect(input).toHaveValue("hello from the mic hello from the mic");
  await expect(input).not.toBeFocused();
  await expect(mobileSmokePage.getByTestId("voice-caption")).toHaveCount(0);

  // Hiding the only reachable composer must cancel the recording and restore
  // the pre-dictation draft, not retain an interim hypothesis as ordinary text.
  const settledDraft = "hello from the mic hello from the mic";
  await mobileSmokePage.evaluate(() => {
    const emitInterim = (window as unknown as { __emitVoiceInterim: () => void }).__emitVoiceInterim;
    emitInterim();
  });
  await mic.tap();
  await expect(voice).toHaveAttribute("data-state", "listening");
  await expect(input).toHaveValue(`${settledDraft} still recording`);
  const drawer = mobileSmokePage.getByTestId("sidebar-drawer");
  await mobileSmokePage.getByTestId("mobile-deck-bar-menu").tap();
  await expect(drawer).toHaveAttribute("data-open", "true");
  await expect(mobileSmokePage.getByTestId("mobile-chat-input")).toHaveCount(0);
  await mobileSmokePage.getByTestId("brand-row-collapse").tap();
  await expect(drawer).toHaveAttribute("data-open", "false");
  await expect(mobileSmokePage.getByTestId("chat-input")).toHaveValue(settledDraft);
  await expect(mobileSmokePage.getByTestId("mobile-voice-input")).toHaveAttribute("data-state", "idle");
});

test("Web Speech second recording starts from an empty sent draft", async ({ mobileSmokePage, stack }) => {
  await stack.client.transcriptionSetConfig({ deepgramKey: "", deepgramLanguage: "en" });
  await mobileSmokePage.addInitScript(() => {
    interface FakeResult extends Array<{ transcript: string }> {
      isFinal: boolean;
    }
    interface FakeResultEvent {
      resultIndex: number;
      results: FakeResult[];
    }
    class FakeSpeechRecognition {
      continuous = false;
      interimResults = false;
      lang = "";
      onresult: ((event: FakeResultEvent) => void) | null = null;
      onend: (() => void) | null = null;
      onerror: ((event: { error: string }) => void) | null = null;
      start() {}
      stop() {
        const result = [{ transcript: "hello from web speech" }] as FakeResult;
        result.isFinal = true;
        this.onresult?.({ resultIndex: 0, results: [result] });
        queueMicrotask(() => this.onend?.());
      }
      abort() {
        queueMicrotask(() => this.onend?.());
      }
    }
    const speechWindow = window as unknown as Window & {
      SpeechRecognition: typeof FakeSpeechRecognition;
      webkitSpeechRecognition: typeof FakeSpeechRecognition;
    };
    speechWindow.SpeechRecognition = FakeSpeechRecognition;
    speechWindow.webkitSpeechRecognition = FakeSpeechRecognition;
  });
  await mobileSmokePage.reload({ waitUntil: "domcontentloaded" });
  await mobileSmokePage.waitForFunction(() => typeof (window as unknown as Window & { __smoke?: unknown }).__smoke === "object");
  const sessionId = (await spawnSmokeShell(mobileSmokePage, stack.workerFp)).session_id;
  await navigateToSmokeSession(mobileSmokePage, sessionId);

  const voice = mobileSmokePage.getByTestId("mobile-voice-input");
  const mic = mobileSmokePage.getByTestId("voice-mic");
  const input = mobileSmokePage.getByTestId("chat-input");
  await expect(voice).toHaveAttribute("data-engine", "web-speech");

  await mic.tap();
  await expect(voice).toHaveAttribute("data-state", "listening");
  await mic.tap();
  await expect(voice).toHaveAttribute("data-state", "idle");
  await expect(input).toHaveValue("hello from web speech");

  await mobileSmokePage.getByTestId("chat-send").click();
  await expect(input).toHaveValue("");
  await expect.poll(() => readStoredComposerDraft(mobileSmokePage, sessionId)).toEqual({
    present: false,
    value: null,
  });

  await mic.tap();
  await expect(voice).toHaveAttribute("data-state", "listening");
  await expect(input).toHaveValue("");
  expect(await readStoredComposerDraft(mobileSmokePage, sessionId)).toEqual({
    present: false,
    value: null,
  });
  await mic.tap();
  await expect(voice).toHaveAttribute("data-state", "idle");
  await expect(input).toHaveValue("hello from web speech");
});

test("desktop dictation stops when its pane is parked and Enter cannot submit it", async ({ smokePage, stack }) => {
  await stack.client.transcriptionSetConfig({ deepgramKey: "", deepgramLanguage: "en" });
  await smokePage.addInitScript(() => {
    interface FakeResult extends Array<{ transcript: string }> {
      isFinal: boolean;
    }
    interface FakeResultEvent {
      resultIndex: number;
      results: FakeResult[];
    }
    let speechStarts = 0;
    class FakeSpeechRecognition {
      continuous = false;
      interimResults = false;
      lang = "";
      onresult: ((event: FakeResultEvent) => void) | null = null;
      onend: (() => void) | null = null;
      onerror: ((event: { error: string }) => void) | null = null;
      start() {
        speechStarts += 1;
        const result = [{ transcript: "still speaking" }] as FakeResult;
        result.isFinal = false;
        queueMicrotask(() => this.onresult?.({ resultIndex: 0, results: [result] }));
      }
      stop() {
        const result = [{ transcript: "finished speech" }] as FakeResult;
        result.isFinal = true;
        this.onresult?.({ resultIndex: 0, results: [result] });
        queueMicrotask(() => this.onend?.());
      }
      abort() {
        queueMicrotask(() => this.onend?.());
      }
    }
    const speechWindow = window as unknown as Window & {
      SpeechRecognition: typeof FakeSpeechRecognition;
      webkitSpeechRecognition: typeof FakeSpeechRecognition;
      __speechStarts: () => number;
    };
    speechWindow.SpeechRecognition = FakeSpeechRecognition;
    speechWindow.webkitSpeechRecognition = FakeSpeechRecognition;
    speechWindow.__speechStarts = () => speechStarts;
  });
  await smokePage.reload({ waitUntil: "domcontentloaded" });
  await smokePage.waitForFunction(
    () => typeof (window as unknown as Window & { __smoke?: unknown }).__smoke === "object",
  );

  const firstId = (await spawnSmokeShell(smokePage, stack.workerFp)).session_id;
  await navigateToSmokeSession(smokePage, firstId);
  const firstSlot = smokePage.getByTestId(`terminal-slot-${firstId}`);
  const firstDock = firstSlot.getByTestId("mobile-chat-input");
  const firstVoice = firstDock.getByTestId("mobile-voice-input");
  const firstInput = firstDock.getByTestId("chat-input");
  await expect(firstVoice).toHaveAttribute("data-engine", "web-speech");
  await firstInput.fill("typed base");
  await firstDock.getByTestId("voice-mic").click();
  await expect(firstVoice).toHaveAttribute("data-state", "listening");
  await expect(firstInput).toHaveValue("typed base still speaking");

  await firstInput.press("Enter");
  await expect(firstVoice).toHaveAttribute("data-state", "listening");
  await expect(firstInput).toHaveValue("typed base still speaking");

  const secondId = (await spawnSmokeShell(smokePage, stack.workerFp)).session_id;
  await switchToSmokeSession(smokePage, secondId);
  const secondDock = smokePage
    .getByTestId(`terminal-slot-${secondId}`)
    .getByTestId("mobile-chat-input");
  await expect(secondDock).toBeVisible();
  await expect(secondDock.getByTestId("voice-mic")).toBeVisible();
  await expect(firstVoice).toHaveAttribute("data-state", "idle");

  await switchToSmokeSession(smokePage, firstId);
  await expect(firstDock).toBeVisible();
  await expect(firstInput).toHaveValue("typed base");
  await expect(firstVoice).toHaveAttribute("data-state", "idle");

  // Spotlight keeps covered pane DOM mounted, so it must explicitly deactivate
  // that pane's recorder and expose the spotlit pane's mic.
  await pressPlatformShortcut(smokePage, "splitRight", "D");
  await expect.poll(() => smokePage.locator("[data-pane-slot]").evaluateAll((slots) =>
    slots.filter((slot) => {
      const rect = slot.getBoundingClientRect();
      return rect.right > 0 && rect.left < innerWidth && rect.bottom > 0 && rect.top < innerHeight;
    }).length)).toBe(2);
  const visibleIds = await smokePage.locator("[data-pane-slot]").evaluateAll((slots) =>
    slots.flatMap((slot) => {
      const rect = slot.getBoundingClientRect();
      const testId = slot.getAttribute("data-testid") ?? "";
      return rect.right > 0 && rect.left < innerWidth && testId.startsWith("terminal-slot-")
        ? [testId.slice("terminal-slot-".length)]
        : [];
    }));
  const spotlightId = visibleIds.find((id) => id !== firstId);
  expect(spotlightId).toBeTruthy();
  const spotlightSlot = smokePage.getByTestId(`terminal-slot-${spotlightId!}`);
  const spotlightDock = spotlightSlot.getByTestId("mobile-chat-input");

  // Both controls can already be activation targets when two clicks arrive in
  // one task. The first claim must atomically exclude the retained second target.
  const startsBeforeRace = await smokePage.evaluate(() =>
    (window as unknown as Window & { __speechStarts: () => number }).__speechStarts());
  await smokePage.evaluate(([firstSessionId, secondSessionId]) => {
    const firstMic = document.querySelector(
      `[data-testid="terminal-slot-${firstSessionId}"] [data-testid="voice-mic"]`,
    );
    const secondMic = document.querySelector(
      `[data-testid="terminal-slot-${secondSessionId}"] [data-testid="voice-mic"]`,
    );
    if (!(firstMic instanceof HTMLElement) || !(secondMic instanceof HTMLElement)) {
      throw new Error("both split-pane microphones must exist before the activation race");
    }
    firstMic.click();
    secondMic.click();
  }, [firstId, spotlightId!] as const);
  await expect.poll(() => smokePage.evaluate(() =>
    (window as unknown as Window & { __speechStarts: () => number }).__speechStarts()))
    .toBe(startsBeforeRace + 1);
  await expect(smokePage.locator(
    '[data-testid="mobile-voice-input"][data-state="listening"]',
  )).toHaveCount(1);
  await expect(smokePage.getByTestId("voice-mic")).toHaveCount(1);
  await smokePage.getByTestId("voice-discard").click();
  await expect(firstDock.getByTestId("voice-mic")).toBeVisible();
  await expect(spotlightDock.getByTestId("voice-mic")).toBeVisible();

  await firstInput.click();
  await firstDock.getByTestId("voice-mic").click();
  await expect(firstVoice).toHaveAttribute("data-state", "listening");
  await expect(firstInput).toHaveValue("typed base still speaking");
  await expect(spotlightDock.getByTestId("voice-mic")).toHaveCount(0);
  await spotlightSlot.getByTestId("terminal-display").click();
  await pressPlatformShortcut(smokePage, "spotlight", "Enter");
  await expect(spotlightSlot).toHaveAttribute("data-spotlit", "true");
  await expect(firstDock).toHaveAttribute("data-active", "false");
  await expect(firstDock).toHaveAttribute("aria-hidden", "true");
  await expect(firstDock).toHaveAttribute("inert", "");
  await expect(spotlightDock).toHaveAttribute("data-active", "true");
  await expect(spotlightDock).not.toHaveAttribute("aria-hidden", "true");
  await expect(firstVoice).toHaveAttribute("data-state", "idle");
  await expect(firstInput).toHaveValue("typed base");
  await expect(spotlightDock.getByTestId("voice-mic")).toBeVisible();
  // Compact suppresses the spotlight surface but retains its stored ID. The
  // visible viewport composer must become active rather than a silent mic shell.
  await smokePage.setViewportSize({ width: 390, height: 844 });
  const compactDock = smokePage.getByTestId("mobile-chat-input");
  await expect(compactDock).toHaveCount(1);
  await expect(compactDock).toHaveAttribute("data-placement", "viewport");
  await compactDock.getByTestId("voice-mic").click();
  await expect(compactDock.getByTestId("mobile-voice-input")).toHaveAttribute(
    "data-state",
    "listening",
  );
  await compactDock.getByTestId("voice-discard").click();
  await smokePage.keyboard.press("Escape");
});

// Chromium's ordinary media-device behavior cannot reproduce WebKit's
// never-settling promise (and passes on the unfixed code), so
// the stall is injected: getUserMedia parks forever on the FIRST call only.
// Unfixed, that tap never resolves — `warming` keeps the dead promise for the
// page's lifetime, so the mic breathes in `listening` and EVERY later tap awaits
// the same corpse. Fixed, the tap ends with a reason and the next one opens a
// fresh device.
test("a stalled mic open ends the recording and the next tap still works", async ({ mobileSmokePage, stack }) => {
  await stack.client.transcriptionSetConfig({ deepgramKey: "smoke-test-key", deepgramLanguage: "en" });
  await mobileSmokePage.addInitScript(() => {
    let liveNode: { port: { onmessage: ((e: { data: Float32Array }) => void) | null } } | null = null;
    class LiveWorkletNode {
      port = { onmessage: null as ((e: { data: Float32Array }) => void) | null, close() {} };
      constructor() { liveNode = this; }
      connect() {}
      disconnect() {}
    }
    class LiveContext {
      state = "running";
      sampleRate = 48000;
      destination = {};
      audioWorklet = { addModule: () => Promise.resolve() };
      createMediaStreamSource() { return { connect() {}, disconnect() {} }; }
      createScriptProcessor() { return { onaudioprocess: null, connect() {}, disconnect() {} }; }
      resume() { return Promise.resolve(); }
      close() { return Promise.resolve(); }
    }
    const win = window as unknown as Record<string, unknown>;
    win.AudioContext = LiveContext;
    win.webkitAudioContext = LiveContext;
    win.AudioWorkletNode = LiveWorkletNode;
    setInterval(() => liveNode?.port.onmessage?.({ data: new Float32Array(2048).fill(0.3) }), 40);
    let opens = 0;
    Object.defineProperty(navigator, "mediaDevices", {
      configurable: true,
      value: {
        getUserMedia: () => {
          opens++;
          // iOS 18.7 as an installed PWA: the OS audio session is mid-transition
          // and the promise simply never settles. Nothing cancels it.
          if (opens === 1) return Promise.withResolvers<MediaStream>().promise;
          return Promise.resolve({ getTracks: () => [{ stop() {} }] });
        },
      },
    });
    const RealWebSocket = window.WebSocket;
    const PatchedWebSocket = function (url: string, protocols?: string | string[]) {
      if (!String(url).includes("api.deepgram.com")) return new RealWebSocket(url, protocols);
      const sock = {
        url, readyState: 0,
        onopen: null as (() => void) | null,
        onmessage: null as ((e: { data: string }) => void) | null,
        onerror: null, onclose: null,
        send(payload: unknown) {
          if (payload !== '{"type":"Finalize"}') return;
          setTimeout(() => {
            sock.onmessage?.({
              data: JSON.stringify({
                type: "Results", is_final: true, from_finalize: true,
                channel: { alternatives: [{ transcript: "hello from the mic" }] },
              }),
            });
          }, 0);
        },
        close() { sock.readyState = 3; },
      };
      setTimeout(() => { sock.readyState = 1; sock.onopen?.(); }, 10);
      return sock;
    } as unknown as typeof WebSocket;
    Object.assign(PatchedWebSocket, { CONNECTING: 0, OPEN: 1, CLOSING: 2, CLOSED: 3 });
    win.WebSocket = PatchedWebSocket;
  });
  await mobileSmokePage.reload({ waitUntil: "domcontentloaded" });
  await mobileSmokePage.waitForFunction(() => typeof (window as unknown as Window & { __smoke?: unknown }).__smoke === "object");
  const sessionId = (await spawnSmokeShell(mobileSmokePage, stack.workerFp)).session_id;
  await navigateToSmokeSession(mobileSmokePage, sessionId);

  const voice = mobileSmokePage.getByTestId("mobile-voice-input");
  await expect(voice).toHaveAttribute("data-engine", "deepgram");
  const mic = mobileSmokePage.getByTestId("voice-mic");

  // The stalled tap must END, with the deadline's own caption.
  await mic.tap();
  await expect(voice).toHaveAttribute("data-state", "idle", { timeout: 20_000 });
  await expect(mobileSmokePage.getByTestId("voice-caption")).toContainText("did not respond in time");

  // …and the singleton must be usable again on the very next tap.
  await mic.tap();
  await expect(voice).toHaveAttribute("data-state", "listening");
  await mic.tap();
  await expect(voice).toHaveAttribute("data-state", "idle", { timeout: 15_000 });
  await expect(mobileSmokePage.getByTestId("chat-input")).toHaveValue(/hello from the mic/);
});

test("mobile composer keeps an unsent draft per session, across panes and reloads", async ({ mobileSmokePage, stack }) => {
  const firstId = (await spawnSmokeShell(mobileSmokePage, stack.workerFp)).session_id;
  await navigateToSmokeSession(mobileSmokePage, firstId);
  await expectSmokeComposer(mobileSmokePage);

  const box = mobileSmokePage.getByTestId("chat-box");
  const input = mobileSmokePage.getByTestId("chat-input");
  await input.fill("half typed message");

  const drawer = mobileSmokePage.getByTestId("sidebar-drawer");
  await mobileSmokePage.getByTestId("mobile-deck-bar-menu").tap();
  await expect(drawer).toHaveAttribute("data-open", "true");
  await expect(mobileSmokePage.getByTestId("mobile-chat-input")).toHaveCount(0);
  await expect(mobileSmokePage.getByTestId("terminal-nav-toggle")).toHaveCount(0);
  await expect(mobileSmokePage.getByTestId("terminal-nav-buttons")).toHaveCount(0);

  await mobileSmokePage.getByTestId("brand-row-collapse").tap();
  await expect(drawer).toHaveAttribute("data-open", "false");
  await expect(mobileSmokePage.getByTestId("mobile-chat-input")).toBeVisible();
  await expect(mobileSmokePage.getByTestId("chat-input")).toHaveValue("half typed message");

  // Body-portaled controls must leave with the terminal surface. The persistent
  // deck stays mounted behind /file, so hiding only its host is insufficient.
  await mobileSmokePage.evaluate((workerFp) => {
    const smoke = (window as unknown as Window & {
      __smoke: { navigate(href: string): void };
    }).__smoke;
    smoke.navigate(`/file/${workerFp}/tmp/roost-composer-missing.txt`);
  }, stack.workerFp);
  await expect(mobileSmokePage.getByTestId("file-viewer-sheet")).toBeVisible();
  await expect(mobileSmokePage.getByTestId("mobile-chat-input")).toHaveCount(0);
  await expect(mobileSmokePage.getByTestId("terminal-nav-toggle")).toHaveCount(0);
  await mobileSmokePage.evaluate((id) => {
    const smoke = (window as unknown as Window & {
      __smoke: { navigate(href: string): void };
    }).__smoke;
    smoke.navigate(`/s/${id}`);
  }, firstId);
  await expectSmokeComposer(mobileSmokePage);
  await expect(mobileSmokePage.getByTestId("chat-input")).toHaveValue("half typed message");

  // Tapping the terminal blurs the field but keeps both bar and draft.
  await mobileSmokePage.getByTestId(`terminal-slot-${firstId}`).click({ position: { x: 8, y: 8 } });
  await expect(box).toBeVisible();
  await expect(input).not.toBeFocused();
  await expect(input).toHaveValue("half typed message");

  // Escape has the same single-layer behavior: dismiss focus, retain content.
  await input.tap();
  await expect(input).toBeFocused();
  await input.press("Escape");
  await expect(box).toBeVisible();
  await expect(input).not.toBeFocused();
  await expect(input).toHaveValue("half typed message");

  // A different session gets its own empty draft; coming back restores this one.
  const secondId = (await spawnSmokeShell(mobileSmokePage, stack.workerFp)).session_id;
  await switchToSmokeSession(mobileSmokePage, secondId);
  await expectSmokeComposer(mobileSmokePage);
  await expect(mobileSmokePage.getByTestId("chat-input")).toHaveValue("");
  await switchToSmokeSession(mobileSmokePage, firstId);
  await expectSmokeComposer(mobileSmokePage);
  await expect(mobileSmokePage.getByTestId("chat-input")).toHaveValue("half typed message");

  // A reload restores it (localStorage, local device only).
  await mobileSmokePage.reload({ waitUntil: "domcontentloaded" });
  await expect(mobileSmokePage.getByTestId(`terminal-slot-${firstId}`)).toBeVisible();
  await expectSmokeComposer(mobileSmokePage);
  await expect(mobileSmokePage.getByTestId("chat-input")).toHaveValue("half typed message");

  // Send is the only thing that consumes it, and does not dismiss the dock.
  await mobileSmokePage.getByTestId("chat-send").click();
  await expect(mobileSmokePage.getByTestId("chat-box")).toBeVisible();
  await expect(mobileSmokePage.getByTestId("chat-input")).toHaveValue("");
});

test("mobile composer preserves and submits terminal input", async ({ mobileSmokePage, stack }, testInfo) => {
  test.skip(testInfo.project.name !== "webkit-iphone", "iPhone terminal input contract");
  const sessionId = await mobileSmokePage.evaluate(async (workerFp) => {
    const smoke = (window as unknown as Window & { __smoke: { spawnShell(worker: string, folder: string): Promise<{ session_id: string }> } }).__smoke;
    return (await smoke.spawnShell(workerFp, "/tmp")).session_id;
  }, stack.workerFp);
  await mobileSmokePage.goto(`${stack.baseUrl}/s/${sessionId}`);
  const slot = mobileSmokePage.getByTestId(`terminal-slot-${sessionId}`);
  await expect(slot).toBeVisible();
  await expectSmokeComposer(mobileSmokePage);

  const box = mobileSmokePage.getByTestId("chat-box");
  const initialInput = mobileSmokePage.getByTestId("chat-input");
  await expect(initialInput).not.toBeFocused();
  await initialInput.tap();
  await expect(initialInput).toBeFocused();

  await inputSmokeTerminal(
    mobileSmokePage,
    sessionId,
    `IFS= read -r line; printf '<%s>\\n' "$line"\r`,
  );
  await initialInput.fill("  x  ");
  await mobileSmokePage.getByTestId("chat-send").click();
  await expect.poll(() => slot.textContent()).toContain("<  x  >");
  await expect(box).toBeVisible();
  await expect(initialInput).toHaveValue("");

  await slot.click({ position: { x: 8, y: 8 } });
  await expect(box).toBeVisible();
  await expect(initialInput).not.toBeFocused();
  await expect.poll(() => mobileSmokePage.evaluate((id) => {
    const smoke = (window as unknown as Window & {
      __smoke: { paneFocused(sessionId: string): { focused: boolean } };
    }).__smoke;
    return smoke.paneFocused(id).focused;
  }, sessionId)).toBe(true);
  const directMarker = `MOBILE_DIRECT_${crypto.randomUUID().replace(/-/g, "").slice(0, 8)}`;
  await mobileSmokePage.keyboard.type(`printf '%s\\n' ${directMarker}`);
  await mobileSmokePage.keyboard.press("Enter");
  await expect.poll(() => slot.textContent()).toContain(directMarker);

  // The same permanent three-control bar accepts another draft.
  await expect(mobileSmokePage.getByTestId("chat-lead")).toHaveCount(0);
  await initialInput.fill("y");
  await expect(mobileSmokePage.getByTestId("chat-lead")).toHaveCount(0);
  await expect(mobileSmokePage.getByTestId("chat-send")).toBeVisible();

  // Tapping outside and Escape each blur without discarding or unmounting.
  await slot.click({ position: { x: 8, y: 8 } });
  await expect(box).toBeVisible();
  await expect(initialInput).not.toBeFocused();
  await expect(initialInput).toHaveValue("y");
  await initialInput.tap();
  await initialInput.press("Escape");
  await expect(box).toBeVisible();
  await expect(initialInput).not.toBeFocused();
  await expect(initialInput).toHaveValue("y");

  // Enter is a NEWLINE, never a submit: only the send button commits the draft.
  const composed = mobileSmokePage.getByTestId("chat-input");
  await composed.fill("printf 'ENTER_LINE_A\\n'");
  await composed.press("Enter");
  await mobileSmokePage.keyboard.type("printf 'ENTER_LINE_B\\n'");
  await expect(composed).toHaveValue("printf 'ENTER_LINE_A\\n'\nprintf 'ENTER_LINE_B\\n'");
  await expect(box).toBeVisible();
  await mobileSmokePage.getByTestId("chat-send").click();
  await expect.poll(() => slot.textContent()).toContain("ENTER_LINE_A");
  await expect.poll(() => slot.textContent()).toContain("ENTER_LINE_B");
  await expect(composed).toHaveValue("");
  await expect(box).toBeVisible();

  await inputSmokeTerminal(
    mobileSmokePage,
    sessionId,
    `IFS= read -r line; printf '<%s>\\n' "$line"\r`,
  );
  await mobileSmokePage.getByTestId("chat-send").click();
  await expect.poll(() => slot.textContent()).toContain("<>");
  await expect(composed).toHaveValue("");
  await expect(box).toBeVisible();

  const secondSessionId = (await spawnSmokeShell(mobileSmokePage, stack.workerFp)).session_id;
  await switchToSmokeSession(mobileSmokePage, secondSessionId);
  await expectSmokeComposer(mobileSmokePage);
  const secondInput = mobileSmokePage.getByTestId("chat-input");
  await expect(secondInput).not.toBeFocused();
  await secondInput.tap();
  await expect(secondInput).toBeFocused();
  await secondInput.press("Escape");
  await expect(mobileSmokePage.getByTestId("chat-box")).toBeVisible();
  await expect(secondInput).not.toBeFocused();
  await switchToSmokeSession(mobileSmokePage, sessionId);
  await expectSmokeComposer(mobileSmokePage);
  await switchToSmokeSession(mobileSmokePage, secondSessionId);
  await expectSmokeComposer(mobileSmokePage);
});

test("trusted keyboard input and bottom-follow behavior", async ({ smokePage, stack }) => {
  const sessionId = await smokePage.evaluate(async (workerFp) => {
    const smoke = (window as unknown as Window & { __smoke: { spawnShell(worker: string, folder: string): Promise<{ session_id: string }> } }).__smoke;
    return (await smoke.spawnShell(workerFp, "/tmp")).session_id;
  }, stack.workerFp);
  await smokePage.goto(`${stack.baseUrl}/s/${sessionId}`);
  const slot = smokePage.getByTestId(`terminal-slot-${sessionId}`);
  await expect(slot).toBeVisible();
  const marker = `PW_INPUT_${crypto.randomUUID().replace(/-/g, "").slice(0, 8)}`;
  await smokePage.keyboard.type(`printf '%s\\n' ${marker}`);
  await smokePage.keyboard.press("Enter");
  await expect.poll(() => slot.textContent()).toContain(marker);

  await smokePage.keyboard.type("for i in $(seq 1 500); do echo BOTTOMLINE-$i; done");
  await smokePage.keyboard.press("Enter");
  await expect.poll(async () => smokePage.evaluate((id) => {
    const smoke = (window as unknown as Window & { __smoke: { renderProbe(sessionId: string): { rowCount: number; atBottom: boolean } } }).__smoke;
    return smoke.renderProbe(id);
  }, sessionId)).toMatchObject({ atBottom: true });
});

test("streaming sequence repair leaves an off-bottom reader fixed", async ({ smokePage, stack }, testInfo) => {
  test.skip(testInfo.project.name !== "chromium-desktop", "desktop scroll-geometry contract");
  test.setTimeout(120_000);
  const sessionId = await smokePage.evaluate(async (workerFp) => {
    const smoke = (window as unknown as Window & {
      __smoke: { spawnShell(worker: string, folder: string): Promise<{ session_id: string }> };
    }).__smoke;
    return (await smoke.spawnShell(workerFp, "/tmp")).session_id;
  }, stack.workerFp);
  await smokePage.goto(`${stack.baseUrl}/s/${sessionId}`);
  const slot = smokePage.getByTestId(`terminal-slot-${sessionId}`);
  await expect(slot).toBeVisible();
  await smokePage.evaluate(async (id) => {
    const smoke = (window as unknown as Window & {
      __smoke: { input(sessionId: string, text: string): Promise<void> };
    }).__smoke;
    await smoke.input(id, "for i in $(seq 1 1500); do printf 'READERLINE-%04d stable-history\\n' $i; done\r");
  }, sessionId);
  await expect.poll(() => smokePage.evaluate((id) => {
    const smoke = (window as unknown as Window & {
      __smoke: { markerScan(sessionId: string, prefix: string): { max: number } };
    }).__smoke;
    return smoke.markerScan(id, "READERLINE-").max;
  }, sessionId), { timeout: 60_000 }).toBe(1500);

  const grid = slot.locator(".wterm.cell-grid");
  const box = await grid.boundingBox();
  if (!box) throw new Error("streaming reader grid missing");
  await smokePage.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
  await smokePage.mouse.wheel(0, -6000);
  const sample = () => smokePage.evaluate((id) => {
    const smoke = (window as unknown as Window & {
      __smoke: {
        cellFrameCount(sessionId: string): number;
        cellFullFrameCount(sessionId: string): number;
        cellGridEpoch(sessionId: string): string;
        droppedCellFrameCount(sessionId: string): number;
      };
    }).__smoke;
    const container = document.querySelector(
      `[data-testid="terminal-slot-${id}"] .wterm.cell-grid`,
    ) as HTMLElement;
    const rect = container.getBoundingClientRect();
    const row = document.elementFromPoint(rect.left + 100, rect.top + 200)?.closest(".cell-row");
    return {
      scrollTop: container.scrollTop,
      scrollHeight: container.scrollHeight,
      row: row?.textContent ?? "",
      rowOffset: row ? row.getBoundingClientRect().top - (rect.top + 200) : null,
      frames: smoke.cellFrameCount(id),
      gridEpoch: smoke.cellGridEpoch(id),
      fullFrames: smoke.cellFullFrameCount(id),
      dropped: smoke.droppedCellFrameCount(id),
    };
  }, sessionId);
  await expect.poll(async () => (await sample()).row, { timeout: 10_000 })
    .toContain("READERLINE-");
  const before = await sample();

  await smokePage.evaluate(async (id) => {
    const smoke = (window as unknown as Window & {
      __smoke: {
        dropNextCellFrame(sessionId: string): void;
        input(sessionId: string, text: string): Promise<void>;
      };
    }).__smoke;
    smoke.dropNextCellFrame(id);
    await smoke.input(
      id,
      "for i in $(seq 1501 1800); do printf 'READERLINE-%04d streaming\\n' $i; sleep 0.01; done\r",
    );
  }, sessionId);
  await expect.poll(() => sample(), { timeout: 60_000 }).toMatchObject({
    fullFrames: before.fullFrames + 1,
    dropped: before.dropped + 1,
  });
  await expect.poll(async () => (await sample()).frames, { timeout: 60_000 })
    .toBeGreaterThan(before.frames + 200);

  const during = await sample();
  expect(during.gridEpoch).toBe(before.gridEpoch);
  expect(during.scrollTop).toBe(before.scrollTop);
  expect(during.scrollHeight).toBe(before.scrollHeight);
  expect(during.row).toBe(before.row);
  expect(during.rowOffset).toBe(before.rowOffset);

  await smokePage.mouse.wheel(0, 100_000);
  await expect.poll(() => smokePage.evaluate((id) => {
    const smoke = (window as unknown as Window & {
      __smoke: {
        renderProbe(sessionId: string): { atBottom: boolean };
        markerScan(sessionId: string, prefix: string): { max: number };
      };
    }).__smoke;
    return {
      atBottom: smoke.renderProbe(id).atBottom,
      markerMax: smoke.markerScan(id, "READERLINE-").max,
    };
  }, sessionId), { timeout: 30_000 }).toEqual({ atBottom: true, markerMax: 1800 });
});

test("alternate screen survives width and height perturbations", async ({ smokePage, stack }) => {
  test.setTimeout(240_000);
  const sessionId = await smokePage.evaluate(async (workerFp) => {
    const smoke = (window as unknown as Window & { __smoke: { spawnShell(worker: string, folder: string): Promise<{ session_id: string }> } }).__smoke;
    return (await smoke.spawnShell(workerFp, "/tmp")).session_id;
  }, stack.workerFp);
  await smokePage.goto(`${stack.baseUrl}/s/${sessionId}`);
  await expect(smokePage.getByTestId(`terminal-slot-${sessionId}`)).toBeVisible();
  await smokePage.keyboard.type(`'${fixturePath}'`);
  await smokePage.keyboard.press("Enter");
  await expect.poll(() => smokePage.getByTestId(`terminal-slot-${sessionId}`).textContent()).toContain("CELLLINE-60");
  const result = await smokePage.evaluate(async (id) => {
    const smoke = (window as unknown as Window & {
      __smoke: {
        runRenderStress(options: {
          sessionId: string; prefix: string; screen: "main" | "alt"; iterations: number;
        }): Promise<{ verdict: string; fails: unknown[] }>;
      };
    }).__smoke;
    return smoke.runRenderStress({ sessionId: id, prefix: "CELLLINE-", screen: "alt", iterations: 80 });
  }, sessionId);
  expect(result).toMatchObject({ verdict: "PASS", fails: [] });
  await smokePage.keyboard.press("q");
});

test("two viewers preserve ordered terminal markers", async ({ smokePage, browser, stack }) => {
  const sessionId = await smokePage.evaluate(async (workerFp) => {
    const smoke = (window as unknown as Window & { __smoke: { spawnShell(worker: string, folder: string): Promise<{ session_id: string }> } }).__smoke;
    return (await smoke.spawnShell(workerFp, "/tmp")).session_id;
  }, stack.workerFp);
  await smokePage.goto(`${stack.baseUrl}/s/${sessionId}`);
  await smokePage.keyboard.type("for i in $(seq 1 120); do echo MULTIVIEW-$i; done");
  await smokePage.keyboard.press("Enter");
  const passiveContext = await browser.newContext({ viewport: { width: 1400, height: 900 } });
  await passiveContext.addInitScript(() => localStorage.setItem("roostSmoke", "1"));
  const passive = await passiveContext.newPage();
  try {
    await passive.goto(`${stack.baseUrl}/s/${sessionId}`);
    await passive.waitForFunction(() => typeof (window as unknown as Window & { __smoke?: unknown }).__smoke === "object");
    await passive.evaluate(() => (window as unknown as Window & { __smoke: { forceVisible(on: boolean): void } }).__smoke.forceVisible(true));
    await smokePage.evaluate(() => (window as unknown as Window & { __smoke: { forceVisible(on: boolean): void } }).__smoke.forceVisible(true));
    await smokePage.bringToFront();
    for (let iteration = 0; iteration < 24; iteration++) {
      await smokePage.setViewportSize({ width: 700 + (iteration % 2) * 50, height: 500 + (iteration % 3) * 40 });
    }
    const scan = await passive.evaluate((id) => {
      const smoke = (window as unknown as Window & { __smoke: { markerScan(sessionId: string, prefix: string): unknown } }).__smoke;
      return smoke.markerScan(id, "MULTIVIEW-");
    }, sessionId);
    expect(scan).toMatchObject({ duplicated: [], outOfOrder: 0 });
    await passive.bringToFront();
    for (let iteration = 0; iteration < 24; iteration++) {
      await passive.setViewportSize({ width: 1350 + (iteration % 2) * 50, height: 820 + (iteration % 3) * 40 });
    }
    await smokePage.bringToFront();
    const primaryScan = await smokePage.evaluate((id) => {
      const smoke = (window as unknown as Window & { __smoke: { markerScan(sessionId: string, prefix: string): unknown } }).__smoke;
      return smoke.markerScan(id, "MULTIVIEW-");
    }, sessionId);
    expect(primaryScan).toMatchObject({ duplicated: [], outOfOrder: 0 });
  } finally {
    await smokePage.evaluate(() => (window as unknown as Window & { __smoke: { forceVisible(on: boolean): void } }).__smoke.forceVisible(false)).catch(() => undefined);
    await passive.evaluate(() => (window as unknown as Window & { __smoke: { forceVisible(on: boolean): void } }).__smoke.forceVisible(false)).catch(() => undefined);
    await passiveContext.close();
  }
});

// A fresh deep-session attach starts with the current viewport and a truthful
// spacer only. History stays off the network and out of the DOM until demand.
test("deep-history attach/reveal paints the live tail until history is requested", async ({ smokePage, stack }, testInfo) => {
  test.skip(testInfo.project.name !== "chromium-desktop", "desktop scroll-geometry contract");
  test.setTimeout(120_000);
  const sessionId = await smokePage.evaluate(async (workerFp) => {
    const smoke = (window as unknown as Window & {
      __smoke: { spawnShell(worker: string, folder: string): Promise<{ session_id: string }> };
    }).__smoke;
    return (await smoke.spawnShell(workerFp, "/tmp")).session_id;
  }, stack.workerFp);
  await smokePage.goto(`${stack.baseUrl}/s/${sessionId}`);
  const slot = smokePage.getByTestId(`terminal-slot-${sessionId}`);
  await expect(slot).toBeVisible();
  await smokePage.keyboard.type("seq -f 'CELLLINE-%g' 1 8000");
  await smokePage.keyboard.press("Enter");
  await expect.poll(() => slot.textContent(), { timeout: 60_000 }).toContain("CELLLINE-8000");

  await smokePage.reload({ waitUntil: "domcontentloaded" });
  await smokePage.waitForFunction(() =>
    typeof (window as unknown as Window & { __smoke?: unknown }).__smoke === "object");
  await smokePage.waitForFunction((id) => {
    const pane = document.querySelector(`[data-testid="terminal-slot-${id}"]`);
    return !!pane?.querySelector(".cell-sb-spacer") && !!pane.querySelector(".cell-row");
  }, sessionId);

  const probe = () => smokePage.evaluate((id) => {
    const smoke = (window as unknown as Window & {
      __smoke: {
        renderProbe(sessionId: string): {
          mode: "cell" | "byte" | "none";
          rowCount: number;
          atBottom: boolean;
        };
        markerScan(sessionId: string, prefix: string): {
          max: number;
          duplicated: number[];
          missing: number;
          outOfOrder: number;
        };
        lastFullFrameSbRows(sessionId: string): number;
        scrollbackBackfillRequestCount(sessionId: string): number;
      };
    }).__smoke;
    return {
      ...smoke.renderProbe(id),
      markerMax: smoke.markerScan(id, "CELLLINE-").max,
      snapshotSbRows: smoke.lastFullFrameSbRows(id),
      historyRequests: smoke.scrollbackBackfillRequestCount(id),
    };
  }, sessionId);

  const attached = await probe();
  expect(attached).toMatchObject({
    mode: "cell",
    markerMax: 8000,
    atBottom: true,
    snapshotSbRows: 0,
    historyRequests: 0,
  });
  expect(attached.rowCount).toBeLessThan(100);

  const idleSamples = await smokePage.evaluate(async ({ id, rowCount, requests }) => {
    const smoke = (window as unknown as Window & {
      __smoke: {
        renderProbe(sessionId: string): { rowCount: number; atBottom: boolean };
        scrollbackBackfillRequestCount(sessionId: string): number;
      };
    }).__smoke;
    const samples: Array<{ rowCount: number; requests: number; atBottom: boolean }> = [];
    for (let frame = 0; frame < 8; frame++) {
      await new Promise<void>((resolve) => requestAnimationFrame(() => resolve()));
      samples.push({
        rowCount: smoke.renderProbe(id).rowCount,
        requests: smoke.scrollbackBackfillRequestCount(id),
        atBottom: smoke.renderProbe(id).atBottom,
      });
    }
    return { samples, expected: { rowCount, requests, atBottom: true } };
  }, { id: sessionId, rowCount: attached.rowCount, requests: attached.historyRequests });
  expect(idleSamples.samples).toEqual(Array(8).fill(idleSamples.expected));

  await smokePage.evaluate((id) => {
    const pane = document.querySelector(`[data-testid="terminal-slot-${id}"]`);
    const container = pane?.querySelector(".wterm") as HTMLElement | null;
    if (!container) return;
    container.scrollTop = 0;
    container.dispatchEvent(new Event("scroll"));
  }, sessionId);
  await smokePage.waitForFunction(({ id, previous }) => {
    const smoke = (window as unknown as Window & {
      __smoke: { scrollbackBackfillRequestCount(sessionId: string): number };
    }).__smoke;
    return smoke.scrollbackBackfillRequestCount(id) > previous;
  }, { id: sessionId, previous: attached.historyRequests });
  await smokePage.waitForFunction(({ id, previous }) => {
    const smoke = (window as unknown as Window & {
      __smoke: { renderProbe(sessionId: string): { rowCount: number } };
    }).__smoke;
    return smoke.renderProbe(id).rowCount > previous;
  }, { id: sessionId, previous: attached.rowCount });

  const demanded = await smokePage.evaluate((id) => {
    const smoke = (window as unknown as Window & {
      __smoke: {
        markerScan(sessionId: string, prefix: string): {
          duplicated: number[];
          missing: number;
          outOfOrder: number;
        };
      };
    }).__smoke;
    return smoke.markerScan(id, "CELLLINE-");
  }, sessionId);
  expect(demanded).toMatchObject({ duplicated: [], missing: 0, outOfOrder: 0 });
});

// Returning to an already-open pane keeps the Sync socket but reclaims an
// authoritative snapshot. Hidden and offscreen panes must receive no cells.
test("returning to a dormant pane reclaims once without a Sync re-dial", async ({ smokePage, stack }, testInfo) => {
  test.skip(testInfo.project.name !== "chromium-desktop", "desktop deck/visibility contract");
  const spawn = (folder: string) => smokePage.evaluate(async ({ workerFp, dir }) => {
    const smoke = (window as unknown as Window & {
      __smoke: { spawnShell(worker: string, folder: string): Promise<{ session_id: string }> };
    }).__smoke;
    return (await smoke.spawnShell(workerFp, dir)).session_id;
  }, { workerFp: stack.workerFp, dir: folder });

  const sessionA = await spawn("/tmp");
  const probe = () => smokePage.evaluate((id) => {
    const smoke = (window as unknown as Window & {
      __smoke: {
        renderProbe(sessionId: string): { atBottom: boolean };
        markerScan(sessionId: string, prefix: string): { max: number; duplicated: number[]; outOfOrder: number };
        cellFrameCount(sessionId: string): number;
        cellFullFrameCount(sessionId: string): number;
        syncWsGeneration(): number;
      };
    }).__smoke;
    const scan = smoke.markerScan(id, "CELLLINE-");
    return {
      atBottom: smoke.renderProbe(id).atBottom,
      markerMax: scan.max,
      duplicated: scan.duplicated,
      outOfOrder: scan.outOfOrder,
      frames: smoke.cellFrameCount(id),
      fullFrames: smoke.cellFullFrameCount(id),
      wsGeneration: smoke.syncWsGeneration(),
    };
  }, sessionA);

  await smokePage.goto(`${stack.baseUrl}/s/${sessionA}`);
  const slotA = smokePage.getByTestId(`terminal-slot-${sessionA}`);
  await expect(slotA).toBeVisible();
  await smokePage.keyboard.type("seq -f 'CELLLINE-%g' 1 8000");
  await smokePage.keyboard.press("Enter");
  await expect.poll(() => slotA.textContent(), { timeout: 60_000 }).toContain("CELLLINE-8000");
  await smokePage.waitForTimeout(1000);

  // Deck switch: A withdraws while offscreen, then one claim snapshot restores
  // the still-mounted renderer. The shared Sync socket stays open.
  const sessionB = await spawn("/tmp");
  await expect.poll(() => smokePage.evaluate((id) => {
    const smoke = (window as unknown as Window & { __smoke: { state(): { sessions: Record<string, unknown> } } }).__smoke;
    return id in smoke.state().sessions;
  }, sessionB)).toBe(true);
  const beforeSwitch = await probe();
  expect(beforeSwitch.markerMax).toBe(8000);

  await smokePage.evaluate((id) => {
    const smoke = (window as unknown as Window & { __smoke: { navigate(href: string): void } }).__smoke;
    smoke.navigate(`/s/${id}`);
  }, sessionB);
  await expect(smokePage.getByTestId(`tab-${sessionB}`)).toHaveAttribute("data-active", "true");
  await smokePage.waitForTimeout(1000);
  expect((await probe()).frames).toBe(beforeSwitch.frames);

  await smokePage.evaluate((id) => {
    const smoke = (window as unknown as Window & { __smoke: { navigate(href: string): void } }).__smoke;
    smoke.navigate(`/s/${id}`);
  }, sessionA);
  await expect(smokePage.getByTestId(`tab-${sessionA}`)).toHaveAttribute("data-active", "true");
  await expect.poll(async () => (await probe()).fullFrames).toBe(beforeSwitch.fullFrames + 1);
  expect(await probe()).toMatchObject({
    atBottom: true,
    markerMax: 8000,
    duplicated: [],
    outOfOrder: 0,
    wsGeneration: beforeSwitch.wsGeneration,
  });

  // Deterministic hidden pin: the page stays schedulable while lifecycle
  // handlers withdraw A. Output advances at the PTY but no cell reaches the
  // browser until visibility returns and one authoritative snapshot reclaims it.
  const beforeHide = await probe();
  await smokePage.evaluate(() => {
    const smoke = (window as unknown as Window & { __smoke: { forceHidden(on: boolean): void } }).__smoke;
    smoke.forceHidden(true);
  });
  await smokePage.evaluate(async (id) => {
    const smoke = (window as unknown as Window & { __smoke: { input(sessionId: string, text: string): Promise<void> } }).__smoke;
    await smoke.input(id, "for i in $(seq 8001 8200); do echo CELLLINE-$i; sleep 0.01; done\r");
  }, sessionA);
  await smokePage.waitForTimeout(3000);
  expect(await probe()).toMatchObject({
    markerMax: 8000,
    frames: beforeHide.frames,
    fullFrames: beforeHide.fullFrames,
    wsGeneration: beforeHide.wsGeneration,
  });

  await smokePage.evaluate(() => {
    const smoke = (window as unknown as Window & { __smoke: { forceHidden(on: boolean): void } }).__smoke;
    smoke.forceHidden(false);
  });
  await expect.poll(async () => (await probe()).markerMax, { timeout: 60_000 }).toBe(8200);
  const afterShow = await probe();
  expect(afterShow).toMatchObject({
    atBottom: true,
    markerMax: 8200,
    duplicated: [],
    outOfOrder: 0,
    fullFrames: beforeHide.fullFrames + 1,
    wsGeneration: beforeHide.wsGeneration,
  });
});

test("offline producer divergence reconnects and repaints without a reload", async ({ smokePage, stack }, testInfo) => {
  test.skip(testInfo.project.name !== "chromium-desktop", "desktop transport recovery contract");
  test.setTimeout(90_000);
  const sessionId = (await spawnSmokeShell(smokePage, stack.workerFp)).session_id;
  await navigateToSmokeSession(smokePage, sessionId);
  await expect.poll(() => smokePage.evaluate(
    (id) => (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.cellFullFrameCount(id),
    sessionId,
  )).toBeGreaterThan(0);
  await smokePage.evaluate(
    async (id) => (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.input(
      id,
      "printf 'OFFLINE-READY-%03d\\n' 1\r",
    ),
    sessionId,
  );
  await expect.poll(() => smokePage.evaluate(
    (id) => (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.viewportText(id),
    sessionId,
  )).toContain("OFFLINE-READY-001");
  await waitForStableCellFrames(smokePage, sessionId);

  const canary = `offline-${sessionId}`;
  await setRecoveryCanary(smokePage, canary);
  const before = await smokePage.evaluate((id) => {
    const smoke = (window as unknown as { __smoke: RecoverySmokeApi }).__smoke;
    return {
      frames: smoke.cellFrameCount(id),
      fullFrames: smoke.cellFullFrameCount(id),
      gridEpoch: smoke.cellGridEpoch(id),
      wsGeneration: smoke.syncWsGeneration(),
    };
  }, sessionId);

  const context = smokePage.context();
  await smokePage.evaluate(
    () => (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.pauseSyncTransport(),
  );
  try {
    await context.setOffline(true);
    await expect.poll(() => smokePage.evaluate(() => navigator.onLine)).toBe(false);
    await stack.client.sessionsInput({
      sessionId,
      data: new TextEncoder().encode(
        "for i in $(seq 1 30); do printf 'OFFLINE-RECOVER-%03d\\n' \"$i\"; sleep 0.01; done; seq 1 48; printf 'OFFLINE-CURRENT-%03d\\n' 1\r",
      ),
    });
    await expect.poll(async () => {
      const cells = await stack.client.sessionsGetScrollbackCells({
        sessionId,
        endRow: BigInt(Number.MAX_SAFE_INTEGER),
        maxRows: 250,
        gridEpoch: before.gridEpoch,
      });
      const text = cells.rows
        .map((row) => row.spans.map((span) => span.text || " ").join(""))
        .join("\n");
      return Math.max(0, ...Array.from(text.matchAll(/OFFLINE-RECOVER-(\d+)/g), (match) => Number(match[1])));
    }, { timeout: 30_000, intervals: [100] }).toBe(30);
    const isolated = await smokePage.evaluate((id) => {
      const smoke = (window as unknown as { __smoke: RecoverySmokeApi }).__smoke;
      return {
        frames: smoke.cellFrameCount(id),
        max: smoke.markerScan(id, "OFFLINE-RECOVER-").max,
      };
    }, sessionId);
    expect(isolated).toEqual({ frames: before.frames, max: 0 });
  } finally {
    await context.setOffline(false);
    await smokePage.evaluate(
      () => (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.resumeSyncTransport(),
    );
  }

  await expect.poll(() => smokePage.evaluate(
    () => (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.syncWsGeneration(),
  ), { timeout: 30_000, intervals: [100] }).toBeGreaterThan(before.wsGeneration);
  await expect.poll(() => smokePage.evaluate(
    (id) => (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.viewportText(id),
    sessionId,
  ), { timeout: 30_000, intervals: [100] }).toContain("OFFLINE-CURRENT-001");

  const recovered = await recoveryProbe(smokePage, sessionId, "OFFLINE-RECOVER-");
  expect(recovered).toMatchObject({
    canary,
    atBottom: true,
    scan: { max: 0, duplicated: [], missing: 0, outOfOrder: 0 },
  });
  expect(await smokePage.evaluate(
    (id) => (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.lastFullFrameSbRows(id),
    sessionId,
  )).toBe(0);
  const afterReconnect = await smokePage.evaluate((id) => ({
    fullFrames: (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.cellFullFrameCount(id),
    wsGeneration: (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.syncWsGeneration(),
  }), sessionId);
  expect(afterReconnect.fullFrames).toBe(before.fullFrames + 1);

  await smokePage.evaluate(
    async (id) => (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.input(
      id,
      "printf 'OFFLINE-AFTER-%03d\\n' 1\r",
    ),
    sessionId,
  );
  await expect.poll(() => smokePage.evaluate(
    (id) => (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.viewportText(id),
    sessionId,
  )).toContain("OFFLINE-AFTER-001");
  expect(await smokePage.evaluate(
    () => (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.syncWsGeneration(),
  )).toBe(afterReconnect.wsGeneration);
});

test("long hidden deep-history resume paints the current viewport before history", async ({ smokePage, stack }, testInfo) => {
  test.skip(testInfo.project.name !== "chromium-desktop", "desktop visibility and geometry contract");
  test.setTimeout(180_000);
  const sessionId = await smokePage.evaluate(async (workerFp) => {
    const smoke = (window as unknown as Window & { __smoke: RecoverySmokeApi }).__smoke;
    return (await smoke.spawnShell(workerFp, "/tmp")).session_id;
  }, stack.workerFp);
  await smokePage.goto(`${stack.baseUrl}/s/${sessionId}`);
  const slot = smokePage.getByTestId(`terminal-slot-${sessionId}`);
  await expect(slot).toBeVisible();
  await smokePage.keyboard.type("seq -f 'HIDDEN-%g' 1 9000");
  await smokePage.keyboard.press("Enter");
  await expect.poll(() => slot.textContent(), { timeout: 60_000 }).toContain("HIDDEN-9000");
  await expect.poll(() => smokePage.evaluate((id) => {
    const smoke = (window as unknown as Window & {
      __smoke: { renderProbe(sessionId: string): { mode: "cell" | "byte" | "none" } };
    }).__smoke;
    return smoke.renderProbe(id).mode;
  }, sessionId)).toBe("cell");

  const control = await smokePage.context().newPage();
  try {
    await control.goto(`${stack.baseUrl}/s/${sessionId}`, { waitUntil: "domcontentloaded" });
    await control.waitForFunction(() =>
      typeof (window as unknown as Window & { __smoke?: unknown }).__smoke === "object");
    await control.evaluate(() => {
      const smoke = (window as unknown as Window & {
        __smoke: { forceVisible(on: boolean): void };
      }).__smoke;
      smoke.forceVisible(true);
    });
    await control.waitForFunction((id) => {
      const smoke = (window as unknown as Window & {
        __smoke: { viewportText(sessionId: string): string };
      }).__smoke;
      return smoke.viewportText(id).includes("HIDDEN-9000");
    }, sessionId);
    await smokePage.bringToFront();

    const sentinel = await smokePage.evaluate(() => {
      const key = `__roostResumeSentinel_${crypto.randomUUID().replaceAll("-", "")}`;
      const nonce = crypto.randomUUID();
      Object.defineProperty(document, key, {
        value: Object.freeze({ nonce }),
        configurable: false,
        enumerable: false,
      });
      return { key, nonce };
    });
    const before = await smokePage.evaluate((id) => {
      const smoke = (window as unknown as Window & {
        __smoke: {
          cellFrameCount(sessionId: string): number;
          renderProbe(sessionId: string): { rowCount: number; atBottom: boolean };
          scrollbackBackfillRequestCount(sessionId: string): number;
          syncWsGeneration(): number;
          forceHidden(on: boolean): void;
        };
      }).__smoke;
      const result = {
        frames: smoke.cellFrameCount(id),
        requests: smoke.scrollbackBackfillRequestCount(id),
        generation: smoke.syncWsGeneration(),
        ...smoke.renderProbe(id),
      };
      smoke.forceHidden(true);
      return result;
    }, sessionId);
    expect(before.atBottom).toBe(true);

    // Stay dormant beyond the retired 60 s hidden-stream grace. No cell frames
    // may reach this withdrawn viewer during the entire interval.
    await smokePage.waitForTimeout(62_000);
    const currentMarker = `CURRENT_${crypto.randomUUID().replaceAll("-", "")}`;
    await smokePage.evaluate(() => {
      const smoke = (window as unknown as Window & { __smoke: RecoverySmokeApi }).__smoke;
      smoke.forceSyncRetryExhausted();
    });
    await control.evaluate(async ({ id, marker }) => {
      const smoke = (window as unknown as Window & {
        __smoke: { input(sessionId: string, text: string): Promise<void> };
      }).__smoke;
      await smoke.input(id, `printf '%s\\n' ${marker}\r`);
    }, { id: sessionId, marker: currentMarker });
    await control.waitForFunction(({ id, marker }) => {
      const smoke = (window as unknown as Window & {
        __smoke: { viewportText(sessionId: string): string };
      }).__smoke;
      return smoke.viewportText(id).includes(marker);
    }, { id: sessionId, marker: currentMarker });
    expect(await smokePage.evaluate((id) => {
      const smoke = (window as unknown as Window & {
        __smoke: { cellFrameCount(sessionId: string): number };
      }).__smoke;
      return smoke.cellFrameCount(id);
    }, sessionId)).toBe(before.frames);

    await smokePage.evaluate(({ id, marker }) => {
      type ResumeSample = {
        current: boolean;
        rowCount: number;
        top: number;
        height: number;
        client: number;
        snapshotSbRows: number;
        historyRequests: number;
      };
      const runtime = window as unknown as Window & {
        __resumeSamples: ResumeSample[];
        __resumeSampling: boolean;
      };
      const smoke = (window as unknown as Window & {
        __smoke: {
          lastFullFrameSbRows(sessionId: string): number;
          scrollbackBackfillRequestCount(sessionId: string): number;
        };
      }).__smoke;
      runtime.__resumeSamples = [];
      runtime.__resumeSampling = true;
      const sample = () => {
        if (!runtime.__resumeSampling) return;
        const pane = document.querySelector(`[data-testid="terminal-slot-${id}"]`);
        const container = pane?.querySelector(".wterm") as HTMLElement | null;
        if (container) {
          const box = container.getBoundingClientRect();
          let current = false;
          for (const row of container.querySelectorAll(".cell-row")) {
            const rowBox = row.getBoundingClientRect();
            if (rowBox.bottom <= box.top + 1 || rowBox.top >= box.bottom - 1) continue;
            if ((row.textContent ?? "").includes(marker)) current = true;
          }
          runtime.__resumeSamples.push({
            current,
            rowCount: container.querySelectorAll(".cell-row").length,
            top: container.scrollTop,
            height: container.scrollHeight,
            client: container.clientHeight,
            snapshotSbRows: smoke.lastFullFrameSbRows(id),
            historyRequests: smoke.scrollbackBackfillRequestCount(id),
          });
        }
        requestAnimationFrame(sample);
      };
      requestAnimationFrame(sample);
    }, { id: sessionId, marker: currentMarker });

    await smokePage.evaluate(() => {
      const smoke = (window as unknown as Window & {
        __smoke: { forceHidden(on: boolean): void };
      }).__smoke;
      smoke.forceHidden(false);
    });
    await smokePage.waitForFunction(({ id, marker }) => {
      const smoke = (window as unknown as Window & {
        __smoke: { viewportText(sessionId: string): string };
      }).__smoke;
      return smoke.viewportText(id).includes(marker);
    }, { id: sessionId, marker: currentMarker }, { timeout: 30_000 });
    await smokePage.evaluate(async () => {
      for (let frame = 0; frame < 8; frame++) {
        await new Promise<void>((resolve) => requestAnimationFrame(() => resolve()));
      }
    });
    const recovered = await smokePage.evaluate(({ key, nonce }) => {
      type ResumeSample = {
        current: boolean;
        rowCount: number;
        top: number;
        height: number;
        client: number;
        snapshotSbRows: number;
        historyRequests: number;
      };
      const runtime = window as unknown as Window & {
        __resumeSamples: ResumeSample[];
        __resumeSampling: boolean;
      };
      runtime.__resumeSampling = false;
      const value = (document as unknown as Record<string, unknown>)[key] as { nonce?: string } | undefined;
      return { samples: runtime.__resumeSamples, sentinelSurvived: value?.nonce === nonce };
    }, sentinel);
    expect(recovered.sentinelSurvived).toBe(true);
    const authoritativeAt = recovered.samples.findIndex((sample) => sample.current);
    expect(authoritativeAt).toBeGreaterThanOrEqual(0);
    const authoritative = recovered.samples[authoritativeAt]!;
    expect(authoritative.top).toBeGreaterThanOrEqual(
      authoritative.height - authoritative.client - 2,
    );
    expect(authoritative.snapshotSbRows).toBe(0);
    expect(authoritative.historyRequests).toBe(before.requests);
    expect(authoritative.rowCount).toBeLessThan(100);
    expect(recovered.samples.slice(authoritativeAt).every((sample) =>
      sample.rowCount === authoritative.rowCount
      && sample.historyRequests === before.requests
    )).toBe(true);
    expect(await smokePage.evaluate(() => {
      const smoke = (window as unknown as Window & {
        __smoke: { syncWsGeneration(): number };
      }).__smoke;
      return smoke.syncWsGeneration();
    })).toBeGreaterThan(before.generation);

    await smokePage.evaluate((id) => {
      const pane = document.querySelector(`[data-testid="terminal-slot-${id}"]`);
      const container = pane?.querySelector(".wterm") as HTMLElement | null;
      if (!container) return;
      container.scrollTop = 0;
      container.dispatchEvent(new Event("scroll"));
    }, sessionId);
    await smokePage.waitForFunction(({ id, previous }) => {
      const smoke = (window as unknown as Window & {
        __smoke: { scrollbackBackfillRequestCount(sessionId: string): number };
      }).__smoke;
      return smoke.scrollbackBackfillRequestCount(id) > previous;
    }, { id: sessionId, previous: before.requests });
    await smokePage.waitForFunction((id) => {
      const smoke = (window as unknown as Window & {
        __smoke: { markerScan(sessionId: string, prefix: string): RecoveryMarkerScan };
      }).__smoke;
      const scan = smoke.markerScan(id, "HIDDEN-");
      return scan.duplicated.length === 0 && scan.missing === 0 && scan.outOfOrder === 0;
    }, sessionId);
    const history = await smokePage.evaluate((id) => {
      const smoke = (window as unknown as Window & {
        __smoke: { markerScan(sessionId: string, prefix: string): RecoveryMarkerScan };
      }).__smoke;
      return smoke.markerScan(id, "HIDDEN-");
    }, sessionId);
    expect(history).toMatchObject({ duplicated: [], missing: 0, outOfOrder: 0 });
  } finally {
    await smokePage.evaluate(() => {
      const smoke = (window as unknown as Window & {
        __smoke: { forceHidden(on: boolean): void };
      }).__smoke;
      smoke.forceHidden(false);
    }).catch(() => undefined);
    await control.evaluate(() => {
      const smoke = (window as unknown as Window & {
        __smoke: { forceVisible(on: boolean): void };
      }).__smoke;
      smoke.forceVisible(false);
    }).catch(() => undefined);
    await control.close();
  }
});

// A stale pane may retain deep painted history while parked, but its catch-up
// frame replaces that obsolete image with only the current viewport. No
// history page may race the reveal or move the reader away from the bottom.
test("deck switch to a stale deep-history pane lands at the live bottom instantly", async ({ smokePage, stack }, testInfo) => {
  test.skip(testInfo.project.name !== "chromium-desktop", "desktop scroll-geometry contract");
  test.setTimeout(180_000);
  const sessionId = await smokePage.evaluate(async (workerFp) => {
    const smoke = (window as unknown as Window & {
      __smoke: { spawnShell(worker: string, folder: string): Promise<{ session_id: string }> };
    }).__smoke;
    return (await smoke.spawnShell(workerFp, "/tmp")).session_id;
  }, stack.workerFp);
  await smokePage.goto(`${stack.baseUrl}/s/${sessionId}`);
  const slot = smokePage.getByTestId(`terminal-slot-${sessionId}`);
  await expect(slot).toBeVisible();
  await smokePage.keyboard.type("seq -f 'SWL-%g' 1 8000");
  await smokePage.keyboard.press("Enter");
  await expect.poll(() => slot.textContent(), { timeout: 60_000 }).toContain("SWL-8000");

  const siblings: string[] = [];
  for (let index = 0; index < 5; index++) {
    const siblingId = await smokePage.evaluate(async (workerFp) => {
      const smoke = (window as unknown as Window & {
        __smoke: { spawnShell(worker: string, folder: string): Promise<{ session_id: string }> };
      }).__smoke;
      return (await smoke.spawnShell(workerFp, "/tmp")).session_id;
    }, stack.workerFp);
    await smokePage.waitForFunction((id) => {
      const smoke = (window as unknown as Window & {
        __smoke: { state(): { sessions: Record<string, unknown> } };
      }).__smoke;
      return id in smoke.state().sessions;
    }, siblingId);
    await smokePage.evaluate((id) => {
      const smoke = (window as unknown as Window & { __smoke: { navigate(href: string): void } }).__smoke;
      smoke.navigate(`/s/${id}`);
    }, siblingId);
    await expect(smokePage.getByTestId(`tab-${siblingId}`)).toHaveAttribute("data-active", "true");
    siblings.push(siblingId);
  }
  await smokePage.waitForTimeout(1200);

  const before = await smokePage.evaluate((id) => {
    const smoke = (window as unknown as Window & {
      __smoke: {
        cellFrameCount(sessionId: string): number;
        renderProbe(sessionId: string): { rowCount: number; atBottom: boolean };
        scrollbackBackfillRequestCount(sessionId: string): number;
      };
    }).__smoke;
    return {
      frames: smoke.cellFrameCount(id),
      requests: smoke.scrollbackBackfillRequestCount(id),
      ...smoke.renderProbe(id),
    };
  }, sessionId);
  expect(before.rowCount).toBeGreaterThan(1500);
  expect(before.atBottom).toBe(true);

  await smokePage.evaluate(async (id) => {
    const smoke = (window as unknown as Window & {
      __smoke: { input(sessionId: string, text: string): Promise<void> };
    }).__smoke;
    await smoke.input(id, "seq -f 'FRESH-%g' 1 300\r");
  }, sessionId);
  await smokePage.waitForTimeout(500);
  expect(await smokePage.evaluate((id) => {
    const smoke = (window as unknown as Window & {
      __smoke: { cellFrameCount(sessionId: string): number };
    }).__smoke;
    return smoke.cellFrameCount(id);
  }, sessionId)).toBe(before.frames);

  await smokePage.getByTestId(`tab-${sessionId}`).click();
  const reveal = await smokePage.evaluate(async ({ id, priorRequests }) => {
    const smoke = (window as unknown as Window & {
      __smoke: {
        lastFullFrameSbRows(sessionId: string): number;
        scrollbackBackfillRequestCount(sessionId: string): number;
      };
    }).__smoke;
    const samples: Array<{
      visibleFresh: number;
      painted: number;
      top: number;
      height: number;
      client: number;
      snapshotSbRows: number;
      historyRequests: number;
    }> = [];
    let authoritativeAt = -1;
    for (let frame = 0; frame < 180; frame++) {
      await new Promise<void>((resolve) => requestAnimationFrame(() => resolve()));
      const pane = document.querySelector(`[data-testid="terminal-slot-${id}"]`);
      const container = pane?.querySelector(".wterm") as HTMLElement | null;
      if (!container) continue;
      const box = container.getBoundingClientRect();
      let visibleFresh = -1;
      for (const row of container.querySelectorAll(".cell-row")) {
        const rowBox = row.getBoundingClientRect();
        if (rowBox.bottom <= box.top + 1 || rowBox.top >= box.bottom - 1) continue;
        const match = (row.textContent ?? "").match(/FRESH-(\d+)/);
        if (match) visibleFresh = Math.max(visibleFresh, Number(match[1]));
      }
      samples.push({
        visibleFresh,
        painted: container.querySelectorAll(".cell-row").length,
        top: container.scrollTop,
        height: container.scrollHeight,
        client: container.clientHeight,
        snapshotSbRows: smoke.lastFullFrameSbRows(id),
        historyRequests: smoke.scrollbackBackfillRequestCount(id),
      });
      if (visibleFresh === 300 && authoritativeAt < 0) authoritativeAt = samples.length - 1;
      if (authoritativeAt >= 0 && samples.length >= authoritativeAt + 8) break;
    }
    return { samples, authoritativeAt, priorRequests };
  }, { id: sessionId, priorRequests: before.requests });

  expect(reveal.authoritativeAt).toBeGreaterThanOrEqual(0);
  const authoritative = reveal.samples[reveal.authoritativeAt]!;
  expect(authoritative.top).toBeGreaterThanOrEqual(authoritative.height - authoritative.client - 2);
  expect(authoritative.snapshotSbRows).toBe(0);
  expect(authoritative.historyRequests).toBe(reveal.priorRequests);
  expect(authoritative.painted).toBeLessThan(100);
  expect(reveal.samples.slice(reveal.authoritativeAt).map((sample) => ({
    painted: sample.painted,
    historyRequests: sample.historyRequests,
  }))).toEqual(Array(reveal.samples.length - reveal.authoritativeAt).fill({
    painted: authoritative.painted,
    historyRequests: reveal.priorRequests,
  }));

  await smokePage.evaluate((id) => {
    const pane = document.querySelector(`[data-testid="terminal-slot-${id}"]`);
    const container = pane?.querySelector(".wterm") as HTMLElement | null;
    if (!container) return;
    container.scrollTop = 0;
    container.dispatchEvent(new Event("scroll"));
  }, sessionId);
  await smokePage.waitForFunction(({ id, previous }) => {
    const smoke = (window as unknown as Window & {
      __smoke: { scrollbackBackfillRequestCount(sessionId: string): number };
    }).__smoke;
    return smoke.scrollbackBackfillRequestCount(id) > previous;
  }, { id: sessionId, previous: reveal.priorRequests });
  await smokePage.waitForFunction((id) => {
    const smoke = (window as unknown as Window & {
      __smoke: {
        markerScan(sessionId: string, prefix: string): {
          duplicated: number[];
          missing: number;
          outOfOrder: number;
        };
      };
    }).__smoke;
    const scan = smoke.markerScan(id, "SWL-");
    return scan.duplicated.length === 0 && scan.missing === 0 && scan.outOfOrder === 0;
  }, sessionId);
  const scan = await smokePage.evaluate((id) => {
    const smoke = (window as unknown as Window & {
      __smoke: {
        markerScan(sessionId: string, prefix: string): {
          duplicated: number[];
          missing: number;
          outOfOrder: number;
        };
      };
    }).__smoke;
    return smoke.markerScan(id, "SWL-");
  }, sessionId);
  expect(scan).toMatchObject({ duplicated: [], missing: 0, outOfOrder: 0 });
});

// Bottom-follow must survive geometry changes that happen while a pane is
// parked: the box shrinks (window resize / keyboard inset / divider drag),
// nothing re-samples the bottom, and pre-noteBoxResize the pane revealed
// off-bottom with live output landing below the fold — permanently.
test("a pane revealed after the window shrank is still at the bottom", async ({ smokePage, stack }, testInfo) => {
  test.skip(testInfo.project.name !== "chromium-desktop", "desktop scroll-geometry contract");
  const sessionId = await smokePage.evaluate(async (workerFp) => {
    const smoke = (window as unknown as Window & { __smoke: { spawnShell(worker: string, folder: string): Promise<{ session_id: string }> } }).__smoke;
    return (await smoke.spawnShell(workerFp, "/tmp")).session_id;
  }, stack.workerFp);
  await smokePage.goto(`${stack.baseUrl}/s/${sessionId}`);
  const slot = smokePage.getByTestId(`terminal-slot-${sessionId}`);
  await expect(slot).toBeVisible();
  await smokePage.keyboard.type("seq -f 'SHRK-%g' 1 600");
  await smokePage.keyboard.press("Enter");
  await expect.poll(() => slot.textContent(), { timeout: 30_000 }).toContain("SHRK-600");

  // Park A behind a sibling, then shrink the window UNDER the parked pane.
  const siblingId = await smokePage.evaluate(async (workerFp) => {
    const smoke = (window as unknown as Window & { __smoke: { spawnShell(worker: string, folder: string): Promise<{ session_id: string }> } }).__smoke;
    return (await smoke.spawnShell(workerFp, "/tmp")).session_id;
  }, stack.workerFp);
  await smokePage.evaluate((id) => {
    const smoke = (window as unknown as Window & { __smoke: { navigate(href: string): void } }).__smoke;
    smoke.navigate(`/s/${id}`);
  }, siblingId);
  await expect(smokePage.getByTestId(`tab-${siblingId}`)).toHaveAttribute("data-active", "true");
  const vp = smokePage.viewportSize()!;
  // −100 px keeps the short side ≥ 600 (windowSizeClass compact boundary keys
  // on min(w,h)): the pane must SHRINK, not flip the whole app to mobile UI.
  await smokePage.setViewportSize({ width: vp.width, height: vp.height - 100 });
  await smokePage.waitForTimeout(400); // park restyle + ResizeObserver tick

  // Reveal: the FIRST sample with painted rows is already at the bottom.
  await smokePage.evaluate((id) => {
    const w = window as unknown as Window & { __shrkSamples?: Array<{ painted: number; top: number; height: number; client: number }>; __shrkTimer?: number };
    const slotEl = document.querySelector(`[data-testid="terminal-slot-${id}"]`);
    const c = slotEl?.querySelector(".wterm") as HTMLElement | null;
    const samples: Array<{ painted: number; top: number; height: number; client: number }> = [];
    w.__shrkSamples = samples;
    w.__shrkTimer = window.setInterval(() => {
      if (!c) return;
      samples.push({
        painted: c.querySelectorAll(".cell-row").length,
        top: c.scrollTop, height: c.scrollHeight, client: c.clientHeight,
      });
    }, 50);
  }, sessionId);
  await smokePage.getByTestId(`tab-${sessionId}`).click();
  await expect.poll(() => smokePage.evaluate((id) => {
    const smoke = (window as unknown as Window & {
      __smoke: { renderProbe(sessionId: string): { atBottom: boolean; rowCount: number } };
    }).__smoke;
    const p = smoke.renderProbe(id);
    return p.rowCount > 0 && p.atBottom;
  }, sessionId), { timeout: 1_500, intervals: [50] }).toBe(true);
  await smokePage.waitForTimeout(1_000);
  const samples = await smokePage.evaluate(() => {
    const w = window as unknown as Window & { __shrkSamples?: Array<{ painted: number; top: number; height: number; client: number }>; __shrkTimer?: number };
    if (w.__shrkTimer !== undefined) window.clearInterval(w.__shrkTimer);
    return w.__shrkSamples ?? [];
  });
  expect(samples.length).toBeGreaterThan(5);
  const offBottom = samples.filter((s) => s.painted > 0 && s.top < s.height - s.client - 2);
  expect(offBottom).toEqual([]);
  // The newest marker is visible at the bottom of the SHRUNKEN box.
  const lastVisible = await smokePage.evaluate((id) => {
    const slotEl = document.querySelector(`[data-testid="terminal-slot-${id}"]`);
    const c = slotEl?.querySelector(".wterm") as HTMLElement | null;
    if (!c) return -1;
    const box = c.getBoundingClientRect();
    let max = -1;
    for (const row of c.querySelectorAll(".cell-row")) {
      const r = row.getBoundingClientRect();
      if (r.bottom <= box.top + 1 || r.top >= box.bottom - 1) continue;
      const m = (row.textContent ?? "").match(/SHRK-(\d+)/);
      if (m) max = Math.max(max, Number(m[1]));
    }
    return max;
  }, sessionId);
  expect(lastVisible).toBe(600);
});

// A /file/… visit must NOT tear down the terminal deck: MainPane's screens
// share ONE route definition (App.tsx) and the deck host only flips
// visibility (MainPane.tsx). Separate <Route> entries remounted MainPane on
// every /s ↔ /file crossing — every renderer died, warmSessionIds reset, and
// the return was a cold mount + claim storm. Node identity + a flat full-frame
// count are the remount detectors; nothing else in this suite navigates /file,
// which is exactly why the regression slipped past green runs.
test("a /file round-trip keeps the deck warm and costs no snapshot", async ({ smokePage, stack }, testInfo) => {
  test.skip(testInfo.project.name !== "chromium-desktop", "desktop deck-persistence contract");
  const sessionId = await smokePage.evaluate(async (workerFp) => {
    const smoke = (window as unknown as Window & { __smoke: { spawnShell(worker: string, folder: string): Promise<{ session_id: string }> } }).__smoke;
    return (await smoke.spawnShell(workerFp, "/tmp")).session_id;
  }, stack.workerFp);
  await smokePage.goto(`${stack.baseUrl}/s/${sessionId}`);
  const slot = smokePage.getByTestId(`terminal-slot-${sessionId}`);
  await expect(slot).toBeVisible();
  await smokePage.keyboard.type("seq -f 'FRT-%g' 1 300");
  await smokePage.keyboard.press("Enter");
  await expect.poll(() => slot.textContent(), { timeout: 30_000 }).toContain("FRT-300");

  const baseline = await smokePage.evaluate((id) => {
    const smoke = (window as unknown as Window & {
      __smoke: { cellFullFrameCount(sessionId: string): number; syncWsGeneration(): number };
    }).__smoke;
    const deck = document.querySelector('[data-testid="terminal-deck"]') as (HTMLElement & { __persistCanary?: string }) | null;
    if (deck) deck.__persistCanary = "alive";
    return { fullFrames: smoke.cellFullFrameCount(id), wsGeneration: smoke.syncWsGeneration(), canarySet: !!deck };
  }, sessionId);
  expect(baseline.canarySet).toBe(true);

  // Into the file viewer: deck stays in the DOM, merely visibility-hidden.
  await smokePage.evaluate((fp) => {
    const smoke = (window as unknown as Window & { __smoke: { navigate(href: string): void } }).__smoke;
    smoke.navigate(`/file/${fp}/tmp/roost-frt-missing.txt`);
  }, stack.workerFp);
  await expect.poll(() => smokePage.evaluate(() => {
    const deck = document.querySelector('[data-testid="terminal-deck"]') as (HTMLElement & { __persistCanary?: string }) | null;
    if (!deck?.parentElement) return null;
    return { canary: deck.__persistCanary ?? null, hostVis: getComputedStyle(deck.parentElement).visibility };
  })).toEqual({ canary: "alive", hostVis: "hidden" });

  // The retained folder keeps viewport state warm, not keyboard ownership.
  // A reserved deck chord while the file viewer owns the surface must not
  // mutate the hidden layout.
  await pressPlatformShortcut(smokePage, "spotlight", "Enter");
  await expect(slot).not.toHaveAttribute("data-spotlit", "true");

  // And back: the SAME deck node, zero full frames, zero re-dials — the
  // return is a pure visibility flip, and the pane is still at the bottom.
  await smokePage.evaluate((id) => {
    const smoke = (window as unknown as Window & { __smoke: { navigate(href: string): void } }).__smoke;
    smoke.navigate(`/s/${id}`);
  }, sessionId);
  await expect(slot).toBeVisible();
  await expect.poll(() => smokePage.evaluate((id) => {
    const smoke = (window as unknown as Window & {
      __smoke: {
        cellFullFrameCount(sessionId: string): number;
        syncWsGeneration(): number;
        renderProbe(sessionId: string): { atBottom: boolean; rowCount: number };
        markerScan(sessionId: string, prefix: string): { max: number; duplicated: number[]; outOfOrder: number };
      };
    }).__smoke;
    const deck = document.querySelector('[data-testid="terminal-deck"]') as (HTMLElement & { __persistCanary?: string }) | null;
    if (!deck?.parentElement) return null;
    const scan = smoke.markerScan(id, "FRT-");
    return {
      canary: deck.__persistCanary ?? null,
      hostVis: getComputedStyle(deck.parentElement).visibility,
      fullFrames: smoke.cellFullFrameCount(id),
      wsGeneration: smoke.syncWsGeneration(),
      atBottom: smoke.renderProbe(id).atBottom,
      markerMax: scan.max,
      duplicated: scan.duplicated,
      outOfOrder: scan.outOfOrder,
    };
  }, sessionId)).toEqual({
    canary: "alive",
    hostVis: "visible",
    fullFrames: baseline.fullFrames,
    wsGeneration: baseline.wsGeneration,
    atBottom: true,
    markerMax: 300,
    duplicated: [],
    outOfOrder: 0,
  });
});
