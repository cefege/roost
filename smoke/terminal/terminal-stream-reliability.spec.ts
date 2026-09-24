// Real-stack terminal reliability proofs exercise each replica boundary independently.
// The proxy fault drops only worker→coordinator deltas after a live baseline exists.
// Browser faults preserve real Sync, renderer, and PTY behavior instead of replacing transport.
// Every success path requires an actual visible marker, not only diagnostic convergence.

import { expect, test } from "./fixtures.ts";
import type { CoordWorkerUp } from "../../packages/protocol/src/gen/roost/v1/worker_transport_pb.ts";
import type { TerminalStreamProbe } from "../../apps/web/src/lib/smoke.ts";
import type { RecoverySmokeApi, TerminalIdentityProbeWindow } from "./terminal-smoke-api.ts";
import {
  inputSmokeTerminal,
  navigateToSmokeSession,
  spawnPtyFixtureSession,
  waitForStableCellFrames,
} from "./terminal-helpers.ts";
import {
  encodePtyFixtureCommand,
  PTY_FIXTURE_READY,
} from "./pty-fixture-protocol.ts";
import { readTerminalStreamProbe } from "./terminal-probe-helpers.ts";

type ReliabilityWindow = Omit<Window, "__smoke"> & {
  __smoke: RecoverySmokeApi;
  __terminalIdentityProbe: TerminalIdentityProbeWindow["__terminalIdentityProbe"];
  __terminalMeasurementFault?: { failedMeasures: number; restoredAtMs: number | null };
};

type SourceFaultTarget = { channelId: number; streamId: string };
type SourceFaultState = {
  target: SourceFaultTarget | null;
  droppedDeltas: number;
  droppedCheckpoint: { gridEpoch: string; seq: string } | null;
  repairFullStarts: number;
  repairCheckpoint: { gridEpoch: string; seq: string } | null;
};

declare const window: ReliabilityWindow;

const SOURCE_RECOVERY_TIMEOUT_MS = 6_000;
const DOM_RECOVERY_TIMEOUT_MS = 6_000;
const MEASUREMENT_RECOVERY_TIMEOUT_MS = 1_000;
function record(value: unknown): Record<string, unknown> | null {
  return value !== null && typeof value === "object" && !Array.isArray(value)
    ? value as Record<string, unknown>
    : null;
}

function sourceChannelId(probe: TerminalStreamProbe): number | null {
  const binding = record(probe.worker.session?.channel_binding);
  const channelId = binding?.channel_id;
  return typeof channelId === "number" && Number.isSafeInteger(channelId) && channelId > 0
    ? channelId
    : null;
}

function sameWatermark(
  left: { grid_epoch: string | null; seq: number | null },
  right: { grid_epoch: string | null; seq: number | null },
): boolean {
  return left.grid_epoch === right.grid_epoch && left.seq === right.seq;
}

function matchingCellGrid(
  frame: CoordWorkerUp,
  target: SourceFaultTarget,
): { full: boolean; gridEpoch: string; seq: string } | null {
  if (frame.frame.case !== "cellGrid") return null;
  const cellGrid = frame.frame.value;
  const grid = cellGrid.frame;
  if (cellGrid.channelId !== target.channelId || !grid || grid.streamId !== target.streamId) return null;
  return { full: grid.full, gridEpoch: grid.gridEpoch, seq: grid.seq.toString() };
}

function matchingFullChunk(
  frame: CoordWorkerUp,
  target: SourceFaultTarget,
): { gridEpoch: string; seq: string } | null {
  if (frame.frame.case !== "cellGridChunk") return null;
  const cellGridChunk = frame.frame.value;
  const part = cellGridChunk.chunk?.part;
  if (cellGridChunk.channelId !== target.channelId || !part || !part.full || part.streamId !== target.streamId) {
    return null;
  }
  return { gridEpoch: part.gridEpoch, seq: part.seq.toString() };
}

