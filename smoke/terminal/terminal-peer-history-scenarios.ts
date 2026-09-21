// Transport history continuity scenarios drive one real fixture PTY through
// carrier-specific routes, geometry changes, history demands, and tab switches.
// They compare retained worker rows, canonical frames, and painted DOM rows.
// terminal-peer-failover.spec.ts owns the Playwright registrations.
import type { Browser, Page, TestInfo } from "@playwright/test";
import type { TerminalStreamProbe } from "../../apps/web/src/lib/smoke.ts";
import { expect } from "./fixtures.ts";
import type { EnrolledPage } from "./terminal-local-fast-path-helpers.ts";
import { attachStackLogs } from "./terminal-local-fast-path-helpers.ts";
import {
  createPeerFixtureSession,
  createPeerFixtureSessionInDocument,
  openPeerSmokePage,
  waitForDirectRoute,
  waitForSyncRoute,
} from "./terminal-peer-helpers.ts";
import { inputSmokeTerminal, switchToSmokeSession, waitForStableCellFrames } from "./terminal-helpers.ts";
import { expectHistoryAnchorPreserved, waitForHistoryAnchor, waitForPainted } from "./terminal-multiview-helpers.ts";
import { expectPaintedScrollbackWellFormed, readTerminalStreamProbe } from "./terminal-probe-helpers.ts";
import { encodePtyFixtureCommand } from "./pty-fixture-protocol.ts";
import { expectTerminalTransportIndicator } from "./terminal-transport-indicator-helpers.ts";
import type { TerminalTestStack, TerminalTestWorker } from "./stack.ts";
import { startTerminalTestStack } from "./stack.ts";

export type PeerHistoryCarrier = "sync" | "loopback" | "webrtc";

const HISTORY_LINE_COUNT = 2_500;
const CYCLE_COUNT = 2;
const PRIMARY_VIEWPORT = { width: 1_440, height: 640 };
const SECONDARY_VIEWPORT = { width: 900, height: 960 };

const SYNC_HISTORY_STACK_OPTIONS = {
  terminalPeer: {
    coordinatorEnabled: true,
    coordinatorStunUrls: [],
    workerEnabled: false,
    disableLoopbackProbe: true,
  },
} as const;

const LOOPBACK_HISTORY_STACK_OPTIONS = {
  terminalPeer: {
    coordinatorEnabled: true,
    coordinatorStunUrls: [],
    workerEnabled: true,
    disableLoopbackProbe: false,
  },
} as const;

const WEBRTC_HISTORY_STACK_OPTIONS = {
  terminalPeer: {
    coordinatorEnabled: true,
    coordinatorStunUrls: [],
    workerEnabled: true,
    disableLoopbackProbe: true,
    enableFaults: true,
  },
} as const;

/** Exercises one carrier in isolation so a failure identifies transport versus core or renderer ownership. */
export async function verifyPeerHistoryContinuity(
  browser: Browser,
  testInfo: TestInfo,
  carrier: PeerHistoryCarrier,
): Promise<void> {
  const stack = await startTerminalTestStack(historyStackOptions(carrier));
  let primary: EnrolledPage | undefined;
  let secondary: EnrolledPage | undefined;
  try {
    const fixtureWorker = await stack.startPtyFixtureWorker();
    primary = await openHistoryPage(browser, stack, fixtureWorker, carrier, carrier === "webrtc");
    await primary.page.setViewportSize(PRIMARY_VIEWPORT);
    const sessionId = await createPeerFixtureSession(primary.page, fixtureWorker);
    const initialCarrier = carrier === "webrtc" ? "sync" : carrier;
    await waitForCarrier(primary.page, sessionId, initialCarrier);
    await expectTerminalTransportIndicator(primary.page, sessionId, initialCarrier);

    const suffix = crypto.randomUUID().replaceAll("-", "").slice(0, 8);
    const historyPrefix = `PEER-CONTINUITY-${carrier}-${suffix}-`;
    await inputSmokeTerminal(primary.page, sessionId, encodePtyFixtureCommand({
      op: "FLOOD",
      prefix: historyPrefix,
      count: HISTORY_LINE_COUNT,
    }));
    await waitForPainted(primary.page, sessionId, `${historyPrefix}${HISTORY_LINE_COUNT}`);
    await waitForStableCellFrames(primary.page, sessionId);
    if (carrier === "webrtc") {
      await primary.page.evaluate(() => {
        window.__releaseTerminalPeerAnswer?.();
        delete window.__releaseTerminalPeerAnswer;
      });
      await waitForDirectRoute(primary.page, sessionId, "webrtc");
      await expectTerminalTransportIndicator(primary.page, sessionId, "webrtc");
    }
    await assertHistoryStages(primary.page, sessionId, historyPrefix, carrier);

    await scrollReaderOffBottom(primary.page, sessionId, historyPrefix);
    secondary = await openHistoryPage(browser, stack, fixtureWorker, carrier);
    await secondary.page.setViewportSize(SECONDARY_VIEWPORT);
    await switchToSmokeSession(secondary.page, sessionId);
    await Promise.all([
      waitForCarrier(primary.page, sessionId, carrier),
      waitForCarrier(secondary.page, sessionId, carrier),
      waitForStableCellFrames(primary.page, sessionId),
      waitForStableCellFrames(secondary.page, sessionId),
    ]);
    await returnReaderToLive(primary.page, sessionId);
    await assertHistoryStages(primary.page, sessionId, historyPrefix, carrier);

    const spareSessionId = await createPeerFixtureSessionInDocument(primary.page, fixtureWorker);
    await switchToSmokeSession(primary.page, sessionId);
    await waitForCarrier(primary.page, sessionId, carrier);

    for (let cycle = 0; cycle < CYCLE_COUNT; cycle++) {
      const anchor = await scrollReaderOffBottom(primary.page, sessionId, historyPrefix);
      if (carrier === "webrtc") {
        const peerFaults = stack.peerFaults;
        if (!peerFaults) throw new Error("WebRTC history scenario lacked packet fault controls");
        await peerFaults.setPeerPacketBlackhole(fixtureWorker.label, true);
        await waitForSyncRoute(primary.page, sessionId);
        await expectHistoryAnchorPreserved(primary.page, sessionId, anchor);
        await demandUnpaintedHistoryPage(primary.page, sessionId);
        await peerFaults.setPeerPacketBlackhole(fixtureWorker.label, false);
        await waitForDirectRoute(primary.page, sessionId, "webrtc");
      } else {
        await demandUnpaintedHistoryPage(primary.page, sessionId);
      }

      await returnReaderToLive(primary.page, sessionId);
      await switchToSmokeSession(primary.page, spareSessionId);
      await waitForCarrier(primary.page, spareSessionId, carrier);
      await switchToSmokeSession(primary.page, sessionId);
      await waitForCarrier(primary.page, sessionId, carrier);
      await waitForStableCellFrames(primary.page, sessionId);
      await assertHistoryStages(primary.page, sessionId, historyPrefix, carrier);
    }
  } finally {
    try {
      await secondary?.close();
    } finally {
      await stopPeerHistoryScenario(stack, primary, testInfo);
    }
  }
}

