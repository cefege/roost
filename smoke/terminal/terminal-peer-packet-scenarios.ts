// Large direct terminal packet smoke proof uses the real fixture output path.
// It forces multi-megabyte cell delivery and a direct history demand, then sends
// a trusted control key so history traffic cannot hide control starvation.

import type { Browser, TestInfo } from "@playwright/test";
import { expect } from "./fixtures.ts";
import type { EnrolledPage } from "./terminal-local-fast-path-helpers.ts";
import { attachStackLogs } from "./terminal-local-fast-path-helpers.ts";
import {
  blockCoordinatorHistoryFallback,
  createPeerFixtureSession,
  openPeerSmokePage,
  readPeerRoute,
  sendTrustedPeerKey,
  waitForDirectRoute,
} from "./terminal-peer-helpers.ts";
import { encodePtyFixtureCommand } from "./pty-fixture-protocol.ts";
import { inputSmokeTerminal, waitForStableCellFrames } from "./terminal-helpers.ts";
import { waitForPainted } from "./terminal-multiview-helpers.ts";
import type { TerminalTestStack } from "./stack.ts";
import { startTerminalTestStack } from "./stack.ts";

const PEER_STACK_OPTIONS = {
  terminalPeer: {
    coordinatorEnabled: true,
    coordinatorStunUrls: [],
    workerEnabled: true,
    disableLoopbackProbe: true,
  },
} as const;

async function stopPeerPacketScenario(
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

/** Produces enough encoded cell data to cross the direct framing boundary more than once. */
export async function verifyLargeDirectPacketAndHistory(browser: Browser, testInfo: TestInfo): Promise<void> {
  const stack = await startTerminalTestStack(PEER_STACK_OPTIONS);
  let page: EnrolledPage | undefined;
  let unblockCoordinatorHistory: (() => Promise<void>) | undefined;
  try {
    const fixtureWorker = await stack.startPtyFixtureWorker();
    page = await openPeerSmokePage(browser, stack);
    const sessionId = await createPeerFixtureSession(page.page, fixtureWorker);
    await waitForDirectRoute(page.page, sessionId);

    const suffix = crypto.randomUUID().replaceAll("-", "");
    const prefix = `PEER-LARGE-${suffix}-${"x".repeat(48)}-`;
    const count = 40_000;
    await inputSmokeTerminal(page.page, sessionId, encodePtyFixtureCommand({
      op: "FLOOD",
      prefix,
      count,
    }));
    await waitForPainted(page.page, sessionId, `-${count}`);
    await waitForStableCellFrames(page.page, sessionId);
    const retained = await page.page.evaluate(
      ({ id, markerPrefix }) => window.__smoke.retainedMarkerScan(id, markerPrefix, 512),
      { id: sessionId, markerPrefix: prefix },
    );
    expect(retained).toMatchObject({ markerMax: count - 31, markerMissing: 0, markerDuplicated: [] });

    const route = await readPeerRoute(page.page, sessionId);
    expect(route).toMatchObject({ activeKind: "webrtc", proofKind: "webrtc" });
    expect(route.activeBufferedBytes ?? 0).toBeLessThanOrEqual(64 * 1024 * 1024 + 256 * 1024);
    const directHistoryResponsesBefore = await page.page.evaluate(
      (id) => window.__smoke.directHistoryResponseCount(id),
      sessionId,
    );
    await page.page.evaluate(() => window.__smoke.pauseSyncTransport());
    unblockCoordinatorHistory = await blockCoordinatorHistoryFallback(page.page);
    const backfillsBefore = await page.page.evaluate(
      (id) => window.__smoke.scrollbackBackfillRequestCount(id),
      sessionId,
    );
    const scrollBox = await page.page.getByTestId(`terminal-slot-${sessionId}`).locator(".wterm").boundingBox();
    if (!scrollBox) throw new Error("large packet terminal scroll container was unavailable");
    await page.page.mouse.move(scrollBox.x + scrollBox.width / 2, scrollBox.y + scrollBox.height / 2);
    await page.page.mouse.wheel(0, -100_000);
    await expect.poll(() => page!.page.evaluate(
      (id) => window.__smoke.scrollbackBackfillRequestCount(id),
      sessionId,
    ), { timeout: 60_000, intervals: [100, 250, 500] }).toBeGreaterThan(backfillsBefore);
    await expect.poll(() => page!.page.evaluate(
      (id) => window.__smoke.directHistoryResponseCount(id),
      sessionId,
    ), { timeout: 60_000, intervals: [100, 250, 500] }).toBeGreaterThan(directHistoryResponsesBefore);
    await page.page.mouse.wheel(0, 100_000);
    await expect.poll(() => page!.page.evaluate(
      (id) => window.__smoke.renderProbe(id).atBottom,
      sessionId,
    ), { timeout: 10_000, intervals: [50, 100, 250] }).toBe(true);
    const key = await sendTrustedPeerKey(page.page, sessionId);
    await waitForPainted(page.page, sessionId, key.marker);
  } finally {
    await unblockCoordinatorHistory?.().catch(() => undefined);
    await stopPeerPacketScenario(stack, page, testInfo);
  }
}
