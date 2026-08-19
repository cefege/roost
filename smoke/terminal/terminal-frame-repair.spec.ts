import { test, expect } from "./fixtures.ts";
import type { RecoverySmokeApi } from "./terminal-smoke-api.ts";
import {
  spawnSmokeShell,
  navigateToSmokeSession,
  switchToSmokeSession,
  waitForStableCellFrames,
  setRecoveryCanary,
  recoveryProbe,
  expectCleanRecovery,
} from "./terminal-helpers.ts";
import { holdNativeTerminalSelection } from "./terminal-paint-helpers.ts";
import { readTerminalStreamProbe } from "./terminal-probe-helpers.ts";

test("dropped initial full frame reclaims immediately on the first delta", async ({ smokePage, stack }, testInfo) => {
  test.skip(!testInfo.project.name.startsWith("chromium"), "desktop cell recovery contract");
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
  test.skip(!testInfo.project.name.startsWith("chromium"), "desktop cell recovery contract");
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
  test.skip(!testInfo.project.name.startsWith("chromium"), "desktop cell recovery contract");
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
    const parts = smokeWindow.__smoke.viewportText(id).split("HEARTBEAT-READY-001");
    // PS1-agnostic: viewportText concatenates rows (and trailing composer UI
    // text), so require the row right after the marker to be a prompt token
    // ending in $ or #. Hardcoding bash-<version>$ broke on CI runners whose
    // profile sets PS1 to user@host:cwd$.
    return /^\S{0,120}[$#](?:\s|$)/.test(parts.length > 1 ? (parts.at(-1) ?? "") : "");
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
  test.skip(!testInfo.project.name.startsWith("chromium"), "desktop paint-hold contract");
  const sessionId = (await spawnSmokeShell(smokePage, stack.workerFp)).session_id;
  await navigateToSmokeSession(smokePage, sessionId);
  await expect.poll(() => smokePage.evaluate(
    (id) => (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.cellFullFrameCount(id),
    sessionId,
  )).toBeGreaterThan(0);
  const canary = `selection-hold-${sessionId}`;
  await setRecoveryCanary(smokePage, canary);
  const selected = await holdNativeTerminalSelection(smokePage, sessionId);
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
  // Report the layer that stalled instead of just "text missing": wire vs
  // canonical vs DOM plus the reason the renderer refused to reconcile.
  await expect.poll(async () => {
    const text = await smokePage.evaluate(
      (id) => (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.viewportText(id),
      sessionId,
    );
    if (text.includes("HOLD-RECOVER-001")) return "painted";
    const probe = await readTerminalStreamProbe(smokePage, sessionId);
    return {
      text,
      wire: probe.browser.wire_received,
      canonical: probe.browser.handler_canonical,
      dom: probe.browser.dom_reconciled,
      blocked: probe.browser.reconcile_block_reason,
    };
  }, { timeout: 10_000 }).toBe("painted");
  expect(await smokePage.evaluate(() => window.getSelection()?.isCollapsed ?? true)).toBe(true);
  expectCleanRecovery(await recoveryProbe(smokePage, sessionId, "HOLD-RECOVER-"), canary, 1, 1);
});