test("worker upstream delta loss obtains a source full on the same browser socket", async ({
  smokePage,
  secondSmokePage,
  stack,
}, testInfo) => {
  test.skip(!testInfo.project.name.startsWith("chromium"), "desktop source-repair contract");
  test.setTimeout(60_000);
  const sourceFault: SourceFaultState = {
    target: null,
    droppedDeltas: 0,
    droppedCheckpoint: null,
    repairFullStarts: 0,
    repairCheckpoint: null,
  };
  const fixtureWorker = await stack.startSecondPtyFixtureWorker({
    workerLinkOneWayDelayMs: 0,
    workerFrameFilter(frame) {
      const target = sourceFault.target;
      if (!target) return true;
      const cellGrid = matchingCellGrid(frame, target);
      if (cellGrid) {
        if (!cellGrid.full) {
          sourceFault.droppedDeltas += 1;
          sourceFault.droppedCheckpoint = { gridEpoch: cellGrid.gridEpoch, seq: cellGrid.seq };
          return false;
        }
        if (sourceFault.droppedDeltas > 0) {
          sourceFault.repairFullStarts += 1;
          sourceFault.repairCheckpoint = { gridEpoch: cellGrid.gridEpoch, seq: cellGrid.seq };
          sourceFault.target = null;
        }
        return true;
      }
      const chunk = matchingFullChunk(frame, target);
      if (chunk && sourceFault.droppedDeltas > 0) {
        sourceFault.repairFullStarts += 1;
        sourceFault.repairCheckpoint = chunk;
        sourceFault.target = null;
      }
      return true;
    },
  });
  const fixtureFolder = process.platform === "win32"
    ? fixtureWorker.home.replaceAll("\\", "/")
    : fixtureWorker.home;
  await smokePage.waitForFunction((workerFp) => !!window.__smoke.state().workers[workerFp], fixtureWorker.workerFp);
  const targetSession = await smokePage.evaluate(
    ({ workerFp, folder }) => window.__smoke.spawnShell(workerFp, folder),
    { workerFp: fixtureWorker.workerFp, folder: fixtureFolder },
  );
  const targetSessionId = targetSession.session_id;
  await navigateToSmokeSession(smokePage, targetSessionId);
  await smokePage.evaluate(
    ({ id, marker }) => window.__smoke.waitForPaintedMarker(id, marker, 10_000),
    { id: targetSessionId, marker: PTY_FIXTURE_READY },
  );
  await waitForStableCellFrames(smokePage, targetSessionId);
  await expect.poll(async () => {
    const probe = await readTerminalStreamProbe(smokePage, targetSessionId);
    return sourceChannelId(probe) !== null
      && probe.browser.replica.baseline_ready
      && probe.browser.replica.expected_stream_id !== null
      && sameWatermark(probe.browser.handler_canonical, probe.browser.dom_reconciled);
  }, { timeout: 10_000, intervals: [50, 100] }).toBe(true);
  const baseline = await readTerminalStreamProbe(smokePage, targetSessionId);
  const channelId = sourceChannelId(baseline);
  const streamId = baseline.browser.replica.expected_stream_id;
  if (channelId === null || streamId === null) throw new Error("target baseline omitted worker channel or stream identity");
  sourceFault.target = { channelId, streamId };
  try {
    const peerSessionId = await spawnPtyFixtureSession(secondSmokePage, fixtureWorker);
    await navigateToSmokeSession(secondSmokePage, peerSessionId);
    await secondSmokePage.evaluate(
      ({ id, marker }) => window.__smoke.waitForPaintedMarker(id, marker, 10_000),
      { id: peerSessionId, marker: PTY_FIXTURE_READY },
    );
    const before = await readTerminalStreamProbe(smokePage, targetSessionId);
    const beforeFullCount = await smokePage.evaluate(
      (id) => window.__smoke.cellFullFrameCount(id),
      targetSessionId,
    );
    expect(before.browser.sync.socket_generation).not.toBeNull();
    expect(before.browser.sync.socket_id).not.toBeNull();
    const suffix = crypto.randomUUID().replaceAll("-", "").slice(0, 8).toUpperCase();
    const targetMarker = `SOURCE-REPAIR-${suffix}-001`;
    const peerMarker = `SOURCE-PEER-${suffix}-001`;
    const laterMarker = `SOURCE-REPAIR-${suffix}-002`;
    const startedAt = Date.now();
    await Promise.all([
      inputSmokeTerminal(smokePage, targetSessionId, encodePtyFixtureCommand({ op: "EMIT", text: targetMarker })),
      inputSmokeTerminal(secondSmokePage, peerSessionId, encodePtyFixtureCommand({ op: "EMIT", text: peerMarker })),
    ]);
    await expect.poll(() => sourceFault.droppedDeltas, { timeout: 2_000, intervals: [20, 50] }).toBeGreaterThan(0);
    const peerPaint = secondSmokePage.evaluate(
      ({ id, marker, timeout }) => window.__smoke.waitForPaintedMarker(id, marker, timeout),
      { id: peerSessionId, marker: peerMarker, timeout: SOURCE_RECOVERY_TIMEOUT_MS },
    );
    await expect.poll(async () => {
      const probe = await readTerminalStreamProbe(smokePage, targetSessionId);
      return sourceFault.repairFullStarts === 1
        && sourceFault.repairCheckpoint !== null
        && probe.browser.replica.repair_attempts >= 1
        && probe.browser.replica.repair_outcome === "proved";
    }, { timeout: SOURCE_RECOVERY_TIMEOUT_MS, intervals: [50, 100] }).toBe(true);
    await peerPaint;
    const paintTimeout = Math.max(1, SOURCE_RECOVERY_TIMEOUT_MS - (Date.now() - startedAt));
    await smokePage.evaluate(
      ({ id, marker, timeout }) => window.__smoke.waitForPaintedMarker(id, marker, timeout),
      { id: targetSessionId, marker: targetMarker, timeout: paintTimeout },
    );
    expect(Date.now() - startedAt).toBeLessThanOrEqual(SOURCE_RECOVERY_TIMEOUT_MS);

    const repaired = await readTerminalStreamProbe(smokePage, targetSessionId);
    expect(sourceFault.droppedCheckpoint).not.toBeNull();
    expect(sourceFault.repairCheckpoint).not.toBeNull();
    expect(repaired.browser.sync.socket_generation).toBe(before.browser.sync.socket_generation);
    expect(repaired.browser.sync.socket_id).toBe(before.browser.sync.socket_id);
    expect(await smokePage.evaluate((id) => window.__smoke.cellFullFrameCount(id), targetSessionId))
      .toBe(beforeFullCount + 1);
    await inputSmokeTerminal(
      smokePage,
      targetSessionId,
      encodePtyFixtureCommand({ op: "EMIT", text: laterMarker }),
    );
    await smokePage.evaluate(
      ({ id, marker }) => window.__smoke.waitForPaintedMarker(id, marker, 10_000),
      { id: targetSessionId, marker: laterMarker },
    );
    expect(await smokePage.evaluate(
      ({ id, prefix }) => window.__smoke.markerScan(id, prefix),
      { id: targetSessionId, prefix: `SOURCE-REPAIR-${suffix}-` },
    )).toMatchObject({ total: 2, unique: 2, min: 1, max: 2, missing: 0, duplicated: [], outOfOrder: 0 });
  } finally {
    sourceFault.target = null;
  }
});

