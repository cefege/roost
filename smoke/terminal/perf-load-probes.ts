// These callbacks cover offscreen stream suppression and stalled-browser recovery under PTY load.
// The perf spec registers both in its contention-aware single-file schedule.
// Recovery assertions pin generation rollover, one full repair, and resumed trusted input.

import type { Page, TestInfo } from "@playwright/test";
import type { SmokeApi } from "../../apps/web/src/lib/smoke.ts";
import { test, expect } from "./fixtures.ts";
import { encodePtyFixtureCommand, PTY_FIXTURE_READY } from "./pty-fixture-protocol.ts";
import type { TerminalTestStack } from "./stack.ts";
import {
  navigateAndProve,
  spawnFixtureSession,
  waitForFixtureWorker,
  workerFolder,
} from "./perf-probe-fixture.ts";

type SmokeWindow = Window & {
  readonly __smoke: SmokeApi;
  __stallCanary?: string;
};
declare const window: SmokeWindow;
export async function probeOffscreenLoad(
  { smokePage, stack }: { smokePage: Page; stack: TerminalTestStack },
  testInfo: TestInfo,
): Promise<void> {
  test.skip(!testInfo.project.name.startsWith("chromium"), "desktop offscreen load");
  test.setTimeout(180_000);

  const fixtureWorker = await stack.startPtyFixtureWorker();
  await waitForFixtureWorker(smokePage, fixtureWorker.workerFp);
  const sessionIds = await smokePage.evaluate(async ({ workerFp, folder }) => {
    const smoke = window.__smoke;
    return Promise.all(Array.from({ length: 10 }, async () =>
      (await smoke.spawnShell(workerFp, folder)).session_id));
  }, { workerFp: fixtureWorker.workerFp, folder: workerFolder(fixtureWorker) });

  for (const sessionId of sessionIds) await navigateAndProve(smokePage, sessionId, PTY_FIXTURE_READY);
  const visibleId = sessionIds.at(-1)!;
  await smokePage.waitForTimeout(500);
  const before = await smokePage.evaluate((ids) => {
    const smoke = window.__smoke;
    return Object.fromEntries(ids.map((id) => [id, smoke.cellFrameCount(id)]));
  }, sessionIds);

  const offscreenFloodFrames = sessionIds.map((_, index) =>
    encodePtyFixtureCommand({ op: "FLOOD", prefix: `OFFSCREEN-${index}-`, count: 2_000 }));
  await smokePage.evaluate(async ({ ids, frames }) => {
    const smoke = window.__smoke;
    await Promise.all(ids.map((id, index) => smoke.input(id, frames[index]!)));
  }, { ids: sessionIds, frames: offscreenFloodFrames });
  await smokePage.evaluate(({ id, marker }) => {
    const smoke = window.__smoke;
    return smoke.waitForPaintedMarker(id, marker, 60_000);
  }, { id: visibleId, marker: "OFFSCREEN-9-2000" });
  await smokePage.waitForTimeout(500);

  const after = await smokePage.evaluate((ids) => {
    const smoke = window.__smoke;
    return Object.fromEntries(ids.map((id) => [id, smoke.cellFrameCount(id)]));
  }, sessionIds);
  expect(after[visibleId]! - before[visibleId]!).toBeGreaterThan(0);
  for (const hiddenId of sessionIds.slice(0, -1)) {
    expect(after[hiddenId]! - before[hiddenId]!).toBe(0);
  }
}

export async function probeStalledConsumerRecovery(
  { smokePage, stack }: { smokePage: Page; stack: TerminalTestStack },
  testInfo: TestInfo,
): Promise<void> {
  test.skip(!testInfo.project.name.startsWith("chromium"), "desktop transport recovery");
  test.setTimeout(120_000);

  const fixtureWorker = await stack.startPtyFixtureWorker();
  const sessionId = await spawnFixtureSession(smokePage, fixtureWorker);
  await navigateAndProve(smokePage, sessionId, PTY_FIXTURE_READY);
  const suffix = `${Date.now()}_${Math.floor(Math.random() * 1_000_000)}`;
  const recoveryMarker = `STALL_RECOVER_${suffix}`;
  const before = await smokePage.evaluate(({ id, canary }) => {
    const win = window;
    win.__stallCanary = canary;
    return {
      generation: win.__smoke.syncWsGeneration(),
      fullFrames: win.__smoke.cellFullFrameCount(id),
    };
  }, { id: sessionId, canary: suffix });

  const stalledFrames = encodePtyFixtureCommand({ op: "FLOOD", prefix: "STALLLINE-", count: 4_000 })
    + encodePtyFixtureCommand({ op: "EMIT", text: recoveryMarker, delayMs: 3_500 });
  await smokePage.evaluate(async ({ id, frames }) => {
    const smoke = window.__smoke;
    await smoke.input(id, frames);
    const blockedUntil = performance.now() + 4_500;
    while (performance.now() < blockedUntil) {
      // The flood starts the ACK deadline; the delayed marker lands after the stale socket closes.
    }
  }, { id: sessionId, frames: stalledFrames });

  await expect.poll(() => smokePage.evaluate(() => {
    const smoke = window.__smoke;
    return smoke.syncWsGeneration();
  }), { timeout: 30_000, intervals: [100] }).toBeGreaterThan(before.generation);
  await smokePage.evaluate(({ id, marker }) => {
    const smoke = window.__smoke;
    return smoke.waitForPaintedMarker(id, marker, 30_000);
  }, { id: sessionId, marker: recoveryMarker });

  const recovered = await smokePage.evaluate(({ id, marker, canary }) => {
    const win = window;
    const text = win.__smoke.viewportText(id);
    return {
      canary: win.__stallCanary,
      markerCount: text.split(marker).length - 1,
      fullFrames: win.__smoke.cellFullFrameCount(id),
    };
  }, { id: sessionId, marker: recoveryMarker, canary: suffix });
  expect(recovered.canary).toBe(suffix);
  expect(recovered.markerCount).toBe(1);
  expect(recovered.fullFrames - before.fullFrames).toBe(1);

  await smokePage.getByTestId(`terminal-slot-${sessionId}`).click();
  const keyNonce = `recover-${suffix}`;
  await smokePage.evaluate(async ({ id, frame, marker }) => {
    const smoke = window.__smoke;
    await smoke.input(id, frame);
    await smoke.waitForPaintedMarker(id, marker);
  }, {
    id: sessionId,
    frame: encodePtyFixtureCommand({ op: "ARM_KEY", nonce: keyNonce }),
    marker: `ARMED:${keyNonce}`,
  });
  const timingId = await smokePage.evaluate((id) => {
    const smoke = window.__smoke;
    return smoke.beginTerminalTiming("trusted_key", id);
  }, sessionId);
  await smokePage.keyboard.press("x");
  const keyResult = await smokePage.evaluate(({ timingId, id, marker }) => {
    const smoke = window.__smoke;
    return smoke.finishTerminalTiming(timingId, id, marker);
  }, { timingId, id: sessionId, marker: `ACK:${keyNonce}` });
  expect(keyResult.trustedKey).toBe(true);
}
