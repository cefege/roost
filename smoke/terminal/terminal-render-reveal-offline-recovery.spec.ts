import { test, expect } from "./fixtures.ts";
import type { RecoverySmokeApi } from "./terminal-smoke-api.ts";
import {
  spawnSmokeShell,
  navigateToSmokeSession,
  waitForStableCellFrames,
  setRecoveryCanary,
  recoveryProbe,
} from "./terminal-helpers.ts";

test("offline producer divergence reconnects and repaints without a reload", async ({ smokePage, stack }, testInfo) => {
  test.skip(!testInfo.project.name.startsWith("chromium"), "desktop transport recovery contract");
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
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    const smoke = smokeWindow.__smoke;
    return {
      frames: smoke.cellFrameCount(id),
      fullFrames: smoke.cellFullFrameCount(id),
      gridEpoch: smoke.cellGridEpoch(id),
      historyRequests: smoke.scrollbackBackfillRequestCount(id),
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
      const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
      const smoke = smokeWindow.__smoke;
      return {
        frames: smoke.cellFrameCount(id),
        historyRequests: smoke.scrollbackBackfillRequestCount(id),
        scan: smoke.markerScan(id, "OFFLINE-RECOVER-"),
      };
    }, sessionId);
    // Closing the old socket and the producer write race at a real delivery
    // boundary. Either no marker or one complete ordered prefix may arrive; a
    // partial frame may never duplicate, skip, or reorder cells.
    expect(isolated.frames).toBeGreaterThanOrEqual(before.frames);
    expect(isolated.historyRequests).toBe(before.historyRequests);
    expect(isolated.scan).toMatchObject({
      duplicated: [],
      missing: 0,
      outOfOrder: 0,
    });
    expect(isolated.scan.max).toBeLessThanOrEqual(30);
    expect(isolated.scan.min).toBe(isolated.scan.total === 0 ? 0 : 1);
    expect(isolated.scan.total).toBe(isolated.scan.max);
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

  const retainedRows = await smokePage.evaluate((id) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return smokeWindow.__smoke.lastFullFrameSbRows(id);
  }, sessionId);
  expect(retainedRows).toBe(0);
  const afterReconnect = await smokePage.evaluate((id) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    const smoke = smokeWindow.__smoke;
    return {
      fullFrames: smoke.cellFullFrameCount(id),
      historyRequests: smoke.scrollbackBackfillRequestCount(id),
      wsGeneration: smoke.syncWsGeneration(),
    };
  }, sessionId);
  expect(afterReconnect.fullFrames).toBeGreaterThan(before.fullFrames);
  expect(afterReconnect.historyRequests).toBe(before.historyRequests);
  await smokePage.evaluate((id) => {
    const container = document.querySelector(`[data-testid="terminal-slot-${id}"] .wterm`);
    if (!(container instanceof HTMLElement)) throw new Error("recovered terminal has no scroll container");
    container.scrollTop = 0;
    container.dispatchEvent(new Event("scroll"));
  }, sessionId);
  await smokePage.waitForFunction(({ id, previous }) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    const smoke = smokeWindow.__smoke;
    return smoke.scrollbackBackfillRequestCount(id) > previous
      && smoke.markerScan(id, "OFFLINE-RECOVER-").max === 30;
  }, { id: sessionId, previous: afterReconnect.historyRequests });
  await smokePage.evaluate((id) => {
    const container = document.querySelector(`[data-testid="terminal-slot-${id}"] .wterm`);
    if (!(container instanceof HTMLElement)) throw new Error("recovered terminal has no scroll container");
    container.scrollTop = container.scrollHeight;
    container.dispatchEvent(new Event("scroll"));
  }, sessionId);
  const recovered = await recoveryProbe(smokePage, sessionId, "OFFLINE-RECOVER-");
  expect(recovered.canary).toBe(canary);
  expect(recovered.atBottom).toBe(true);
  expect(recovered.scan.total).toBeGreaterThan(0);
  expect(recovered.scan.unique).toBe(recovered.scan.total);
  expect(recovered.scan.min).toBeGreaterThanOrEqual(1);
  expect(recovered.scan).toMatchObject({
    max: 30,
    duplicated: [],
    missing: 0,
    outOfOrder: 0,
  });
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