function historyStackOptions(carrier: PeerHistoryCarrier) {
  switch (carrier) {
    case "sync": return SYNC_HISTORY_STACK_OPTIONS;
    case "loopback": return LOOPBACK_HISTORY_STACK_OPTIONS;
    case "webrtc": return WEBRTC_HISTORY_STACK_OPTIONS;
  }
}

function openHistoryPage(
  browser: Browser,
  stack: TerminalTestStack,
  fixtureWorker: TerminalTestWorker,
  carrier: PeerHistoryCarrier,
  holdWebRtcAnswer = false,
): Promise<EnrolledPage> {
  if (carrier === "sync") return openPeerSmokePage(browser, stack, { rtcUnavailable: true });
  if (carrier === "loopback") {
    return openPeerSmokePage(browser, stack, { origin: stack.localUiUrl(fixtureWorker.workerFp) });
  }
  return openPeerSmokePage(browser, stack, holdWebRtcAnswer ? { holdRtcAnswer: true } : {});
}

function waitForCarrier(page: Page, sessionId: string, carrier: PeerHistoryCarrier): Promise<unknown> {
  if (carrier === "sync") return waitForSyncRoute(page, sessionId);
  return waitForDirectRoute(page, sessionId, carrier);
}

async function scrollReaderOffBottom(
  page: Page,
  sessionId: string,
  historyPrefix: string,
): Promise<{ row: number; text: string; offsetPx: number }> {
  const scrollBox = await page.getByTestId(`terminal-slot-${sessionId}`).locator(".wterm").boundingBox();
  if (!scrollBox) throw new Error("history continuity terminal scroll container was unavailable");
  await page.mouse.move(scrollBox.x + scrollBox.width / 2, scrollBox.y + scrollBox.height / 2);
  await page.mouse.wheel(0, -2_400);
  return waitForHistoryAnchor(page, sessionId, historyPrefix);
}

async function demandUnpaintedHistoryPage(page: Page, sessionId: string): Promise<void> {
  const before = await page.evaluate((id) => ({
    requests: window.__smoke.scrollbackBackfillRequestCount(id),
    painted: window.__smoke.paintedScrollback(id),
  }), sessionId);
  expect(before.painted.headSpacerPx + before.painted.tailGapPx).toBeGreaterThan(0);
  await page.mouse.wheel(0, -100_000);
  await expect.poll(async () => {
    const current = await page.evaluate((id) => ({
      requests: window.__smoke.scrollbackBackfillRequestCount(id),
      painted: window.__smoke.paintedScrollback(id),
    }), sessionId);
    return current.requests > before.requests
      || current.painted.headSpacerPx + current.painted.tailGapPx
        < before.painted.headSpacerPx + before.painted.tailGapPx;
  }, { timeout: 60_000, intervals: [100, 250, 500] }).toBe(true);
  expectPaintedScrollbackWellFormed(await page.evaluate(
    (id) => window.__smoke.paintedScrollback(id),
    sessionId,
  ));
}

