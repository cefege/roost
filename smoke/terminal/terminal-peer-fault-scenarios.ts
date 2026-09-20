// Direct-peer loss scenarios use only stack-owned source-worker fault controls.
// They prove fallback and ambiguity through real browser routing, painted PTY
// output, and route diagnostics; no scenario manufactures a terminal response.
// terminal-peer-failover.spec.ts owns the Playwright test registration.

import type { Browser, TestInfo } from "@playwright/test";
import { expect } from "./fixtures.ts";
import type { EnrolledPage } from "./terminal-local-fast-path-helpers.ts";
import { attachStackLogs } from "./terminal-local-fast-path-helpers.ts";
import {
  armPeerFixtureKey,
  beginPeerSmokeInput,
  expectNoPeerFixtureAck,
  settlePeerSmokeInput,
  waitForPeerRouteLoss,
} from "./terminal-peer-fault-helpers.ts";
import {
  createPeerFixtureSession,
  openPeerSmokePage,
  readPeerRoute,
  sendTrustedPeerKey,
  waitForDirectRoute,
  waitForSyncRoute,
  waitForTerminalInputReady,
} from "./terminal-peer-helpers.ts";
import { encodePtyFixtureCommand } from "./pty-fixture-protocol.ts";
import { inputSmokeTerminal, waitForStableCellFrames } from "./terminal-helpers.ts";
import {
  expectHistoryAnchorPreserved,
  expectMarkersOnce,
  waitForHistoryAnchor,
  waitForPainted,
} from "./terminal-multiview-helpers.ts";
import type { TerminalTestStack } from "./stack.ts";
import { startTerminalTestStack } from "./stack.ts";

const PEER_FAULT_STACK_OPTIONS = {
  terminalPeer: {
    coordinatorEnabled: true,
    coordinatorStunUrls: [],
    workerEnabled: true,
    disableLoopbackProbe: true,
    enableFaults: true,
  },
} as const;