test("unreconciled terminal DOM recovers its Sync generation without replacing the renderer", async ({
  smokePage,
  secondSmokePage,
  stack,
}, testInfo) => {
  test.skip(!testInfo.project.name.startsWith("chromium"), "desktop DOM reconciliation contract");
  test.setTimeout(60_000);
  const fixtureWorker = await stack.startPtyFixtureWorker();
  const sessionId = await spawnPtyFixtureSession(smokePage, fixtureWorker);
  const peerSessionId = await spawnPtyFixtureSession(secondSmokePage, fixtureWorker);
  await Promise.all([
    navigateToSmokeSession(smokePage, sessionId),
    navigateToSmokeSession(secondSmokePage, peerSessionId),
  ]);
  await Promise.all([
    smokePage.evaluate(
      ({ id, marker }) => window.__smoke.waitForPaintedMarker(id, marker, 10_000),
      { id: sessionId, marker: PTY_FIXTURE_READY },
    ),
    secondSmokePage.evaluate(
      ({ id, marker }) => window.__smoke.waitForPaintedMarker(id, marker, 10_000),
      { id: peerSessionId, marker: PTY_FIXTURE_READY },
    ),
  ]);
  await waitForStableCellFrames(smokePage, sessionId);
  await expect.poll(async () => {
    const probe = await readTerminalStreamProbe(smokePage, sessionId);
    return probe.browser.replica.baseline_ready
      && sameWatermark(probe.browser.handler_canonical, probe.browser.dom_reconciled)
      && probe.browser.reconcile_block_reason === null;
  }, { timeout: 10_000, intervals: [50, 100] }).toBe(true);
  const before = await readTerminalStreamProbe(smokePage, sessionId);
  await smokePage.evaluate((id) => {
    const slot = document.querySelector(`[data-testid="terminal-slot-${CSS.escape(id)}"]`);
    const grid = slot?.querySelector(".cell-grid");
    const textarea = slot?.querySelector("textarea");
    if (!slot || !grid || !textarea) throw new Error("terminal identity probe unavailable");
    window.__terminalIdentityProbe = { slot, grid, textarea };
  }, sessionId);

  const suffix = crypto.randomUUID().replaceAll("-", "").slice(0, 8).toUpperCase();
  const heldMarker = `DOM-HOLD-${suffix}-001`;
  const peerMarker = `DOM-PEER-${suffix}-001`;
  let holdArmed = false;
  try {
    await smokePage.evaluate((id) => window.__smoke.holdTerminalDomForCurrentGeneration(id), sessionId);
    holdArmed = true;
    const startedAt = Date.now();
    await inputSmokeTerminal(
      smokePage,
      sessionId,
      encodePtyFixtureCommand({ op: "EMIT", text: heldMarker }),
    );
    const domHoldObserved = expect.poll(async () => {
      const probe = await readTerminalStreamProbe(smokePage, sessionId);
      const canonical = probe.browser.handler_canonical;
      const advanced = canonical.seq !== null
        && before.browser.handler_canonical.seq !== null
        && canonical.seq > before.browser.handler_canonical.seq;
      return {
        advanced,
        unreconciled: !sameWatermark(probe.browser.dom_reconciled, canonical),
        blockReason: probe.browser.reconcile_block_reason,
      };
    }, { timeout: 3_000, intervals: [25, 50] }).toEqual({ advanced: true, unreconciled: true, blockReason: "not_reconciled" });
    await inputSmokeTerminal(
      secondSmokePage,
      peerSessionId,
      encodePtyFixtureCommand({ op: "EMIT", text: peerMarker }),
    );
    await Promise.all([
      domHoldObserved,
      secondSmokePage.evaluate(
        ({ id, marker }) => window.__smoke.waitForPaintedMarker(id, marker, 10_000),
        { id: peerSessionId, marker: peerMarker },
      ),
    ]);
    await expect.poll(async () => {
      const sync = (await readTerminalStreamProbe(smokePage, sessionId)).browser.sync;
      return sync.ready
        && sync.socket_generation !== null
        && before.browser.sync.socket_generation !== null
        && sync.socket_generation > before.browser.sync.socket_generation
        && sync.socket_id !== before.browser.sync.socket_id;
    }, { timeout: DOM_RECOVERY_TIMEOUT_MS, intervals: [50, 100] }).toBe(true);
    expect(Date.now() - startedAt).toBeLessThanOrEqual(DOM_RECOVERY_TIMEOUT_MS);
    await smokePage.evaluate(
      ({ id, marker }) => window.__smoke.waitForPaintedMarker(id, marker, 10_000),
      { id: sessionId, marker: heldMarker },
    );
    const recovered = await readTerminalStreamProbe(smokePage, sessionId);
    expect(sameWatermark(recovered.browser.handler_canonical, recovered.browser.dom_reconciled)).toBe(true);
    expect(await smokePage.evaluate((id) => {
      const prior = window.__terminalIdentityProbe;
      const slot = document.querySelector(`[data-testid="terminal-slot-${CSS.escape(id)}"]`);
      if (!prior || !slot) return false;
      return slot === prior.slot
        && slot.querySelector(".cell-grid") === prior.grid
        && slot.querySelector("textarea") === prior.textarea
        && prior.slot.isConnected && prior.grid.isConnected && prior.textarea.isConnected;
    }, sessionId)).toBe(true);
    expect(await smokePage.evaluate(
      ({ id, prefix }) => window.__smoke.markerScan(id, prefix),
      { id: sessionId, prefix: `DOM-HOLD-${suffix}-` },
    )).toMatchObject({ total: 1, unique: 1, min: 1, max: 1, missing: 0, duplicated: [], outOfOrder: 0 });
  } finally {
    if (holdArmed) {
      await smokePage.evaluate((id) => window.__smoke.releaseTerminalDomHold(id), sessionId).catch(() => undefined);
    }
  }
});