async function returnReaderToLive(page: Page, sessionId: string): Promise<void> {
  await page.mouse.wheel(0, 100_000);
  await expect.poll(() => page.evaluate(
    (id) => window.__smoke.renderProbe(id).atBottom,
    sessionId,
  ), { timeout: 30_000, intervals: [50, 100, 250] }).toBe(true);
}

async function assertHistoryStages(
  page: Page,
  sessionId: string,
  historyPrefix: string,
  carrier: PeerHistoryCarrier,
): Promise<void> {
  await expect.poll(
    async () => terminalLayersConverged(
      await readTerminalStreamProbe(page, sessionId),
      carrier === "sync",
    ),
    { timeout: 60_000, intervals: [100, 250, 500] },
  ).toBe(true);
  const [retained, painted, markerScan, domMarkerIds] = await Promise.all([
    page.evaluate(({ id, prefix }) => window.__smoke.retainedMarkerScan(id, prefix, 4_096), {
      id: sessionId,
      prefix: historyPrefix,
    }),
    page.evaluate((id) => window.__smoke.paintedScrollback(id), sessionId),
    page.evaluate(({ id, prefix }) => window.__smoke.markerScan(id, prefix), {
      id: sessionId,
      prefix: historyPrefix,
    }),
    page.evaluate(({ id, prefix }) => {
      const escapedPrefix = prefix.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
      const markerPattern = new RegExp(`${escapedPrefix}(\\d+)`, "g");
      const rows = document.querySelectorAll(`[data-testid="terminal-slot-${CSS.escape(id)}"] .cell-row`);
      const markers: number[] = [];
      for (const row of rows) {
        markerPattern.lastIndex = 0;
        let match: RegExpExecArray | null;
        while ((match = markerPattern.exec(row.textContent ?? "")) !== null) markers.push(Number(match[1]));
      }
      return markers.filter((marker) => Number.isSafeInteger(marker));
    }, { id: sessionId, prefix: historyPrefix }),
  ]);
  expect(retained).toMatchObject({
    markerMin: 1,
    markerMissing: 0,
    markerDuplicated: [],
    markerOutOfOrder: 0,
    rowGapCount: 0,
  });
  expect(retained.markerMax).toBeLessThanOrEqual(HISTORY_LINE_COUNT);
  expect(new Set(retained.rowIndices).size).toBe(retained.rowIndices.length);
  expect(retained.rowIndices).toEqual([...retained.rowIndices].sort((left, right) => left - right));
  expect(markerScan).toMatchObject({
    max: HISTORY_LINE_COUNT,
    duplicated: [],
    outOfOrder: 0,
  });
  const observedMarkerIds = new Set([...retained.markerIds, ...domMarkerIds]);
  expect([...observedMarkerIds].sort((left, right) => left - right))
    .toEqual(Array.from({ length: HISTORY_LINE_COUNT }, (_, index) => index + 1));
  expectPaintedScrollbackWellFormed(painted);
}

function terminalLayersConverged(probe: TerminalStreamProbe, requireCoordinator: boolean): boolean {
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
    || browser.presentation === null
    || !sameWatermark(browser.presentation.canonical, canonical)
    || !sameWatermark(browser.presentation.reconciled, canonical)
    || browser.reconcile_block_reason !== null
  ) return false;
  const workerCell = record(record(probe.worker.session)?.cell);
  if (
    workerCell?.grid_epoch !== canonical.grid_epoch
    || sequence(workerCell.seq) !== BigInt(canonical.seq)
  ) return false;
  if (!requireCoordinator) return true;
  const coordScreen = record(record(probe.coord?.session)?.terminal_screen);
  return coordScreen?.valid === true
    && coordScreen.stream_id === browser.replica.expected_stream_id
    && coordScreen.grid_epoch === canonical.grid_epoch
    && sequence(coordScreen.seq) === BigInt(canonical.seq);
}

function record(value: unknown): Record<string, unknown> | null {
  return value !== null && typeof value === "object" && !Array.isArray(value)
    ? value as Record<string, unknown>
    : null;
}

function sequence(value: unknown): bigint | null {
  if (typeof value === "number" && Number.isSafeInteger(value) && value >= 0) return BigInt(value);
  if (typeof value === "string" && /^\d+$/.test(value)) return BigInt(value);
  return null;
}

function sameWatermark(
  left: { grid_epoch: string | null; seq: number | null },
  right: { grid_epoch: string | null; seq: number | null },
): boolean {
  return left.grid_epoch === right.grid_epoch && left.seq === right.seq;
}

async function stopPeerHistoryScenario(
  stack: TerminalTestStack,
  page: EnrolledPage | undefined,
  testInfo: TestInfo,
): Promise<void> {
  try {
    await attachStackLogs(testInfo, stack);
  } finally {
    try {
      await page?.close();
    } finally {
      await stack.stop();
    }
  }
}
