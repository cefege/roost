import { test, expect } from "./fixtures.ts";
import type { RecoverySmokeApi } from "./terminal-smoke-api.ts";
import {
  encodePtyFixtureCommand,
  PTY_FIXTURE_READY,
} from "./pty-fixture-protocol.ts";
import {
  spawnPtyFixtureSession,
  navigateToSmokeSession,
  waitForStableCellFrames,
} from "./terminal-helpers.ts";
import {
  coordinatorTerminalViewState,
  readTerminalStreamProbe,
} from "./terminal-probe-helpers.ts";

test("a document-return repair waits for a synchronized TUI repaint boundary", async ({
  smokePage,
  stack,
}, testInfo) => {
  test.skip(!testInfo.project.name.startsWith("chromium"), "desktop synchronized-output contract");
  test.setTimeout(90_000);

  const suffix = crypto.randomUUID().replaceAll("-", "").slice(0, 12);
  const staticMarker = `SYNC-STATIC-${suffix}`;
  const dynamicMarker = `SYNC-DYNAMIC-${suffix}`;
  const fixtureWorker = await stack.startPtyFixtureWorker();
  const sessionId = await spawnPtyFixtureSession(smokePage, fixtureWorker);
  await navigateToSmokeSession(smokePage, sessionId);
  await smokePage.evaluate(async ({ id, marker }) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return smokeWindow.__smoke.waitForPaintedMarker(id, marker, 30_000);
  }, { id: sessionId, marker: PTY_FIXTURE_READY });
  const readCounters = () => smokePage.evaluate((id) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    const smoke = smokeWindow.__smoke;
    return {
      frames: smoke.cellFrameCount(id),
      fullFrames: smoke.cellFullFrameCount(id),
      backfills: smoke.scrollbackBackfillRequestCount(id),
      lastFullFrameSbRows: smoke.lastFullFrameSbRows(id),
    };
  }, sessionId);
  const workerRawHead = async (): Promise<number> => {
    const probe = await readTerminalStreamProbe(smokePage, sessionId);
    const raw = probe.worker.session?.raw;
    if (raw === null || typeof raw !== "object" || !("head_seq" in raw)) return -1;
    const headSeq = raw.head_seq;
    return typeof headSeq === "number" || typeof headSeq === "string"
      ? Number(headSeq)
      : -1;
  };
  await expect.poll(async () => {
    const counters = await readCounters();
    return counters.fullFrames;
  }).toBeGreaterThan(0);
  await waitForStableCellFrames(smokePage, sessionId);
  const activeBeforeSyncHide = await readTerminalStreamProbe(smokePage, sessionId);
  const beforeSyncRevision = activeBeforeSyncHide.browser.view.revision;
  if (!beforeSyncRevision || !activeBeforeSyncHide.browser.view.stream_id) {
    throw new Error("synchronized-output fixture omitted its active terminal view");
  }

  await smokePage.evaluate(() => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    smokeWindow.__smoke.forceHidden(true);
  });
  await expect.poll(async () => {
    const probe = await readTerminalStreamProbe(smokePage, sessionId);
    const coordinator = coordinatorTerminalViewState(probe);
    return {
      status: probe.browser.view.status,
      active: probe.browser.view.active,
      revisionAdvanced: probe.browser.view.revision !== null
        && BigInt(probe.browser.view.revision) > BigInt(beforeSyncRevision),
      coordinatorViews: coordinator?.activeViews ?? -1,
      coordinatorGeometry: coordinator?.effective,
    };
  }, { timeout: 15_000, intervals: [50, 100, 250] }).toEqual({
    status: "accepted",
    active: false,
    revisionAdvanced: true,
    coordinatorViews: 0,
    coordinatorGeometry: null,
  });
  const hiddenSyncProbe = await readTerminalStreamProbe(smokePage, sessionId);
  const hiddenSyncRevision = hiddenSyncProbe.browser.view.revision;
  if (!hiddenSyncRevision) throw new Error("synchronized-output hide omitted its inactive revision");

  const hiddenRaw = await workerRawHead();
  const before = await readCounters();
  const firstHalf = encodePtyFixtureCommand({
    op: "EMIT",
    newline: false,
    text: `\x1b[?2026h\x1b[2J\x1b[H${staticMarker}`,
  });
  await smokePage.evaluate(async ({ id, command }) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    await smokeWindow.__smoke.input(id, command);
  }, { id: sessionId, command: firstHalf });
  await expect.poll(workerRawHead, {
    timeout: 15_000,
    intervals: [25, 50, 100],
  }).toBeGreaterThan(hiddenRaw);
  expect(await readCounters()).toEqual(before);

  await smokePage.evaluate(() => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    smokeWindow.__smoke.forceHidden(false);
  });
  await expect.poll(async () => {
    const probe = await readTerminalStreamProbe(smokePage, sessionId);
    const { view, replica } = probe.browser;
    const coordinator = coordinatorTerminalViewState(probe);
    const syncOutput = probe.worker.session?.sync_output;
    const counters = await readCounters();
    return {
      status: view.status,
      active: view.active,
      revisionAdvanced: view.revision !== null
        && BigInt(view.revision) > BigInt(hiddenSyncRevision),
      newStream: view.stream_id !== null
        && view.stream_id !== ""
        && view.stream_id !== activeBeforeSyncHide.browser.view.stream_id,
      baselineReady: replica.baseline_ready,
      replicaStreamMatched: replica.expected_stream_id === view.stream_id,
      coordinatorViews: coordinator?.activeViews ?? 0,
      coordinatorStreamMatched: coordinator?.streamId === view.stream_id,
      coordinatorGeometryMatched: coordinator?.effective?.cols === view.effective_cols
        && coordinator.effective?.rows === view.effective_rows,
      syncHeld: syncOutput !== null
        && typeof syncOutput === "object"
        && "tripped" in syncOutput
        && syncOutput.tripped === false,
      frames: counters.frames,
      fullFrames: counters.fullFrames,
    };
  }, { timeout: 15_000, intervals: [10, 25, 50] }).toEqual({
    status: "accepted",
    active: true,
    revisionAdvanced: true,
    newStream: true,
    baselineReady: false,
    replicaStreamMatched: true,
    coordinatorViews: 1,
    coordinatorStreamMatched: true,
    coordinatorGeometryMatched: true,
    syncHeld: true,
    frames: before.frames,
    fullFrames: before.fullFrames,
  });

  const secondHalf = encodePtyFixtureCommand({
    op: "EMIT",
    newline: false,
    text: `\x1b[2;1H${dynamicMarker}\x1b[?2026l`,
  });
  await smokePage.evaluate(async ({ id, command }) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    await smokeWindow.__smoke.input(id, command);
  }, { id: sessionId, command: secondHalf });
  await expect.poll(readCounters, { timeout: 15_000, intervals: [25, 50, 100] }).toMatchObject({
    frames: before.frames + 1,
    fullFrames: before.fullFrames + 1,
  });

  const staticPaint = await smokePage.evaluate(async ({ id, marker }) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return smokeWindow.__smoke.waitForPaintedMarker(id, marker, 15_000);
  }, { id: sessionId, marker: staticMarker });
  const dynamicPaint = await smokePage.evaluate(async ({ id, marker }) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return smokeWindow.__smoke.waitForPaintedMarker(id, marker, 15_000);
  }, { id: sessionId, marker: dynamicMarker });
  expect(staticPaint).toMatchObject({ proof_kind: "marker", marker: staticMarker, frames: 2 });
  expect(dynamicPaint).toMatchObject({ proof_kind: "marker", marker: dynamicMarker, frames: 2 });

  const after = await readCounters();
  expect(after).toMatchObject({
    frames: before.frames + 1,
    fullFrames: before.fullFrames + 1,
    backfills: before.backfills,
    lastFullFrameSbRows: 0,
  });
  const afterProbe = await readTerminalStreamProbe(smokePage, sessionId);
  expect(afterProbe.worker.session?.sync_output ?? null).toBeNull();
  expect(afterProbe.browser.view).toMatchObject({
    status: "accepted",
    active: true,
  });
  expect(afterProbe.browser.replica).toMatchObject({
    expected_stream_id: afterProbe.browser.view.stream_id,
    baseline_ready: true,
    resync_latched: false,
  });
  expect(afterProbe.browser.handler_canonical).toEqual(afterProbe.browser.dom_reconciled);
});