test("a second transient zero terminal measurement publishes the recovered viewport without a layout edge", async ({
  smokePage,
  coldSmokePage,
  stack,
}, testInfo) => {
  test.skip(!testInfo.project.name.startsWith("chromium"), "desktop measurement recovery contract");
  test.setTimeout(45_000);
  const fixtureWorker = await stack.startPtyFixtureWorker();
  const fixtureFolder = process.platform === "win32"
    ? fixtureWorker.home.replaceAll("\\", "/")
    : fixtureWorker.home;
  await smokePage.waitForFunction((workerFp) => !!window.__smoke.state().workers[workerFp], fixtureWorker.workerFp);
  const session = await smokePage.evaluate(
    ({ workerFp, folder }) => window.__smoke.spawnShell(workerFp, folder),
    { workerFp: fixtureWorker.workerFp, folder: fixtureFolder },
  );
  await coldSmokePage.context().addInitScript(() => {
    const originalMeasure = HTMLElement.prototype.getBoundingClientRect;
    const fault = { failedMeasures: 0, restoredAtMs: null as number | null };
    (window as Window & { __terminalMeasurementFault?: typeof fault }).__terminalMeasurementFault = fault;
    HTMLElement.prototype.getBoundingClientRect = function measureTerminalProbe(): DOMRect {
      if (
        this instanceof HTMLSpanElement
        && this.classList.contains("cell-row")
        && this.textContent === "0000000000"
        && fault.failedMeasures < 2
      ) {
        fault.failedMeasures += 1;
        if (fault.failedMeasures === 2) fault.restoredAtMs = performance.now();
        return new DOMRect(0, 0, 0, 0);
      }
      return originalMeasure.call(this);
    };
  });
  await coldSmokePage.goto(`${stack.baseUrl}/s/${session.session_id}`, { waitUntil: "domcontentloaded" });
  await expect(coldSmokePage.getByTestId(`terminal-slot-${session.session_id}`)).toBeVisible({ timeout: 30_000 });
  await coldSmokePage.waitForFunction(() => window.__terminalMeasurementFault?.failedMeasures === 2, undefined, {
    timeout: 5_000,
  });
  await expect.poll(async () => {
    const probe = await readTerminalStreamProbe(coldSmokePage, session.session_id);
    return probe.browser.view.active
      && probe.browser.view.status === "accepted"
      && (probe.browser.view.effective_cols ?? 0) > 0
      && (probe.browser.view.effective_rows ?? 0) > 0
      && probe.browser.replica.baseline_ready;
  }, { timeout: MEASUREMENT_RECOVERY_TIMEOUT_MS, intervals: [20, 50] }).toBe(true);
  await coldSmokePage.evaluate(
    ({ id, marker, timeoutMs }) => window.__smoke.waitForPaintedMarker(id, marker, timeoutMs),
    { id: session.session_id, marker: PTY_FIXTURE_READY, timeoutMs: MEASUREMENT_RECOVERY_TIMEOUT_MS },
  );
  const published = await coldSmokePage.evaluate(() => {
    const fault = window.__terminalMeasurementFault;
    return {
      failedMeasures: fault?.failedMeasures ?? 0,
      elapsedMs: fault?.restoredAtMs === null || fault?.restoredAtMs === undefined
        ? null
        : performance.now() - fault.restoredAtMs,
    };
  });
  expect(published.failedMeasures).toBe(2);
  expect(published.elapsedMs).not.toBeNull();
  expect(published.elapsedMs!).toBeLessThanOrEqual(MEASUREMENT_RECOVERY_TIMEOUT_MS);
});
