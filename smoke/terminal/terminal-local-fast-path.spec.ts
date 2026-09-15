// Real-flow proof of the local terminal fast path: a page served by the worker
// that owns the PTY drives it over that worker's loopback socket, the
// coordinator only mirrors it for other machines, and an open local pane keeps
// painting through a coordinator outage. The fixture worker's coordinator link
// carries a 200 ms one-way delay, which is what makes "who is in this pane's
// path" measurable rather than merely asserted.

import { writeFileSync } from "node:fs";
import { test, expect } from "./fixtures.ts";
import { PTY_FIXTURE_READY } from "./pty-fixture-protocol.ts";
import { LOCAL_TERMINAL_PROCESS_EPOCH } from "../../apps/web/src/store/terminal-stream-transport.ts";
import { startTerminalTestStack } from "./stack.ts";
import {
  expectCleanRecovery,
  navigateToSmokeSession,
  recoveryProbe,
  setRecoveryCanary,
  spawnPtyFixtureSession,
} from "./terminal-helpers.ts";
import {
  type EnrolledPage,
  armPaintedMarkerEpoch,
  attachStackLogs,
  emitMarkerFromPage,
  openEnrolledPage,
  readArmedPaintedMarkerEpoch,
  readLocalTransportReading,
  waitForKeeperChannels,
  waitForKeeperRowAfter,
  workerStreamDeliveries,
} from "./terminal-local-fast-path-helpers.ts";
import {
  expectMarkersOnce,
  forceVisible,
  waitForPainted,
  waitForTransition,
} from "./terminal-multiview-helpers.ts";
import {
  coordinatorTerminalViewState,
  readTerminalStreamProbe,
} from "./terminal-probe-helpers.ts";
import { coordinatorConstrainedGeometry } from "./terminal-probe-viewer-inputs.ts";

// The local page starts wider than the coordinator-origin page, so the remote
// view constrains both axes first and the narrowing below is what hands the
// column constraint to the local one. Both viewports are pinned rather than
// inherited: the aggregate under test is a function of them.
const LOCAL_VIEWPORT = { width: 1_600, height: 900 };
const REMOTE_VIEWPORT = { width: 1_280, height: 720 };
const LOCAL_NARROW_VIEWPORT = { width: 760, height: 900 };

const INJECTED_ONE_WAY_DELAY_MS = 200;
// A keystroke that never leaves this machine: local socket round trip, fixture
// echo, one render, and the paint proof's own two animation frames.
const LOCAL_PAINT_BUDGET_MS = 150;
// A coordinator-origin keystroke crosses the delayed link twice, so its own
// round trip cannot beat two one-way delays.
const COORD_ROUND_TRIP_FLOOR_MS = 2 * INJECTED_ONE_WAY_DELAY_MS;
// Separation between the two origins painting ONE locally typed marker. The
// remote copy waits out a single worker→coordinator hop; the floor keeps most
// of that hop while tolerating loaded-runner jitter on either measurement.
const TRANSPORT_SEPARATION_FLOOR_MS = 150;
const OUTAGE_PAINT_BUDGET_MS = 2_000;
const COORD_RECOVERY_TIMEOUT_MS = 120_000;