async function stopPeerFaultScenario(
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

/** Drops only worker→browser peer packets so two real missed probes force Sync repair. */
export async function verifyPeerBlackholeFallback(browser: Browser, testInfo: TestInfo): Promise<void> {
  const stack = await startTerminalTestStack(PEER_FAULT_STACK_OPTIONS);
  let page: EnrolledPage | undefined;
  try {
    const fixtureWorker = await stack.startPtyFixtureWorker();
    page = await openPeerSmokePage(browser, stack);
    const sessionId = await createPeerFixtureSession(page.page, fixtureWorker);
    await waitForDirectRoute(page.page, sessionId);

    const suffix = crypto.randomUUID().replaceAll("-", "").slice(0, 8);
    const historyPrefix = `PEER-HISTORY-${suffix}-`;
    await inputSmokeTerminal(page.page, sessionId, encodePtyFixtureCommand({
      op: "FLOOD",
      prefix: historyPrefix,
      count: 2_500,
    }));
    await waitForPainted(page.page, sessionId, `${historyPrefix}2500`);
    await waitForStableCellFrames(page.page, sessionId);
    const slot = page.page.getByTestId(`terminal-slot-${sessionId}`);
    const box = await slot.boundingBox();
    if (!box) throw new Error("peer history terminal did not expose a painted box");
    await page.page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
    await page.page.mouse.wheel(0, -2_400);
    const anchor = await waitForHistoryAnchor(page.page, sessionId, historyPrefix);

    const peerFaults = stack.peerFaults;
    if (!peerFaults) throw new Error("source peer fault controls were unavailable");
    await peerFaults.setPeerPacketBlackhole(fixtureWorker.label, true);
    await waitForStableCellFrames(page.page, sessionId);
    const beforeFulls = await page.page.evaluate((id) => window.__smoke.cellFullFrameCount(id), sessionId);
    await waitForSyncRoute(page.page, sessionId);
    await waitForTerminalInputReady(page.page, sessionId);
    await expect.poll(() => page!.page.evaluate(
      (id) => window.__smoke.cellFullFrameCount(id),
      sessionId,
    ), { timeout: 60_000, intervals: [100, 250, 500] }).toBeGreaterThan(beforeFulls);
    expect(await page.page.evaluate(
      (id) => window.__smoke.cellFullFrameCount(id),
      sessionId,
    )).toBe(beforeFulls + 1);
    await expectHistoryAnchorPreserved(page.page, sessionId, anchor);
    await page.page.mouse.wheel(0, 100_000);
    await expect.poll(() => page!.page.evaluate(
      (id) => window.__smoke.renderProbe(id).atBottom,
      sessionId,
    ), { timeout: 10_000, intervals: [50, 100, 250] }).toBe(true);

    const fallbackKey = await sendTrustedPeerKey(page.page, sessionId);
    await expectMarkersOnce(page.page, sessionId, [fallbackKey.marker]);
    expect((await readPeerRoute(page.page, sessionId)).activeKind).toBe("sync");
  } finally {
    await stopPeerFaultScenario(stack, page, testInfo.status !== testInfo.expectedStatus, testInfo);
  }
}

/** Drops one real peer input result after its PTY write and proves fallback never resends it. */
export async function verifyDroppedPeerResultIsAmbiguous(browser: Browser, testInfo: TestInfo): Promise<void> {
  const stack = await startTerminalTestStack(PEER_FAULT_STACK_OPTIONS);
  let page: EnrolledPage | undefined;
  try {
    const fixtureWorker = await stack.startPtyFixtureWorker();
    page = await openPeerSmokePage(browser, stack);
    const sessionId = await createPeerFixtureSession(page.page, fixtureWorker);
    await waitForDirectRoute(page.page, sessionId);
    const peerFaults = stack.peerFaults;
    if (!peerFaults) throw new Error("source peer fault controls were unavailable");

    const ackMarker = await armPeerFixtureKey(page.page, sessionId, crypto.randomUUID());
    await page.page.evaluate(() => window.__smoke.resetTerminalInputCapture());
    await peerFaults.dropNextDirectInputResult(fixtureWorker.label);
    await beginPeerSmokeInput(page.page, sessionId, "x");
    await waitForPainted(page.page, sessionId, ackMarker);
    const outcome = await settlePeerSmokeInput(page.page);
    expect(outcome.reason).toBe("input result timed out; the batch will not be retried");
    await expectMarkersOnce(page.page, sessionId, [ackMarker]);

    await peerFaults.setPeerPacketBlackhole(fixtureWorker.label, true);
    await waitForSyncRoute(page.page, sessionId);
    await expectMarkersOnce(page.page, sessionId, [ackMarker]);
    const capture = await page.page.evaluate(() => window.__smoke.terminalInputCapture());
    expect(capture.batches).toHaveLength(1);
    expect(capture.batches[0]).toMatchObject({ sessionId, data: [120] });
    await expect(page.page.evaluate(
      (id) => window.__smoke.input(id, "x"),
      sessionId,
    )).rejects.toThrow("terminal input route is reconnecting");
  } finally {
    await stopPeerFaultScenario(stack, page, testInfo.status !== testInfo.expectedStatus, testInfo);
  }
}

/** Holds one authenticated peer input, then lets Sync claim ownership before releasing the stale bytes. */
export async function verifyPeerToSyncInputFence(browser: Browser, testInfo: TestInfo): Promise<void> {
  const stack = await startTerminalTestStack(PEER_FAULT_STACK_OPTIONS);
  let page: EnrolledPage | undefined;
  try {
    const fixtureWorker = await stack.startPtyFixtureWorker();
    page = await openPeerSmokePage(browser, stack);
    const sessionId = await createPeerFixtureSession(page.page, fixtureWorker);
    await waitForDirectRoute(page.page, sessionId);
    const peerFaults = stack.peerFaults;
    if (!peerFaults) throw new Error("source peer fault controls were unavailable");

    const oldAckMarker = await armPeerFixtureKey(page.page, sessionId, crypto.randomUUID());
    const heldInput = peerFaults.holdNextDirectInput(sessionId);
    await beginPeerSmokeInput(page.page, sessionId, "x");
    const held = await heldInput;
    expect(held.inputSeq).toBeGreaterThan(0n);
    await peerFaults.setPeerPacketBlackhole(fixtureWorker.label, true);
    await waitForSyncRoute(page.page, sessionId);
    held.release();
    await settlePeerSmokeInput(page.page);
    await expectNoPeerFixtureAck(page.page, sessionId, oldAckMarker);
    await waitForPeerRouteLoss(page.page, sessionId);

    await expect(page.page.evaluate(
      (id) => window.__smoke.input(id, "x"),
      sessionId,
    )).rejects.toThrow("terminal input route is reconnecting");
  } finally {
    await stopPeerFaultScenario(stack, page, testInfo.status !== testInfo.expectedStatus, testInfo);
  }
}

/** Advances the worker's injected grant clock while coord is unreachable, then requires a fresh direct grant. */
export async function verifyDirectGrantExpiry(browser: Browser, testInfo: TestInfo): Promise<void> {
  const stack = await startTerminalTestStack(PEER_FAULT_STACK_OPTIONS);
  let page: EnrolledPage | undefined;
  try {
    const fixtureWorker = await stack.startPtyFixtureWorker();
    page = await openPeerSmokePage(browser, stack);
    const sessionId = await createPeerFixtureSession(page.page, fixtureWorker);
    const beforeExpiry = await waitForDirectRoute(page.page, sessionId);
    const peerFaults = stack.peerFaults;
    if (!peerFaults) throw new Error("source peer fault controls were unavailable");

    await stack.stopCoordinator();
    await peerFaults.advanceGrantClock(fixtureWorker.label, 12 * 60 * 60 * 1_000 + 1);
    await waitForPeerRouteLoss(page.page, sessionId);
    expect((await readPeerRoute(page.page, sessionId)).activeKind).not.toBe("webrtc");

    await stack.startCoordinator();
    const afterExpiry = await waitForDirectRoute(page.page, sessionId);
    expect(afterExpiry.activeWorkerEpoch).toBe(beforeExpiry.activeWorkerEpoch);
    expect(afterExpiry.activePeerId).not.toBe(beforeExpiry.activePeerId);
    const key = await sendTrustedPeerKey(page.page, sessionId);
    await expectMarkersOnce(page.page, sessionId, [key.marker]);
  } finally {
    await stopPeerFaultScenario(stack, page, testInfo.status !== testInfo.expectedStatus, testInfo);
  }
}

/** Drops the retirement frame intentionally, then uses grant expiry rather than pretending outage revokes a peer. */
export async function verifyDroppedRetirementExpires(browser: Browser, testInfo: TestInfo): Promise<void> {
  const stack = await startTerminalTestStack(PEER_FAULT_STACK_OPTIONS);
  let page: EnrolledPage | undefined;
  try {
    const fixtureWorker = await stack.startPtyFixtureWorker();
    page = await openPeerSmokePage(browser, stack);
    const sessionId = await createPeerFixtureSession(page.page, fixtureWorker);
    await waitForDirectRoute(page.page, sessionId);
    const peerFaults = stack.peerFaults;
    if (!peerFaults) throw new Error("source peer fault controls were unavailable");

    const oldAckMarker = await armPeerFixtureKey(page.page, sessionId, crypto.randomUUID());
    const heldInput = peerFaults.holdNextDirectInput(sessionId);
    await beginPeerSmokeInput(page.page, sessionId, "x");
    const held = await heldInput;
    await peerFaults.dropNextDirectRetire(fixtureWorker.label);
    const deleted = await stack.client.workersDelete({ fp: fixtureWorker.workerFp });
    expect(deleted.ok).toBe(true);
    await peerFaults.advanceGrantClock(fixtureWorker.label, 12 * 60 * 60 * 1_000 + 1);
    held.release();
    await settlePeerSmokeInput(page.page);
    await waitForPeerRouteLoss(page.page, sessionId);
    await expectNoPeerFixtureAck(page.page, sessionId, oldAckMarker);
  } finally {
    await stopPeerFaultScenario(stack, page, testInfo.status !== testInfo.expectedStatus, testInfo);
  }
}
