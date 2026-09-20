// Packet-port fault scenarios stay inside a disposable source-worker stack.
// They inject malformed authenticated peer packets at the real reassembler and
// pause only the history lane, while assertions remain browser/PTY observable.
// terminal-peer-failover.spec.ts registers the scenarios as Playwright tests.

import type { Browser, TestInfo } from "@playwright/test";
import { expect } from "./fixtures.ts";
import type { EnrolledPage } from "./terminal-local-fast-path-helpers.ts";
import { attachStackLogs } from "./terminal-local-fast-path-helpers.ts";
import {
  blockCoordinatorHistoryFallback,
  createPeerFixtureSession,
  openPeerSmokePage,
  sendTrustedPeerKey,
  waitForDirectRoute,
  waitForSyncRoute,
} from "./terminal-peer-helpers.ts";
import { encodePtyFixtureCommand } from "./pty-fixture-protocol.ts";
import { inputSmokeTerminal, navigateToSmokeSession, waitForStableCellFrames } from "./terminal-helpers.ts";
import { waitForPainted } from "./terminal-multiview-helpers.ts";
import type { TerminalTestStack } from "./stack.ts";
import { startTerminalTestStack } from "./stack.ts";

const PEER_PACKET_FAULT_STACK_OPTIONS = {
  terminalPeer: {
    coordinatorEnabled: true,
    coordinatorStunUrls: [],
    workerEnabled: true,
    disableLoopbackProbe: true,
    enableFaults: true,
  },
} as const;

async function stopPeerPacketFaultScenario(
  stack: TerminalTestStack,
  page: EnrolledPage | undefined,
  failed: boolean,
  testInfo: TestInfo,
): Promise<void> {
  try {
    if (failed) await attachStackLogs(testInfo, stack);
    await page?.close();
  } finally {
    await stack.stop();
  }
}

/** Every malformed header variant retires only its one peer and leaves another worker's peer usable. */
export async function verifyMalformedPeerPacketIsolation(browser: Browser, testInfo: TestInfo): Promise<void> {
  const kinds = ["offset", "total", "id"] as const;
  for (const kind of kinds) {
    const stack = await startTerminalTestStack(PEER_PACKET_FAULT_STACK_OPTIONS);
    let page: EnrolledPage | undefined;
    try {
      const affectedWorker = await stack.startPtyFixtureWorker();
      const healthyWorker = await stack.startSecondPtyFixtureWorker();
      page = await openPeerSmokePage(browser, stack);
      const healthySessionId = await createPeerFixtureSession(page.page, healthyWorker);
      await waitForDirectRoute(page.page, healthySessionId);
      const affectedSessionId = await createPeerFixtureSession(page.page, affectedWorker);
      await waitForDirectRoute(page.page, affectedSessionId);
      const peerFaults = stack.peerFaults;
      if (!peerFaults) throw new Error("source peer fault controls were unavailable");

      await peerFaults.injectMalformedDirectPacket(affectedWorker.label, kind);
      await waitForSyncRoute(page.page, affectedSessionId);
      await navigateToSmokeSession(page.page, healthySessionId);
      await waitForDirectRoute(page.page, healthySessionId);
      const healthyKey = await sendTrustedPeerKey(page.page, healthySessionId);
      await waitForPainted(page.page, healthySessionId, healthyKey.marker);
    } finally {
      await stopPeerPacketFaultScenario(stack, page, testInfo.status !== testInfo.expectedStatus, testInfo);
    }
  }
}

/** A paused history lane cannot starve direct control, and resumes only when its lane is released. */
export async function verifyPausedHistoryDoesNotStarveControl(browser: Browser, testInfo: TestInfo): Promise<void> {
  const stack = await startTerminalTestStack(PEER_PACKET_FAULT_STACK_OPTIONS);
  let page: EnrolledPage | undefined;
  let unblockCoordinatorHistory: (() => Promise<void>) | undefined;
  try {
    const fixtureWorker = await stack.startPtyFixtureWorker();
    page = await openPeerSmokePage(browser, stack);
    const sessionId = await createPeerFixtureSession(page.page, fixtureWorker);
    await waitForDirectRoute(page.page, sessionId);
    const peerFaults = stack.peerFaults;
    if (!peerFaults) throw new Error("source peer fault controls were unavailable");

    const prefix = `PAUSED-HISTORY-${crypto.randomUUID().replaceAll("-", "")}-`;
    await inputSmokeTerminal(page.page, sessionId, encodePtyFixtureCommand({
      op: "FLOOD",
      prefix,
      count: 40_000,
    }));
    await waitForPainted(page.page, sessionId, `${prefix}40000`);
    await waitForStableCellFrames(page.page, sessionId);
    const beforeBackfills = await page.page.evaluate(
      (id) => window.__smoke.scrollbackBackfillRequestCount(id),
      sessionId,
    );
    const directHistoryResponsesBefore = await page.page.evaluate(
      (id) => window.__smoke.directHistoryResponseCount(id),
      sessionId,
    );
    await page.page.evaluate(() => window.__smoke.pauseSyncTransport());
    unblockCoordinatorHistory = await blockCoordinatorHistoryFallback(page.page);
    await peerFaults.setDirectHistoryPaused(fixtureWorker.label, true);
    const scrollBox = await page.page.getByTestId(`terminal-slot-${sessionId}`).locator(".wterm").boundingBox();
    if (!scrollBox) throw new Error("paused history terminal scroll container was unavailable");
    await page.page.mouse.move(scrollBox.x + scrollBox.width / 2, scrollBox.y + scrollBox.height / 2);
    await page.page.mouse.wheel(0, -100_000);
    await expect.poll(() => page!.page.evaluate(
      (id) => window.__smoke.scrollbackBackfillRequestCount(id),
      sessionId,
    ), { timeout: 30_000, intervals: [50, 100, 250] }).toBeGreaterThan(beforeBackfills);
    expect(await page.page.evaluate(
      (id) => window.__smoke.directHistoryResponseCount(id),
      sessionId,
    )).toBe(directHistoryResponsesBefore);
    await page.page.mouse.wheel(0, 100_000);
    await expect.poll(() => page!.page.evaluate(
      (id) => window.__smoke.renderProbe(id).atBottom,
      sessionId,
    ), { timeout: 10_000, intervals: [50, 100, 250] }).toBe(true);

    const key = await sendTrustedPeerKey(page.page, sessionId);
    await waitForPainted(page.page, sessionId, key.marker);
    await peerFaults.setDirectHistoryPaused(fixtureWorker.label, false);
    await page.page.mouse.wheel(0, -100_000);
    await expect.poll(() => page!.page.evaluate(
      (id) => window.__smoke.directHistoryResponseCount(id),
      sessionId,
    ), { timeout: 60_000, intervals: [100, 250, 500] }).toBeGreaterThan(directHistoryResponsesBefore);
  } finally {
    await unblockCoordinatorHistory?.().catch(() => undefined);
    await stopPeerPacketFaultScenario(stack, page, testInfo.status !== testInfo.expectedStatus, testInfo);
  }
}
