import { expect, test } from "./fixtures.ts";
import type { TerminalStreamProbe } from "../../apps/web/src/lib/smoke.ts";
import type {
  RecoverySmokeApi,
  TerminalIdentityProbeWindow,
} from "./terminal-smoke-api.ts";
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
interface ForegroundEvents {
  visibility: number;
  focus: number;
  blur: number;
  pagehide: number;
}
type ForegroundSmokeWindow = Omit<Window, "__smoke"> & {
  __smoke: RecoverySmokeApi;
  __terminalIdentityProbe: TerminalIdentityProbeWindow["__terminalIdentityProbe"];
  __foregroundEvents: ForegroundEvents;
  __foregroundDocument: Document;
};
declare const window: ForegroundSmokeWindow;


function record(value: unknown): Record<string, unknown> | null {
  return value !== null && typeof value === "object" && !Array.isArray(value)
    ? value as Record<string, unknown>
    : null;
}

function sequence(value: unknown): bigint | null {
  if (typeof value === "number" && Number.isSafeInteger(value) && value >= 0) {
    return BigInt(value);
  }
  if (typeof value === "string" && /^\d+$/.test(value)) return BigInt(value);
  return null;
}

function sameWatermark(
  left: { grid_epoch: string | null; seq: number | null },
  right: { grid_epoch: string | null; seq: number | null },
): boolean {
  return left.grid_epoch === right.grid_epoch && left.seq === right.seq;
}

function everyLayerConverged(probe: TerminalStreamProbe): boolean {
  const browser = probe.browser;
  const canonical = browser.handler_canonical;
  if (
    canonical.seq === null
    || canonical.grid_epoch === null
    || !browser.replica.baseline_ready
    || browser.replica.resync_latched
    || browser.wire_received.stream_id !== browser.replica.expected_stream_id
    || browser.view.stream_id !== browser.replica.expected_stream_id
    || !sameWatermark(browser.wire_received, canonical)
    || !sameWatermark(browser.dom_reconciled, canonical)
    || !browser.presentation
    || !sameWatermark(browser.presentation.canonical, canonical)
    || !sameWatermark(browser.presentation.reconciled, canonical)
    || browser.reconcile_block_reason !== null
  ) return false;
  const coordScreen = record(record(probe.coord?.session)?.terminal_screen);
  const workerCell = record(record(probe.worker.session)?.cell);
  return coordScreen?.valid === true
    && coordScreen.stream_id === browser.replica.expected_stream_id
    && coordScreen.grid_epoch === canonical.grid_epoch
    && workerCell !== null
    && workerCell.grid_epoch === canonical.grid_epoch
    && sequence(coordScreen.seq) === BigInt(canonical.seq)
    && sequence(workerCell.seq) === BigInt(canonical.seq);
}

function foregroundVisible(probe: TerminalStreamProbe): boolean {
  return probe.browser.visibility.document_visible
    && probe.browser.visibility.page_visible
    && probe.browser.slot.registered
    && probe.browser.slot.connected
    && probe.browser.slot.in_layout === true
    && probe.browser.slot.surface_active === true
    && probe.browser.slot.css_visible === true;
}

async function installForegroundIdentityProbe(
  page: Parameters<typeof readTerminalStreamProbe>[0],
  sessionId: string,
): Promise<void> {
  await page.evaluate((id) => {
    const slot = document.querySelector(`[data-testid="terminal-slot-${CSS.escape(id)}"]`);
    const grid = slot?.querySelector(".cell-grid");
    const textarea = slot?.querySelector("textarea");
    if (!slot || !grid || !textarea) throw new Error("terminal identity probe unavailable");
    const target = window;
    target.__terminalIdentityProbe = { slot, grid, textarea };
    target.__foregroundEvents = { visibility: 0, focus: 0, blur: 0, pagehide: 0 };
    target.__foregroundDocument = document;
    document.addEventListener("visibilitychange", () => target.__foregroundEvents.visibility++);
    window.addEventListener("focus", () => target.__foregroundEvents.focus++);
    window.addEventListener("blur", () => target.__foregroundEvents.blur++);
    window.addEventListener("pagehide", () => target.__foregroundEvents.pagehide++);
  }, sessionId);
}

