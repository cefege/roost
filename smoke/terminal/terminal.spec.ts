import { fileURLToPath } from "node:url";
import { test, expect } from "./fixtures.ts";
import type { Page } from "@playwright/test";
import { dirname, join } from "node:path";

const fixturePath = join(dirname(fileURLToPath(import.meta.url)), "resize-tui.ts");

interface RecoveryMarkerScan {
  total: number; unique: number; min: number; max: number;
  duplicated: number[]; missing: number; outOfOrder: number; firstInversion: number;
}

interface RecoverySmokeApi {
  spawnShell(workerFp: string, folder: string, sessionId?: string): Promise<{ session_id: string; channel_id: number }>;
  createWorkspace(workerFp: string, folder: string, sessionId: string): Promise<{ id: string; channel: number }>;
  navigate(href: string): void;
  input(sessionId: string, text: string): Promise<void>;
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

// Auto-open is once per pane mount (CellTerminal's composerDefaultConsumed), so
// after a pane switch or a reload the bar may land closed. Retention is what the
// draft specs assert, not which state the bar happens to be in.
async function openSmokeComposer(page: Page): Promise<void> {
  await expect(page.getByTestId("mobile-chat-input")).toBeVisible();
  const toggle = page.getByTestId("terminal-chat-toggle");
  if (await toggle.count() > 0) await toggle.click();
  await expect(page.getByTestId("chat-input")).toBeVisible();
}
async function inputSmokeTerminal(page: Page, sessionId: string, text: string): Promise<void> {
  await page.evaluate(async ({ id, input }) => {
    // The smoke backdoor is injected only in smoke-enabled browser contexts.
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    await smokeWindow.__smoke.input(id, input);
  }, { id: sessionId, input: text });
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
  await expect.poll(() => smokePage.evaluate(
    (id) => (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.viewportText(id)
      .includes("HEARTBEAT-READY-001bash-5.1$"),
    sessionId,
  )).toBe(true);
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
  await expect(smokePage.getByTestId("chat-box")).toHaveCount(0);
  await expect(smokePage.getByTestId("chat-input")).toHaveCount(0);
  await expect(smokePage.getByTestId("terminal-chat-toggle")).toHaveCount(0);

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

test("nav pad taps reach the PTY without focusing the terminal textarea", async ({ mobileSmokePage, stack }, testInfo) => {
  test.skip(testInfo.project.name !== "webkit-iphone", "iPhone terminal input contract");
  const sessionId = await mobileSmokePage.evaluate(async (workerFp) => {
    const smoke = (window as unknown as Window & { __smoke: { spawnShell(worker: string, folder: string): Promise<{ session_id: string }> } }).__smoke;
    return (await smoke.spawnShell(workerFp, "/tmp")).session_id;
  }, stack.workerFp);
  await mobileSmokePage.goto(`${stack.baseUrl}/s/${sessionId}`);
  const slot = mobileSmokePage.getByTestId(`terminal-slot-${sessionId}`);
  await expect(slot).toBeVisible();

  await mobileSmokePage.keyboard.type("cat -vet");
  await mobileSmokePage.keyboard.press("Enter");
  await mobileSmokePage.getByTestId("terminal-nav-toggle").click();

  const paneFocused = () => mobileSmokePage.evaluate((id) => {
    const smoke = (window as unknown as Window & {
      __smoke: {
        paneFocused(sessionId: string): {
          hasSlot: boolean;
          hasTextarea: boolean;
          focused: boolean;
        };
      };
    }).__smoke;
    return smoke.paneFocused(id);
  }, sessionId);
  await mobileSmokePage.evaluate(() => {
    (document.activeElement as HTMLElement | null)?.blur();
    document.body.tabIndex = -1;
    document.body.focus();
  });
  await expect.poll(paneFocused).toMatchObject({ hasTextarea: true, focused: false });

  const clickWithoutFocus = async (testId: string) => {
    await mobileSmokePage.getByTestId(testId).click();
    await expect.poll(async () => (await paneFocused()).focused).toBe(false);
  };
  for (const testId of [
    "nav-up",
    "nav-down",
    "nav-left",
    "nav-right",
    "nav-home",
    "nav-end",
    "nav-pgup",
    "nav-pgdn",
    "nav-esc",
    "nav-tab",
    "nav-mouse",
    "nav-mouse",
    "nav-ctrl",
    "nav-ctrl",
    "terminal-nav-toggle",
    "terminal-nav-toggle",
  ]) {
    await clickWithoutFocus(testId);
  }
  await expect(mobileSmokePage.getByTestId("nav-mouse")).toHaveAttribute("aria-pressed", "false");
  await expect(mobileSmokePage.getByTestId("nav-ctrl")).toHaveAttribute("aria-pressed", "false");
  await expect(mobileSmokePage.getByTestId("terminal-nav-toggle")).toHaveAttribute("data-open", "true");

  await clickWithoutFocus("nav-enter");
  await expect.poll(() => slot.textContent()).toContain("^I$");
});

// The bar used to flip between a one-row and a two-row layout: a measured
// 2-line height set data-multiline, which widened the field to full width, at
// which the same text fit on ONE line, which narrowed it again — so every
// keystroke in the ~28-44 character band toggled the whole shape. The layout
// decision fed the measurement that produced it. The invariant that forbids
// that feedback edge is asserted here: nothing in the bar moves HORIZONTALLY as
// the draft grows, whatever its length; only the field's height changes.
test("mobile composer keeps one control row at every draft length", async ({ mobileSmokePage, stack }) => {
  const sessionId = (await spawnSmokeShell(mobileSmokePage, stack.workerFp)).session_id;
  await navigateToSmokeSession(mobileSmokePage, sessionId);

  const input = mobileSmokePage.getByTestId("chat-input");
  const mic = mobileSmokePage.getByTestId("mobile-voice-input");
  const send = mobileSmokePage.getByTestId("chat-send");

  const composerGeometry = async () => {
    const [inputBox, micBox, sendBox] = await Promise.all([
      input.boundingBox(),
      mic.boundingBox(),
      send.boundingBox(),
    ]);
    expect(inputBox).not.toBeNull();
    expect(micBox).not.toBeNull();
    expect(sendBox).not.toBeNull();
    return {
      input: inputBox!,
      mic: micBox!,
      send: sendBox!,
    };
  };

  // The pill animates in (voice-caption-enter: a 200ms translateY). Measuring
  // before it settles bakes a sub-pixel offset into the baseline that the
  // exact-equality check at the end would then report as a layout change.
  await mobileSmokePage.getByTestId("chat-box")
    .evaluate(async (el) => { await Promise.all(el.getAnimations().map((a) => a.finished)); });
  await input.fill("short");
  const baseline = await composerGeometry();

  const expectOneRow = async (label: string) => {
    const geometry = await composerGeometry();
    const near = (actual: number, expected: number, tol: number, what: string) =>
      expect(Math.abs(actual - expected), `${label}: ${what}`).toBeLessThanOrEqual(tol);
    near(geometry.input.x, baseline.input.x, 1, "input.x");
    near(geometry.input.width, baseline.input.width, 1, "input.width");
    near(geometry.mic.x, baseline.mic.x, 1, "mic.x");
    near(geometry.send.x, baseline.send.x, 1, "send.x");
    // The field grows upward; the controls stay pinned to its bottom edge.
    const fieldBottom = geometry.input.y + geometry.input.height;
    near(geometry.mic.y + geometry.mic.height, fieldBottom, 2, "mic bottom");
    near(geometry.send.y + geometry.send.height, fieldBottom, 2, "send bottom");
    return geometry;
  };

  // An unbreakable token is the worst case for the wrap boundary.
  const heights: number[] = [];
  for (const length of [24, 28, 32, 36, 40, 44, 60, 240]) {
    await input.fill("W".repeat(length));
    heights.push((await expectOneRow(`W x ${length}`)).input.height);
  }
  // …and a draft the browser CAN break must hold the same row.
  await input.fill("word ".repeat(9));
  await expectOneRow("spaced draft");

  // What DOES change is the field's height: monotonic in the draft length,
  // strictly taller than one line at 240, and capped by the CSS max-height.
  for (let i = 1; i < heights.length; i++) {
    expect(heights[i]!, `height at step ${i}`).toBeGreaterThanOrEqual(heights[i - 1]! - 0.5);
  }
  expect(heights.at(-1)!).toBeGreaterThan(baseline.input.height);
  for (const height of heights) expect(height).toBeLessThanOrEqual(160);

  await input.fill("short");
  expect(await composerGeometry()).toEqual(baseline);
});

// The bar is exactly three controls in every state — field, mic, send — and
// never grows an attachment affordance. Escape now has a single layer: it
// collapses the bar and keeps the draft.
test("mobile composer is field + mic + send only", async ({ mobileSmokePage, stack }) => {
  const sessionId = (await spawnSmokeShell(mobileSmokePage, stack.workerFp)).session_id;
  await navigateToSmokeSession(mobileSmokePage, sessionId);

  const input = mobileSmokePage.getByTestId("chat-input");
  await expect(input).toBeVisible();
  await expect(mobileSmokePage.getByTestId("mobile-voice-input")).toBeVisible();
  await expect(mobileSmokePage.getByTestId("chat-send")).toBeVisible();
  await expect(mobileSmokePage.getByTestId("chat-lead")).toHaveCount(0);
  await expect(mobileSmokePage.getByTestId("chat-add-menu")).toHaveCount(0);

  // Still three with a draft in the field.
  await input.fill("hello");
  await expect(mobileSmokePage.getByTestId("chat-lead")).toHaveCount(0);
  await expect(mobileSmokePage.getByTestId("chat-send")).toBeVisible();

  // Escape collapses the bar in one press; the draft survives.
  await input.press("Escape");
  await expect(mobileSmokePage.getByTestId("chat-box")).toHaveCount(0);
  await mobileSmokePage.getByTestId("terminal-chat-toggle").click();
  await expect(mobileSmokePage.getByTestId("chat-input")).toHaveValue("hello");
});

// voice-input.css carried three byte-identical copies of the FAB container, the
// M3 state layer and the Material-Symbols glyph. They are one selector list
// each now, so the merge is pinned through computed style: a typo in any list
// changes a size, a corner or an elevation.
test("composer controls and corner FABs share one M3 anatomy", async ({ mobileSmokePage, stack }) => {
  const sessionId = (await spawnSmokeShell(mobileSmokePage, stack.workerFp)).session_id;
  await navigateToSmokeSession(mobileSmokePage, sessionId);

  const anatomy = (testId: string) => mobileSmokePage.getByTestId(testId).evaluate((el) => {
    const style = getComputedStyle(el);
    return {
      width: style.width,
      height: style.height,
      borderRadius: style.borderRadius,
      boxShadow: style.boxShadow,
    };
  });

  await expect(mobileSmokePage.getByTestId("chat-box")).toBeVisible();
  const [barSend, barMic] = await Promise.all([
    anatomy("chat-send"),
    anatomy("voice-mic"),
  ]);
  for (const [label, control] of [["chat-send", barSend], ["voice-mic", barMic]] as const) {
    expect(control.width, label).toBe("44px");
    expect(control.height, label).toBe("44px");
    expect(control.borderRadius, label).toBe(barSend.borderRadius);
  }
  // The in-bar tier carries the state layer but never an elevation.
  expect(barSend.boxShadow).toBe("none");
  expect(barMic.boxShadow).toBe("none");

  // Collapsed, the same anatomy resolves to the 56dp corner tier.
  await mobileSmokePage.getByTestId(`terminal-slot-${sessionId}`).click({ position: { x: 8, y: 8 } });
  await expect(mobileSmokePage.getByTestId("chat-box")).toHaveCount(0);
  const [chatToggle, cornerMic] = await Promise.all([
    anatomy("terminal-chat-toggle"),
    anatomy("voice-mic"),
  ]);
  for (const [label, fab] of [["terminal-chat-toggle", chatToggle], ["voice-mic", cornerMic]] as const) {
    expect(fab.width, label).toBe("56px");
    expect(fab.height, label).toBe("56px");
  }
  expect(chatToggle.borderRadius).toBe(cornerMic.borderRadius);
  expect(chatToggle.boxShadow).toBe(cornerMic.boxShadow);
});

// The mobile terminal floats exactly three controls: record, message, keyboard.
// The paperclip and agent-launch FABs were deleted; nothing may reintroduce a
// button into this corner without failing here.
test("mobile terminal floats only the mic, message and keyboard FABs", async ({ mobileSmokePage, stack }) => {
  const sessionId = (await spawnSmokeShell(mobileSmokePage, stack.workerFp)).session_id;
  await navigateToSmokeSession(mobileSmokePage, sessionId);

  // Collapse the auto-opened composer so the corner stack is the visible state.
  await mobileSmokePage.getByTestId(`terminal-slot-${sessionId}`).click({ position: { x: 8, y: 8 } });
  await expect(mobileSmokePage.getByTestId("chat-box")).toHaveCount(0);

  await expect(mobileSmokePage.getByTestId("voice-mic")).toBeVisible();
  await expect(mobileSmokePage.getByTestId("terminal-chat-toggle")).toBeVisible();
  await expect(mobileSmokePage.getByTestId("terminal-nav-toggle")).toBeVisible();
  await expect(mobileSmokePage.getByTestId("attach-file")).toHaveCount(0);
  await expect(mobileSmokePage.getByTestId("agent-launch")).toHaveCount(0);
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

  const voice = mobileSmokePage.getByTestId("mobile-voice-input");
  await expect(voice).toHaveAttribute("data-engine", "deepgram");
  const mic = mobileSmokePage.getByTestId("voice-mic");
  await mic.tap();

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
  await mic.tap();
  await expect(voice).toHaveAttribute("data-state", "idle", { timeout: 15_000 });
  await expect(input).toHaveValue(/hello from the mic/);

  // Longer than the 4 s mobile idle release: the device really closes, so the
  // next tap re-opens it cold and re-rolls exactly the dice the phone re-rolls.
  await mobileSmokePage.waitForTimeout(4_500);

  await mic.tap();
  await expect(voice).toHaveAttribute("data-state", "listening");
  await mic.tap();
  await expect(voice).toHaveAttribute("data-state", "idle", { timeout: 15_000 });
  await expect(input).toHaveValue(/hello from the mic.*hello from the mic/);
  // No error caption survived either recording.
  await expect(mobileSmokePage.getByTestId("voice-caption")).toHaveCount(0);
});

// The Chromium harness cannot reproduce WebKit's never-settling promise on its
// own (the case above passes on the unfixed code for exactly that reason), so
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
  const input = mobileSmokePage.getByTestId("chat-input");
  await expect(input).toBeVisible();
  await input.fill("half typed message");

  // Tapping outside the bar keeps it too.
  await mobileSmokePage.getByTestId(`terminal-slot-${firstId}`).click({ position: { x: 8, y: 8 } });
  await expect(mobileSmokePage.getByTestId("chat-box")).toHaveCount(0);
  await mobileSmokePage.getByTestId("terminal-chat-toggle").click();
  await expect(mobileSmokePage.getByTestId("chat-input")).toHaveValue("half typed message");

  // Escape collapses the bar; the draft is retained.
  await mobileSmokePage.getByTestId("chat-input").press("Escape");
  await expect(mobileSmokePage.getByTestId("chat-box")).toHaveCount(0);
  await mobileSmokePage.getByTestId("terminal-chat-toggle").click();
  await expect(mobileSmokePage.getByTestId("chat-input")).toHaveValue("half typed message");

  // A different session gets its own empty draft; coming back restores this one.
  const secondId = (await spawnSmokeShell(mobileSmokePage, stack.workerFp)).session_id;
  await switchToSmokeSession(mobileSmokePage, secondId);
  await openSmokeComposer(mobileSmokePage);
  await expect(mobileSmokePage.getByTestId("chat-input")).toHaveValue("");
  await switchToSmokeSession(mobileSmokePage, firstId);
  await openSmokeComposer(mobileSmokePage);
  await expect(mobileSmokePage.getByTestId("chat-input")).toHaveValue("half typed message");

  // A reload restores it (localStorage, local device only).
  await mobileSmokePage.reload({ waitUntil: "domcontentloaded" });
  await expect(mobileSmokePage.getByTestId(`terminal-slot-${firstId}`)).toBeVisible();
  await openSmokeComposer(mobileSmokePage);
  await expect(mobileSmokePage.getByTestId("chat-input")).toHaveValue("half typed message");

  // Send is the only thing that consumes it.
  await mobileSmokePage.getByTestId("chat-send").click();
  await expect(mobileSmokePage.getByTestId("chat-box")).toHaveCount(0);
  await mobileSmokePage.getByTestId("terminal-chat-toggle").click();
  await expect(mobileSmokePage.getByTestId("chat-input")).toHaveValue("");
});

test("mobile composer preserves input and the Ctrl pad interrupts", async ({ mobileSmokePage, stack }, testInfo) => {
  test.skip(testInfo.project.name !== "webkit-iphone", "iPhone terminal input contract");
  const sessionId = await mobileSmokePage.evaluate(async (workerFp) => {
    const smoke = (window as unknown as Window & { __smoke: { spawnShell(worker: string, folder: string): Promise<{ session_id: string }> } }).__smoke;
    return (await smoke.spawnShell(workerFp, "/tmp")).session_id;
  }, stack.workerFp);
  await mobileSmokePage.goto(`${stack.baseUrl}/s/${sessionId}`);
  const slot = mobileSmokePage.getByTestId(`terminal-slot-${sessionId}`);
  await expect(slot).toBeVisible();

  await expect(mobileSmokePage.getByTestId("mobile-chat-input")).toHaveAttribute("data-open", "true");
  await expect(mobileSmokePage.getByTestId("chat-box")).toBeVisible();
  const initialInput = mobileSmokePage.getByTestId("chat-input");
  await expect(initialInput).toBeVisible();
  await expect(mobileSmokePage.getByTestId("terminal-chat-toggle")).toHaveCount(0);
  await expect(initialInput).toBeFocused();

  await inputSmokeTerminal(
    mobileSmokePage,
    sessionId,
    `IFS= read -r line; printf '<%s>\\n' "$line"\r`,
  );
  await initialInput.fill("  x  ");
  await mobileSmokePage.getByTestId("chat-send").click();
  await expect.poll(() => slot.textContent()).toContain("<  x  >");
  await expect(mobileSmokePage.getByTestId("terminal-chat-toggle")).toBeVisible();
  await expect(mobileSmokePage.getByTestId("chat-box")).toHaveCount(0);

  await slot.click({ position: { x: 8, y: 8 } });
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

  // Reopen: three controls, and typing never adds a fourth.
  await mobileSmokePage.getByTestId("terminal-chat-toggle").click();
  await expect(mobileSmokePage.getByTestId("chat-lead")).toHaveCount(0);
  await mobileSmokePage.getByTestId("chat-input").fill("y");
  await expect(mobileSmokePage.getByTestId("chat-lead")).toHaveCount(0);
  await expect(mobileSmokePage.getByTestId("chat-send")).toBeVisible();
  // Tapping outside collapses the bar without discarding: reopening shows "y".
  await slot.click({ position: { x: 8, y: 8 } });
  await expect(mobileSmokePage.getByTestId("chat-box")).toHaveCount(0);
  await mobileSmokePage.getByTestId("terminal-chat-toggle").click();
  await expect(mobileSmokePage.getByTestId("chat-input")).toHaveValue("y");
  // Tapping outside keeps it too.
  await slot.click({ position: { x: 8, y: 8 } });
  await expect(mobileSmokePage.getByTestId("chat-box")).toHaveCount(0);
  await mobileSmokePage.getByTestId("terminal-chat-toggle").click();
  await expect(mobileSmokePage.getByTestId("chat-input")).toHaveValue("y");
  // Leave the next block its empty-draft, closed-bar precondition.
  await mobileSmokePage.getByTestId("chat-input").fill("");
  await mobileSmokePage.keyboard.press("Escape");
  await expect(mobileSmokePage.getByTestId("chat-box")).toHaveCount(0);

  // Enter is a NEWLINE, never a submit: only the send button commits the draft.
  await mobileSmokePage.getByTestId("terminal-chat-toggle").click();
  const composed = mobileSmokePage.getByTestId("chat-input");
  await composed.fill("printf 'ENTER_LINE_A\\n'");
  await composed.press("Enter");
  await mobileSmokePage.keyboard.type("printf 'ENTER_LINE_B\\n'");
  // Enter grew the draft instead of submitting: two lines, composer still open.
  await expect(composed).toHaveValue("printf 'ENTER_LINE_A\\n'\nprintf 'ENTER_LINE_B\\n'");
  await expect(mobileSmokePage.getByTestId("chat-box")).toHaveCount(1);
  await mobileSmokePage.getByTestId("chat-send").click();
  await expect.poll(() => slot.textContent()).toContain("ENTER_LINE_A");
  await expect.poll(() => slot.textContent()).toContain("ENTER_LINE_B");

  await inputSmokeTerminal(
    mobileSmokePage,
    sessionId,
    `IFS= read -r line; printf '<%s>\\n' "$line"\r`,
  );
  await mobileSmokePage.getByTestId("terminal-chat-toggle").click();
  await mobileSmokePage.getByTestId("chat-send").click();
  await expect.poll(() => slot.textContent()).toContain("<>");

  await inputSmokeTerminal(mobileSmokePage, sessionId, "sleep 30\r");
  await mobileSmokePage.getByTestId("terminal-nav-toggle").click();
  await expect(mobileSmokePage.getByTestId("nav-tab")).toBeVisible();
  await expect(mobileSmokePage.getByTestId("nav-home")).toBeVisible();
  await expect(mobileSmokePage.getByTestId("nav-end")).toBeVisible();
  const ctrl = mobileSmokePage.getByTestId("nav-ctrl");
  await expect(ctrl).toHaveAttribute("aria-pressed", "false");
  await ctrl.click();
  await expect(ctrl).toHaveAttribute("aria-pressed", "true");
  await mobileSmokePage.getByTestId("terminal-nav-toggle").click();
  await mobileSmokePage.getByTestId("terminal-nav-toggle").click();
  await expect(mobileSmokePage.getByTestId("nav-ctrl")).toHaveAttribute("aria-pressed", "false");
  await mobileSmokePage.getByTestId("nav-ctrl").click();
  await expect.poll(() => mobileSmokePage.evaluate((id) => {
    const smoke = (window as unknown as Window & {
      __smoke: { paneFocused(sessionId: string): { focused: boolean } };
    }).__smoke;
    return smoke.paneFocused(id).focused;
  }, sessionId)).toBe(false);
  await slot.click({ position: { x: 8, y: 8 } });
  await expect.poll(() => mobileSmokePage.evaluate((id) => {
    const smoke = (window as unknown as Window & {
      __smoke: { paneFocused(sessionId: string): { focused: boolean } };
    }).__smoke;
    return smoke.paneFocused(id).focused;
  }, sessionId)).toBe(true);
  await mobileSmokePage.keyboard.type("c");
  const marker = `TOUCH_CTRL_${crypto.randomUUID().replace(/-/g, "").slice(0, 8)}`;
  await mobileSmokePage.keyboard.type(`printf '%s\\n' ${marker}`);
  await mobileSmokePage.keyboard.press("Enter");
  await expect.poll(() => slot.textContent()).toContain(marker);

  const secondSessionId = (await spawnSmokeShell(mobileSmokePage, stack.workerFp)).session_id;
  await switchToSmokeSession(mobileSmokePage, secondSessionId);
  await expect(mobileSmokePage.getByTestId("mobile-chat-input")).toHaveAttribute("data-open", "true");
  await expect(mobileSmokePage.getByTestId("chat-input")).toBeFocused();
  await mobileSmokePage.getByTestId("chat-input").press("Escape");
  await expect(mobileSmokePage.getByTestId("terminal-chat-toggle")).toBeVisible();
  await expect(mobileSmokePage.getByTestId("chat-box")).toHaveCount(0);
  await switchToSmokeSession(mobileSmokePage, sessionId);
  await expect(mobileSmokePage.getByTestId("chat-box")).toHaveCount(0);
  await switchToSmokeSession(mobileSmokePage, secondSessionId);
  await expect(mobileSmokePage.getByTestId("terminal-chat-toggle")).toBeVisible();
  await expect(mobileSmokePage.getByTestId("chat-box")).toHaveCount(0);
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

// Returning to an already-open pane must be INSTANT: no Sync-socket re-dial and
// no claim snapshot. Both legs assert the absence of work, which is why they
// read syncWsGeneration() (socket dial count) and cellFullFrameCount() (worker
// claim snapshots) rather than timing anything.
test("returning to a streaming pane costs no re-dial and no snapshot", async ({ smokePage, stack }, testInfo) => {
  test.skip(testInfo.project.name !== "chromium-desktop", "desktop deck/visibility contract");
  const spawn = (folder: string) => smokePage.evaluate(async ({ workerFp, dir }) => {
    const smoke = (window as unknown as Window & {
      __smoke: { spawnShell(worker: string, folder: string): Promise<{ session_id: string }> };
    }).__smoke;
    return (await smoke.spawnShell(workerFp, dir)).session_id;
  }, { workerFp: stack.workerFp, dir: folder });

  const sessionA = await spawn("/tmp");
  // Probe every quantity in ONE evaluate so a reveal can be inspected without
  // polling — polling would hide the very round trip under test.
  const probe = () => smokePage.evaluate((id) => {
    const smoke = (window as unknown as Window & {
      __smoke: {
        renderProbe(sessionId: string): { atBottom: boolean };
        markerScan(sessionId: string, prefix: string): { max: number; duplicated: number[]; outOfOrder: number };
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
  // Let the burst quiesce: a frame in flight during a claim legitimately costs
  // one redundant snapshot, and this test asserts an exact count.
  await smokePage.waitForTimeout(1000);

  // ── leg 1: deck tab switch away and back ──────────────────────────────────
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
  await smokePage.waitForTimeout(1000); // A parks into the background-stream set
  await smokePage.evaluate((id) => {
    const smoke = (window as unknown as Window & { __smoke: { navigate(href: string): void } }).__smoke;
    smoke.navigate(`/s/${id}`);
  }, sessionA);
  await expect(smokePage.getByTestId(`tab-${sessionA}`)).toHaveAttribute("data-active", "true");

  // A never stopped streaming, so its reveal claim carries the worker's own last
  // emitted seq: the bottom is already painted and no full frame is emitted.
  const afterSwitch = await probe();
  expect(afterSwitch).toMatchObject({
    atBottom: true,
    markerMax: 8000,
    duplicated: [],
    outOfOrder: 0,
    fullFrames: beforeSwitch.fullFrames,
    wsGeneration: beforeSwitch.wsGeneration,
  });

  // The deterministic hidden pin dispatches visibilitychange without asking
  // Chromium to background this test page, so lifecycle handlers run while
  // assertions remain schedulable.
  const beforeHide = await probe();
  await smokePage.evaluate(() => {
    const smoke = (window as unknown as Window & { __smoke: { forceHidden(on: boolean): void } }).__smoke;
    smoke.forceHidden(true);
  });
  await smokePage.evaluate(async (id) => {
    const smoke = (window as unknown as Window & { __smoke: { input(sessionId: string, text: string): Promise<void> } }).__smoke;
    await smoke.input(id, "for i in $(seq 8001 8200); do echo CELLLINE-$i; sleep 0.01; done\r");
  }, sessionA);
  // The hidden pane keeps its 0×0 BACKGROUND claim, so deltas keep painting
  // while the tab is away — wait for the LAST line before returning so the
  // reveal claim can't race a frame in flight.
  await expect.poll(() => smokePage.evaluate((id) => {
    const smoke = (window as unknown as Window & { __smoke: { markerScan(sessionId: string, prefix: string): { max: number } } }).__smoke;
    return smoke.markerScan(id, "CELLLINE-").max;
  }, sessionA), { timeout: 60_000, intervals: [250] }).toBe(8200);
  await smokePage.waitForTimeout(500);

  await smokePage.evaluate(() => {
    const smoke = (window as unknown as Window & { __smoke: { forceHidden(on: boolean): void } }).__smoke;
    smoke.forceHidden(false);
  });
  const afterShow = await probe();
  expect(afterShow).toMatchObject({
    atBottom: true,
    markerMax: 8200,
    duplicated: [],
    outOfOrder: 0,
    fullFrames: beforeHide.fullFrames,   // the pane never fell behind
    wsGeneration: beforeHide.wsGeneration, // the Sync socket survived the hide
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

    // HIDDEN_STREAM_KEEP_MS (60s) + VIEWER_WITHDRAW_GRACE_MS (800ms).
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