// This case owns its stack instead of taking the worker-scoped fixture. The
// fixture worker's link options are fixed at first start
// (stack-fixture-worker.ts memoizes them), and the other @serial files — which
// share one stack under `--workers=1` — already claim both fixture slots with
// no delay and with 25 ms. A private stack is also the only way a spec may
// stop the coordinator without disturbing whatever runs next in that worker.
test("worker-served page owns its local PTYs while the coordinator only mirrors them @serial", async ({
  browser,
}, testInfo) => {
  test.skip(!testInfo.project.name.startsWith("chromium"), "desktop local terminal fast path");
  test.setTimeout(420_000);

  const stack = await startTerminalTestStack();
  const measured: Record<string, number> = {};
  let local: EnrolledPage | undefined;
  let remote: EnrolledPage | undefined;
  try {
    const fixtureWorker = await stack.startPtyFixtureWorker({
      workerLinkOneWayDelayMs: INJECTED_ONE_WAY_DELAY_MS,
    });
    expect(stack.ptyFixtureWorkerLink).not.toBeNull();
    remote = await openEnrolledPage(browser, stack, stack.baseUrl);
    local = await openEnrolledPage(browser, stack, stack.localUiUrl(fixtureWorker.workerFp));
    const localPage = local.page;
    const remotePage = remote.page;
    const pages = [localPage, remotePage] as const;
    const suffix = crypto.randomUUID().replaceAll("-", "").slice(0, 8).toUpperCase();
    const prefix = `LFP-${suffix}-`;
    const canary = `LFP-CANARY-${suffix}`;
    expect(local.origin).not.toBe(stack.baseUrl);
    // This document really came off the worker's own door: only that door
    // answers a bootstrap, and only a bootstrap arms the fast path at all.
    const served = await localPage.evaluate(async () => {
      const response = await fetch("/api/local-bootstrap");
      return (await response.json() as { workerFingerprint: string }).workerFingerprint;
    });
    expect(served).toBe(fixtureWorker.workerFp);
    await localPage.setViewportSize(LOCAL_VIEWPORT);
    await remotePage.setViewportSize(REMOTE_VIEWPORT);
    await Promise.all([forceVisible(localPage, true), forceVisible(remotePage, true)]);

    // ── the local socket, not Sync, carries this pane ──────────────────────
    const sessionId = await spawnPtyFixtureSession(localPage, fixtureWorker);
    await navigateToSmokeSession(localPage, sessionId);
    await waitForPainted(localPage, sessionId, PTY_FIXTURE_READY);

    // Both the view decision and every accepted cell frame pass the SAME
    // generation fence (store/terminal-stream-view-commands.ts and
    // terminal-stream-repair.ts both test session.generation), so a local
    // process epoch on the accepted frame means neither could have come from
    // the coordinator's Sync tube.
    await expect.poll(
      () => readLocalTransportReading(localPage, sessionId),
      { timeout: 60_000, intervals: [100, 250, 500] },
    ).toMatchObject({
      acceptedFrameEpoch: LOCAL_TERMINAL_PROCESS_EPOCH,
      viewStatus: "accepted",
      baselineReady: true,
    });
    const reading = await readLocalTransportReading(localPage, sessionId);
    const localStreamId = reading.viewStreamId;
    expect(localStreamId).toEqual(reading.wireStreamId);
    expect(localStreamId).toMatch(/^[0-9a-f-]{36}$/);

    // The stream the pane paints from is the worker's own: the worker reports
    // holding it and fanning it to a live local sink of its own, beside the
    // coordinator sink the other machine reads. Nothing the coordinator minted
    // reaches this pane.
    await expect.poll(async () => {
      const probe = await readTerminalStreamProbe(localPage, sessionId);
      const stream = workerStreamDeliveries(probe.worker.session);
      return {
        workerStreamId: stream?.streamId ?? null,
        liveLocalSinks: stream?.deliveries.filter(
          (delivery) => delivery.sinkId.startsWith("local:") && delivery.active,
        ).length ?? 0,
        coordSink: stream?.deliveries.some((delivery) => delivery.sinkId === "coord") ?? false,
      };
    }, { timeout: 60_000, intervals: [100, 250, 500] }).toEqual({
      workerStreamId: localStreamId,
      liveLocalSinks: 1,
      coordSink: true,
    });
    expect(coordinatorTerminalViewState(await readTerminalStreamProbe(localPage, sessionId)))
      .toMatchObject({ streamId: localStreamId, unavailable: false });

    // ── one PTY, two viewers, and the delay that separates them ───────────
    await navigateToSmokeSession(remotePage, sessionId);
    await waitForPainted(remotePage, sessionId, PTY_FIXTURE_READY);
    await Promise.all(pages.map((page) => setRecoveryCanary(page, canary)));
    await waitForTransition(pages, sessionId, { activeIndices: [0, 1], activeViewCount: 2 });

    const localMarker = `${prefix}1`;
    await Promise.all(pages.map((page) => armPaintedMarkerEpoch(page, sessionId, localMarker)));
    const localSentAt = await emitMarkerFromPage(localPage, sessionId, localMarker);
    measured.localTypedLocalPaintMs = await readArmedPaintedMarkerEpoch(localPage) - localSentAt;
    measured.localTypedRemotePaintMs = await readArmedPaintedMarkerEpoch(remotePage) - localSentAt;

    const remoteMarker = `${prefix}2`;
    await Promise.all(pages.map((page) => armPaintedMarkerEpoch(page, sessionId, remoteMarker)));
    const remoteSentAt = await emitMarkerFromPage(remotePage, sessionId, remoteMarker);
    measured.remoteTypedRemotePaintMs = await readArmedPaintedMarkerEpoch(remotePage) - remoteSentAt;
    measured.remoteTypedLocalPaintMs = await readArmedPaintedMarkerEpoch(localPage) - remoteSentAt;

    for (const page of pages) await expectMarkersOnce(page, sessionId, [localMarker, remoteMarker]);
    expect(measured.localTypedLocalPaintMs).toBeLessThan(LOCAL_PAINT_BUDGET_MS);
    expect(measured.localTypedRemotePaintMs - measured.localTypedLocalPaintMs)
      .toBeGreaterThanOrEqual(TRANSPORT_SEPARATION_FLOOR_MS);
    expect(measured.remoteTypedRemotePaintMs).toBeGreaterThan(COORD_ROUND_TRIP_FLOOR_MS);
    expect(measured.remoteTypedLocalPaintMs)
      .toBeLessThan(measured.remoteTypedRemotePaintMs);

    // ── the coordinator goes away; the local pane does not ────────────────
    // The pane holds this worker's only PTY, so the keeper accounting the
    // bounce must not disturb is exactly one channel.
    const keeperBefore = await waitForKeeperChannels(stack, fixtureWorker.workerFp, 1);
    const remoteSyncGeneration = await remotePage.evaluate(() => window.__smoke.syncWsGeneration());
    await stack.stopCoordinator();
    await expect(localPage.getByTestId("connection-banner")).toHaveAttribute(
      "data-banner-reason",
      "coord-unreachable-local-live",
      { timeout: 60_000 },
    );

    const outageMarker = `${prefix}3`;
    await armPaintedMarkerEpoch(localPage, sessionId, outageMarker);
    const outageSentAt = await emitMarkerFromPage(localPage, sessionId, outageMarker);
    measured.outagePaintMs = await readArmedPaintedMarkerEpoch(localPage) - outageSentAt;
    expect(measured.outagePaintMs).toBeLessThan(OUTAGE_PAINT_BUDGET_MS);
    // Sync is provably down while those frames were accepted, so nothing but
    // the worker's own socket could have delivered them.
    expect(await readLocalTransportReading(localPage, sessionId)).toMatchObject({
      acceptedFrameEpoch: LOCAL_TERMINAL_PROCESS_EPOCH,
      viewStatus: "accepted",
      syncReady: false,
    });

    // ── the coordinator returns and the other machine converges ───────────
    await stack.startCoordinator();
    await expect.poll(
      () => remotePage.evaluate(
        ({ id, text }) => window.__smoke.viewportText(id).includes(text),
        { id: sessionId, text: outageMarker },
      ),
      { timeout: COORD_RECOVERY_TIMEOUT_MS, intervals: [250, 500, 1_000] },
    ).toBe(true);
    await waitForPainted(remotePage, sessionId, outageMarker);
    // The canary lives on the document element: a reload would take it, and a
    // reload would also restart the Sync dial counter instead of advancing it.
    expectCleanRecovery(await recoveryProbe(remotePage, sessionId, prefix), canary, 1, 3);
    expect(await remotePage.evaluate(() => window.__smoke.syncWsGeneration()))
      .toBeGreaterThan(remoteSyncGeneration);

    const keeperAfter = await waitForKeeperRowAfter(
      stack,
      fixtureWorker.workerFp,
      keeperBefore.lastSeenMs,
    );
    expect(keeperAfter.runtime.channel_count).toBe(keeperBefore.runtime.channel_count);
    expect(keeperAfter.runtime.keeper_epoch).toBe(keeperBefore.runtime.keeper_epoch);
    expect(keeperAfter.runtime.binding_digest).toBe(keeperBefore.runtime.binding_digest);

    // ── the worker, not the coordinator, aggregates the two viewports ─────
    const wide = await waitForTransition(pages, sessionId, {
      activeIndices: [0, 1],
      activeViewCount: 2,
    });
    await localPage.setViewportSize(LOCAL_NARROW_VIEWPORT);
    const narrowed = await waitForTransition(pages, sessionId, {
      activeIndices: [0, 1],
      activeViewCount: 2,
      previousStreamId: wide.control.streamId,
      geometryPredicate: (geometry) => geometry.cols < wide.control.cols,
    });
    // The aggregate IS the per-axis minimum of the inputs the owning worker
    // published, checked against the one shared SCD implementation.
    expect(coordinatorConstrainedGeometry(narrowed.probes[0]!))
      .toEqual({ cols: narrowed.control.cols, rows: narrowed.control.rows });

    const narrowMarker = `${prefix}4`;
    await Promise.all(pages.map((page) => armPaintedMarkerEpoch(page, sessionId, narrowMarker)));
    await emitMarkerFromPage(localPage, sessionId, narrowMarker);
    await Promise.all(pages.map((page) => readArmedPaintedMarkerEpoch(page)));
    for (const page of pages) {
      expectCleanRecovery(await recoveryProbe(page, sessionId, prefix), canary, 1, 4);
    }
  } finally {
    // A path attachment, not an inline body: the measured separation is the
    // point of this spec, so it must be readable straight from the run output
    // directory rather than only through the HTML report.
    const report = testInfo.outputPath("local-fast-path-latency.json");
    writeFileSync(report, `${JSON.stringify({
      injectedOneWayDelayMs: INJECTED_ONE_WAY_DELAY_MS,
      ...measured,
    }, null, 2)}\n`);
    await testInfo.attach("local-fast-path-latency.json", {
      path: report,
      contentType: "application/json",
    });
    if (testInfo.status !== testInfo.expectedStatus) await attachStackLogs(testInfo, stack);
    await local?.close();
    await remote?.close();
    await stack.stop();
  }
});