async function identityAndForegroundState(
  page: Parameters<typeof readTerminalStreamProbe>[0],
  sessionId: string,
) {
  return page.evaluate((id) => {
    const target = window;
    const prior = target.__terminalIdentityProbe;
    const slot = document.querySelector(`[data-testid="terminal-slot-${CSS.escape(id)}"]`);
    return {
      identity: {
        slot: slot === prior.slot,
        grid: slot?.querySelector(".cell-grid") === prior.grid,
        textarea: slot?.querySelector("textarea") === prior.textarea,
        connected: prior.slot.isConnected && prior.grid.isConnected && prior.textarea.isConnected,
        document: document === target.__foregroundDocument,
      },
      events: target.__foregroundEvents,
      documentVisible: document.visibilityState === "visible",
    };
  }, sessionId);
}

test("foreground terminal blackhole recovers without visibility or identity changes", async ({
  smokePage,
  stack,
}, testInfo) => {
  test.skip(!testInfo.project.name.startsWith("chromium"), "desktop foreground liveness contract");
  test.setTimeout(90_000);
  const fixtureWorker = await stack.startPtyFixtureWorker();
  const sessionId = await spawnPtyFixtureSession(smokePage, fixtureWorker);
  const suffix = crypto.randomUUID().replaceAll("-", "").slice(0, 8).toUpperCase();
  const markerPrefix = `FG-LIVE-${suffix}-`;
  const interruptedMarker = `${markerPrefix}001`;
  const laterMarker = `${markerPrefix}002`;
  await navigateToSmokeSession(smokePage, sessionId);
  await smokePage.evaluate(
    ({ id, marker }) => window.__smoke.waitForPaintedMarker(id, marker, 10_000),
    { id: sessionId, marker: PTY_FIXTURE_READY },
  );
  await waitForStableCellFrames(smokePage, sessionId);
  await expect.poll(async () => everyLayerConverged(
    await readTerminalStreamProbe(smokePage, sessionId),
  ), { timeout: 15_000, intervals: [50, 100] }).toBe(true);

  const before = await readTerminalStreamProbe(smokePage, sessionId);
  const beforeFull = await smokePage.evaluate(
    (id) => window.__smoke.cellFullFrameCount(id),
    sessionId,
  );
  const beforeHistory = await smokePage.evaluate(
    (id) => window.__smoke.scrollbackBackfillRequestCount(id),
    sessionId,
  );
  await installForegroundIdentityProbe(smokePage, sessionId);
  const startedAt = Date.now();
  const interruptedFrame = encodePtyFixtureCommand({
    op: "EMIT",
    text: interruptedMarker,
  });
  await smokePage.evaluate(async ({ id, frame }) => {
    const smoke = window.__smoke;
    smoke.blackholeTerminalFramesForCurrentGeneration(id);
    await smoke.input(id, frame);
  }, { id: sessionId, frame: interruptedFrame });

  await expect.poll(async () => {
    const probe = await readTerminalStreamProbe(smokePage, sessionId);
    return foregroundVisible(probe)
      && probe.browser.replica.challenge_age_ms !== null
      && probe.browser.faults.blackhole_drop_count > 0;
  }, { timeout: 22_000, intervals: [100, 250] }).toBe(true);
  const challengeProbe = await readTerminalStreamProbe(smokePage, sessionId);
  const challengeIssuedAt = Date.now() - startedAt
    - (challengeProbe.browser.replica.challenge_age_ms ?? 0);
  expect(challengeIssuedAt).toBeLessThanOrEqual(20_000);

  const generationTimeout = Math.max(1_000, 35_000 - (Date.now() - startedAt));
  await expect.poll(async () => {
    const sync = (await readTerminalStreamProbe(smokePage, sessionId)).browser.sync;
    return sync.ready
      && sync.socket_generation !== null
      && before.browser.sync.socket_generation !== null
      && sync.socket_generation > before.browser.sync.socket_generation
      && sync.socket_id !== before.browser.sync.socket_id;
  }, { timeout: generationTimeout, intervals: [100, 250] }).toBe(true);
  expect(Date.now() - startedAt).toBeLessThanOrEqual(35_000);

  const paintTimeout = Math.max(1_000, 50_000 - (Date.now() - startedAt));
  await smokePage.evaluate(
    ({ id, marker, timeout }) => window.__smoke.waitForPaintedMarker(id, marker, timeout),
    { id: sessionId, marker: interruptedMarker, timeout: paintTimeout },
  );
  expect(Date.now() - startedAt).toBeLessThanOrEqual(50_000);
  await waitForStableCellFrames(smokePage, sessionId);
  await expect.poll(async () => everyLayerConverged(
    await readTerminalStreamProbe(smokePage, sessionId),
  ), { timeout: 15_000, intervals: [50, 100] }).toBe(true);

  const recovered = await readTerminalStreamProbe(smokePage, sessionId);
  expect(recovered.browser.visibility).toEqual({
    document_visible: true,
    page_visible: true,
  });
  expect(recovered.browser.slot).toMatchObject({
    registered: true,
    connected: true,
    in_layout: true,
    surface_active: true,
    css_visible: true,
  });
  expect(recovered.browser.faults.blackhole_drop_count).toBeGreaterThan(0);
  expect(await smokePage.evaluate(
    (id) => window.__smoke.cellFullFrameCount(id),
    sessionId,
  ) - beforeFull).toBe(1);
  expect(await smokePage.evaluate(
    (id) => window.__smoke.lastFullFrameSbRows(id),
    sessionId,
  )).toBe(0);
  expect(await smokePage.evaluate(
    (id) => window.__smoke.scrollbackBackfillRequestCount(id),
    sessionId,
  )).toBe(beforeHistory);
  expect(await identityAndForegroundState(smokePage, sessionId)).toEqual({
    identity: {
      slot: true,
      grid: true,
      textarea: true,
      connected: true,
      document: true,
    },
    events: { visibility: 0, focus: 0, blur: 0, pagehide: 0 },
    documentVisible: true,
  });
  expect(await smokePage.evaluate(
    ({ id, prefix }) => window.__smoke.markerScan(id, prefix),
    { id: sessionId, prefix: markerPrefix },
  )).toMatchObject({ total: 1, unique: 1, min: 1, max: 1, missing: 0, duplicated: [], outOfOrder: 0 });

  const recoveredSeq = recovered.browser.handler_canonical.seq!;
  await inputSmokeTerminal(
    smokePage,
    sessionId,
    encodePtyFixtureCommand({ op: "EMIT", text: laterMarker }),
  );
  await expect.poll(async () => (
    await readTerminalStreamProbe(smokePage, sessionId)
  ).browser.handler_canonical.seq).toBeGreaterThan(recoveredSeq);
  await smokePage.evaluate(
    ({ id, marker }) => window.__smoke.waitForPaintedMarker(id, marker, 10_000),
    { id: sessionId, marker: laterMarker },
  );
  expect(await smokePage.evaluate(
    ({ id, prefix }) => window.__smoke.markerScan(id, prefix),
    { id: sessionId, prefix: markerPrefix },
  )).toMatchObject({ total: 2, unique: 2, min: 1, max: 2, missing: 0, duplicated: [], outOfOrder: 0 });
});

