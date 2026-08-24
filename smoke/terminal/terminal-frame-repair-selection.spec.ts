import { test, expect } from "./fixtures.ts";
import type { RecoverySmokeApi } from "./terminal-smoke-api.ts";
import {
  spawnSmokeShell,
  navigateToSmokeSession,
  switchToSmokeSession,
  setRecoveryCanary,
  recoveryProbe,
  expectCleanRecovery,
} from "./terminal-helpers.ts";
import { holdNativeTerminalSelection } from "./terminal-paint-helpers.ts";
import {
  coordinatorTerminalViewState,
  readTerminalStreamProbe,
} from "./terminal-probe-helpers.ts";

// Selection-held parking/reveal contract split out of
// terminal-frame-repair.spec.ts to honor the 400-line file cap.

test("parking a selection-held pane flushes its latest folded frame", async ({ smokePage, stack }, testInfo) => {
  test.skip(!testInfo.project.name.startsWith("chromium"), "desktop paint-hold contract");
  test.setTimeout(90_000);
  const sessionId = (await spawnSmokeShell(smokePage, stack.workerFp)).session_id;
  await navigateToSmokeSession(smokePage, sessionId);
  await expect.poll(() => smokePage.evaluate((id) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return smokeWindow.__smoke.cellFullFrameCount(id);
  }, sessionId)).toBeGreaterThan(0);
  const canary = `selection-hold-${sessionId}`;
  await setRecoveryCanary(smokePage, canary);
  const selected = await holdNativeTerminalSelection(smokePage, sessionId);
  expect(selected).toBe(true);
  const beforeFrames = await smokePage.evaluate((id) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return smokeWindow.__smoke.cellFrameCount(id);
  }, sessionId);
  await smokePage.evaluate(async (id) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    await smokeWindow.__smoke.input(id, "printf 'HOLD-RECOVER-%03d\\n' 1\r");
  }, sessionId);
  await expect.poll(() => smokePage.evaluate((id) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return smokeWindow.__smoke.cellFrameCount(id);
  }, sessionId)).toBeGreaterThan(beforeFrames);
  expect(await smokePage.getByTestId(`terminal-slot-${sessionId}`).textContent())
    .not.toContain("HOLD-RECOVER-001");

  const otherSessionId = (await spawnSmokeShell(smokePage, stack.workerFp)).session_id;
  await switchToSmokeSession(smokePage, otherSessionId);
  expect(await smokePage.evaluate((id) => {
    const pane = document.querySelector(`[data-testid="terminal-slot-${CSS.escape(id)}"]`);
    const container = pane?.querySelector(".wterm") as HTMLElement | null;
    if (!container) return false;
    container.scrollTop = 0;
    container.dispatchEvent(new Event("scroll"));
    return true;
  }, sessionId)).toBe(true);
  await smokePage.evaluate(({ id, marker }) => {
    type ParkingSample = {
      visible: boolean;
      current: boolean;
      top: number;
      height: number;
      client: number;
    };
    const runtime = window as unknown as Window & {
      __parkingSamples: ParkingSample[];
      __parkingSampling: boolean;
    };
    runtime.__parkingSamples = [];
    runtime.__parkingSampling = true;
    const sample = () => {
      if (!runtime.__parkingSampling) return;
      const pane = document.querySelector(`[data-testid="terminal-slot-${CSS.escape(id)}"]`);
      const container = pane?.querySelector(".wterm") as HTMLElement | null;
      if (container) {
        const box = container.getBoundingClientRect();
        runtime.__parkingSamples.push({
          visible: box.width > 0 && box.height > 0,
          current: (container.textContent ?? "").includes(marker),
          top: container.scrollTop,
          height: container.scrollHeight,
          client: container.clientHeight,
        });
      }
      requestAnimationFrame(sample);
    };
    requestAnimationFrame(sample);
  }, { id: sessionId, marker: "HOLD-RECOVER-001" });
  await switchToSmokeSession(smokePage, sessionId);
  await expect.poll(() => smokePage.evaluate((id) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return smokeWindow.__smoke.viewportText(id);
  }, sessionId), { timeout: 10_000, intervals: [50] }).toContain("HOLD-RECOVER-001");
  await smokePage.evaluate(async () => {
    for (let frame = 0; frame < 4; frame++) {
      await new Promise<void>((resolve) => requestAnimationFrame(() => resolve()));
    }
  });
  const parkingSamples = await smokePage.evaluate(() => {
    type ParkingSample = {
      visible: boolean;
      current: boolean;
      top: number;
      height: number;
      client: number;
    };
    const runtime = window as unknown as Window & {
      __parkingSamples: ParkingSample[];
      __parkingSampling: boolean;
    };
    runtime.__parkingSampling = false;
    return runtime.__parkingSamples;
  });
  const authoritativeAt = parkingSamples.findIndex((sample) => sample.visible && sample.current);
  expect(authoritativeAt).toBeGreaterThanOrEqual(0);
  const authoritative = parkingSamples[authoritativeAt]!;
  expect(authoritative.top).toBeGreaterThanOrEqual(authoritative.height - authoritative.client - 2);
  expect(await smokePage.evaluate(() => window.getSelection()?.isCollapsed ?? true)).toBe(true);
  const parkedRecovery = await readTerminalStreamProbe(smokePage, sessionId);
  expect(parkedRecovery.browser.presentation).toMatchObject({
    reader_intent: "live",
    at_bottom: true,
  });
  expect(parkedRecovery.browser.handler_canonical).toEqual(parkedRecovery.browser.dom_reconciled);
  expectCleanRecovery(await recoveryProbe(smokePage, sessionId, "HOLD-RECOVER-"), canary, 1, 1);

  const hiddenSelected = await holdNativeTerminalSelection(smokePage, sessionId);
  expect(hiddenSelected).toBe(true);
  const beforeHiddenFrames = await smokePage.evaluate((id) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return smokeWindow.__smoke.cellFrameCount(id);
  }, sessionId);
  await smokePage.evaluate(async (id) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    await smokeWindow.__smoke.input(id, "printf 'HIDDEN-RECOVER-%03d\\n' 1\r");
  }, sessionId);
  await expect.poll(() => smokePage.evaluate((id) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return smokeWindow.__smoke.cellFrameCount(id);
  }, sessionId)).toBeGreaterThan(beforeHiddenFrames);
  expect(await smokePage.getByTestId(`terminal-slot-${sessionId}`).textContent())
    .not.toContain("HIDDEN-RECOVER-001");
  await smokePage.evaluate((id) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    const smoke = smokeWindow.__smoke;
    smoke.forceHidden(true);
    const pane = document.querySelector(`[data-testid="terminal-slot-${CSS.escape(id)}"]`);
    const container = pane?.querySelector(".wterm") as HTMLElement | null;
    if (!container) throw new Error("hidden terminal container is missing");
    container.scrollTop = 0;
    container.dispatchEvent(new Event("scroll"));
  }, sessionId);
  await smokePage.evaluate(() => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    smokeWindow.__smoke.forceHidden(false);
  });
  await expect.poll(() => smokePage.evaluate((id) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return smokeWindow.__smoke.viewportText(id);
  }, sessionId), { timeout: 10_000, intervals: [50] }).toContain("HIDDEN-RECOVER-001");
  const hiddenRecovery = await readTerminalStreamProbe(smokePage, sessionId);
  expect(hiddenRecovery.browser.presentation).toMatchObject({
    reader_intent: "live",
    at_bottom: true,
  });
  expect(hiddenRecovery.browser.handler_canonical).toEqual(hiddenRecovery.browser.dom_reconciled);

  const beforeIdleFrames = await smokePage.evaluate((id) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return smokeWindow.__smoke.cellFrameCount(id);
  }, sessionId);
  await smokePage.evaluate(async (id) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    await smokeWindow.__smoke.input(
      id,
      "for i in $(seq 1 22); do printf 'IDLE-LIVE-%02d\\n' \"$i\"; sleep 1; done\r",
    );
  }, sessionId);
  await expect.poll(() => smokePage.evaluate(({ id, frames }) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    const smoke = smokeWindow.__smoke;
    return {
      marker: smoke.viewportText(id).includes("IDLE-LIVE-11"),
      framesAdvanced: smoke.cellFrameCount(id) > frames,
    };
  }, { id: sessionId, frames: beforeIdleFrames }), {
    timeout: 20_000,
    intervals: [250],
  }).toEqual({ marker: true, framesAdvanced: true });
  const framesAtEleven = await smokePage.evaluate((id) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return smokeWindow.__smoke.cellFrameCount(id);
  }, sessionId);
  await expect.poll(() => smokePage.evaluate(({ id, frames }) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    const smoke = smokeWindow.__smoke;
    return {
      marker: smoke.viewportText(id).includes("IDLE-LIVE-22"),
      framesAdvanced: smoke.cellFrameCount(id) > frames,
    };
  }, { id: sessionId, frames: framesAtEleven }), {
    timeout: 25_000,
    intervals: [250],
  }).toEqual({ marker: true, framesAdvanced: true });

  const finalProbe = await readTerminalStreamProbe(smokePage, sessionId);
  expect(coordinatorTerminalViewState(finalProbe)?.activeViews).toBe(1);
  expect(finalProbe.browser.presentation).toMatchObject({
    reader_intent: "live",
    at_bottom: true,
  });
  expect(finalProbe.browser.handler_canonical).toEqual(finalProbe.browser.dom_reconciled);
});