test("one raw terminal delta loss repairs on the same socket generation", async ({
  smokePage,
  stack,
}, testInfo) => {
  test.skip(!testInfo.project.name.startsWith("chromium"), "desktop wire-gap repair contract");
  test.setTimeout(60_000);
  const fixtureWorker = await stack.startPtyFixtureWorker();
  const sessionId = await spawnPtyFixtureSession(smokePage, fixtureWorker);
  const suffix = crypto.randomUUID().replaceAll("-", "").slice(0, 8).toUpperCase();
  const markerPrefix = `WIRE-LOSS-${suffix}-`;
  const firstMarker = `${markerPrefix}001`;
  const secondMarker = `${markerPrefix}002`;
  const laterMarker = `${markerPrefix}003`;
  await navigateToSmokeSession(smokePage, sessionId);
  await smokePage.evaluate(
    ({ id, marker }) => window.__smoke.waitForPaintedMarker(id, marker, 10_000),
    { id: sessionId, marker: PTY_FIXTURE_READY },
  );
  await waitForStableCellFrames(smokePage, sessionId);
  const before = await readTerminalStreamProbe(smokePage, sessionId);
  const beforeFull = await smokePage.evaluate(
    (id) => window.__smoke.cellFullFrameCount(id),
    sessionId,
  );
  const beforeHistory = await smokePage.evaluate(
    (id) => window.__smoke.scrollbackBackfillRequestCount(id),
    sessionId,
  );

  const frames = encodePtyFixtureCommand({ op: "EMIT", text: firstMarker })
    + encodePtyFixtureCommand({ op: "EMIT", text: secondMarker, delayMs: 350 });
  await smokePage.evaluate(async ({ id, payload }) => {
    const smoke = window.__smoke;
    smoke.dropNextTerminalWireDelta(id);
    await smoke.input(id, payload);
  }, { id: sessionId, payload: frames });
  await expect.poll(async () => {
    const faults = (await readTerminalStreamProbe(smokePage, sessionId)).browser.faults;
    return faults.wire_delta_drop_count === 1
      && faults.wire_delta_dropped_seq !== null
      && faults.wire_delta_post_drop_seq !== null;
  }, { timeout: 10_000, intervals: [25, 50] }).toBe(true);

  const faultProbe = await readTerminalStreamProbe(smokePage, sessionId);
  expect(faultProbe.browser.faults.wire_delta_post_drop_seq)
    .toBe(faultProbe.browser.faults.wire_delta_dropped_seq! + 1);
  await expect.poll(async () => {
    const probe = await readTerminalStreamProbe(smokePage, sessionId);
    const fulls = await smokePage.evaluate(
      (id) => window.__smoke.cellFullFrameCount(id),
      sessionId,
    );
    return everyLayerConverged(probe)
      && probe.browser.replica.repair_attempts >= 1
      && probe.browser.replica.repair_outcome === "proved"
      && fulls === beforeFull + 1;
  }, { timeout: 15_000, intervals: [50, 100] }).toBe(true);

  const repaired = await readTerminalStreamProbe(smokePage, sessionId);
  expect(repaired.browser.sync.socket_generation).toBe(before.browser.sync.socket_generation);
  expect(repaired.browser.sync.socket_id).toBe(before.browser.sync.socket_id);
  expect(await smokePage.evaluate(
    (id) => window.__smoke.scrollbackBackfillRequestCount(id),
    sessionId,
  )).toBe(beforeHistory);
  expect(await smokePage.evaluate(
    ({ id, prefix }) => window.__smoke.markerScan(id, prefix),
    { id: sessionId, prefix: markerPrefix },
  )).toMatchObject({ total: 2, unique: 2, min: 1, max: 2, missing: 0, duplicated: [], outOfOrder: 0 });

  await inputSmokeTerminal(
    smokePage,
    sessionId,
    encodePtyFixtureCommand({ op: "EMIT", text: laterMarker }),
  );
  await smokePage.evaluate(
    ({ id, marker }) => window.__smoke.waitForPaintedMarker(id, marker, 10_000),
    { id: sessionId, marker: laterMarker },
  );
  expect(await smokePage.evaluate(
    ({ id, prefix }) => window.__smoke.markerScan(id, prefix),
    { id: sessionId, prefix: markerPrefix },
  )).toMatchObject({ total: 3, unique: 3, min: 1, max: 3, missing: 0, duplicated: [], outOfOrder: 0 });
});
