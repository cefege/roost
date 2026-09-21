// Real-stack terminal peer continuity and replacement tests.
// These cases keep a real keeper PTY alive while coordinator or worker links
// change, then require a painted marker and exact direct route identity rather
// than accepting a socket-open or forwarding-mock signal as evidence.

import type { Page, TestInfo } from "@playwright/test";
import { test, expect } from "./fixtures.ts";
import type { EnrolledPage } from "./terminal-local-fast-path-helpers.ts";
import {
  attachStackLogs,
  waitForKeeperChannels,
  waitForKeeperRowAfter,
} from "./terminal-local-fast-path-helpers.ts";
import {
  browserDeviceFingerprint,
  createPeerFixtureSession,
  openPeerSmokePage,
  peerRouteIdentity,
  readPeerRoute,
  sendTrustedPeerKey,
  waitForDirectRoute,
  waitForSyncRoute,
} from "./terminal-peer-helpers.ts";
import {
  armPeerFixtureKey,
  beginPeerSmokeInput,
  expectNoPeerFixtureAck,
  settlePeerSmokeInput,
  waitForPeerRouteLoss,
} from "./terminal-peer-fault-helpers.ts";
import {
  verifyDirectGrantExpiry,
  verifyDroppedPeerResultIsAmbiguous,
  verifyDroppedRetirementExpires,
  verifyPeerBlackholeFallback,
  verifyPeerToSyncInputFence,
} from "./terminal-peer-fault-scenarios.ts";
import { verifyPeerHistoryContinuity } from "./terminal-peer-history-scenarios.ts";
import {
  verifyGrantShrinkFencesInputAndHistory,
  verifyPeerInputBudgetFlood,
} from "./terminal-peer-limits-scenarios.ts";
import {
  verifyMalformedPeerPacketIsolation,
  verifyPausedHistoryDoesNotStarveControl,
} from "./terminal-peer-packet-fault-scenarios.ts";
import { navigateToSmokeSession, spawnSmokeShell } from "./terminal-helpers.ts";
import { expectMarkersOnce, waitForPainted } from "./terminal-multiview-helpers.ts";
import type { RecoverySmokeApi } from "./terminal-smoke-api.ts";
import { startTerminalTestStack, type TerminalTestStack } from "./stack.ts";
import { expectTerminalTransportIndicator } from "./terminal-transport-indicator-helpers.ts";

const PEER_STACK_OPTIONS = {
  terminalPeer: {
    coordinatorEnabled: true,
    coordinatorStunUrls: [],
    workerEnabled: true,
    disableLoopbackProbe: true,
  },
} as const;

const PEER_FAULT_STACK_OPTIONS = {
  ...PEER_STACK_OPTIONS,
  terminalPeer: { ...PEER_STACK_OPTIONS.terminalPeer, enableFaults: true },
} as const;

async function stopPeerStack(
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

async function sendTrustedShellMarker(page: Page, sessionId: string, marker: string): Promise<void> {
  await page.getByTestId(`terminal-slot-${sessionId}`).click();
  await expect.poll(() => page.evaluate((id) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return smokeWindow.__smoke.paneFocused(id).focused;
  }, sessionId), { timeout: 10_000, intervals: [50, 100] }).toBe(true);
  const timingId = await page.evaluate((id) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return smokeWindow.__smoke.beginTerminalTiming("trusted_key", id);
  }, sessionId);
  const splitAt = Math.floor(marker.length / 2);
  await page.keyboard.type(
    `printf '%s\\n' '${marker.slice(0, splitAt)}''${marker.slice(splitAt)}'`,
  );
  await page.keyboard.press("Enter");
  const timing = await page.evaluate(({ id, timingId: currentTiming, expected }) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return smokeWindow.__smoke.finishTerminalTiming(currentTiming, id, expected, 30_000);
  }, { id: sessionId, timingId, expected: marker });
  expect(timing.trustedKey).toBe(true);
}

test("WebRTC continues terminal paint through a coordinator outage without direct token churn", async ({ browser }, testInfo) => {
  test.setTimeout(240_000);
  const stack = await startTerminalTestStack(PEER_STACK_OPTIONS);
  let page: EnrolledPage | undefined;
  try {
    const fixtureWorker = await stack.startPtyFixtureWorker();
    page = await openPeerSmokePage(browser, stack);
    const sessionId = await createPeerFixtureSession(page.page, fixtureWorker);
    const beforeOutage = await waitForDirectRoute(page.page, sessionId);
    const keeperBefore = await waitForKeeperChannels(stack, fixtureWorker.workerFp, 1);

    await stack.stopCoordinator();
    await expect(page.page.getByTestId("connection-banner")).toHaveAttribute(
      "data-banner-reason",
      "coord-unreachable-direct-live",
      { timeout: 60_000 },
    );
    await expect(page.page.getByTestId("connection-banner")).toContainText(
      "Coordinator unreachable — direct terminals may remain available; fleet controls unavailable",
    );
    const duringOutage = await waitForDirectRoute(page.page, sessionId, "webrtc", { syncMetadata: false });
    await expectTerminalTransportIndicator(page.page, sessionId, "webrtc");
    expect(peerRouteIdentity(duringOutage)).toBe(peerRouteIdentity(beforeOutage));
    const key = await sendTrustedPeerKey(page.page, sessionId);
    await waitForPainted(page.page, sessionId, key.marker);

    await stack.startCoordinator();
    const afterRecovery = await waitForDirectRoute(page.page, sessionId);
    expect(peerRouteIdentity(afterRecovery)).toBe(peerRouteIdentity(beforeOutage));
    const keeperAfter = await waitForKeeperRowAfter(stack, fixtureWorker.workerFp, keeperBefore.lastSeenMs);
    expect(keeperAfter.runtime).toMatchObject({
      channel_count: keeperBefore.runtime.channel_count,
      keeper_epoch: keeperBefore.runtime.keeper_epoch,
      binding_digest: keeperBefore.runtime.binding_digest,
    });
  } finally {
    try {
      await stack.startCoordinator();
    } finally {
      await stopPeerStack(stack, page, testInfo);
    }
  }
});

test("worker restart retires the old peer epoch while its keeper PTY survives", async ({ browser }, testInfo) => {
  test.setTimeout(300_000);
  const stack = await startTerminalTestStack(PEER_STACK_OPTIONS);
  let page: EnrolledPage | undefined;
  try {
    page = await openPeerSmokePage(browser, stack);
    const sessionId = (await spawnSmokeShell(page.page, stack.workerFp)).session_id;
    await navigateToSmokeSession(page.page, sessionId);
    const beforeRestart = await waitForDirectRoute(page.page, sessionId);
    const keeperBefore = await waitForKeeperChannels(stack, stack.workerFp, 1);
    const beforeMarker = `PEER-BEFORE-${crypto.randomUUID()}`;
    await sendTrustedShellMarker(page.page, sessionId, beforeMarker);

    await stack.restartWorker();
    const afterRestart = await waitForDirectRoute(page.page, sessionId);
    expect(afterRestart.activeWorkerEpoch).not.toBe(beforeRestart.activeWorkerEpoch);
    expect(afterRestart.activePeerId).not.toBe(beforeRestart.activePeerId);
    expect(afterRestart.proofSocketId).not.toBe(beforeRestart.proofSocketId);

    const afterMarker = `PEER-AFTER-${crypto.randomUUID()}`;
    await sendTrustedShellMarker(page.page, sessionId, afterMarker);
    const text = await page.page.evaluate((id) => {
      const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
      return smokeWindow.__smoke.viewportText(id);
    }, sessionId);
    expect(text.split(beforeMarker).length - 1).toBe(1);
    expect(text.split(afterMarker).length - 1).toBe(1);

    const keeperAfter = await waitForKeeperRowAfter(stack, stack.workerFp, keeperBefore.lastSeenMs);
    expect(keeperAfter.runtime).toMatchObject({
      channel_count: keeperBefore.runtime.channel_count,
      keeper_epoch: keeperBefore.runtime.keeper_epoch,
      binding_digest: keeperBefore.runtime.binding_digest,
    });
    const route = await readPeerRoute(page.page, sessionId);
    expect(route).toMatchObject({ activeKind: "webrtc", proofKind: "webrtc", syncReady: true });
  } finally {
    await stopPeerStack(stack, page, testInfo);
  }
});

test("worker deletion retires direct authority before a held authenticated input reaches the PTY", async ({ browser }, testInfo) => {
  test.setTimeout(240_000);
  const stack = await startTerminalTestStack(PEER_FAULT_STACK_OPTIONS);
  let page: EnrolledPage | undefined;
  try {
    const fixtureWorker = await stack.startPtyFixtureWorker();
    page = await openPeerSmokePage(browser, stack);
    const sessionId = await createPeerFixtureSession(page.page, fixtureWorker);
    await waitForDirectRoute(page.page, sessionId);
    const peerFaults = stack.peerFaults;
    if (!peerFaults) throw new Error("source peer fault controls were unavailable");

    const nonce = crypto.randomUUID();
    const ackMarker = await armPeerFixtureKey(page.page, sessionId, nonce);
    await page.page.evaluate(() => window.__smoke.resetTerminalInputCapture());
    const heldInput = peerFaults.holdNextDirectInput(sessionId);
    await beginPeerSmokeInput(page.page, sessionId, "x");
    const held = await heldInput;
    expect(held.inputSeq).toBeGreaterThan(0n);

    const deleted = await stack.client.workersDelete({ fp: fixtureWorker.workerFp });
    expect(deleted.ok).toBe(true);
    held.release();
    const outcome = await settlePeerSmokeInput(page.page);
    expect(outcome.reason ?? "").toMatch(/terminal input route changed|terminal session is unavailable|not be retried/u);
    await waitForPeerRouteLoss(page.page, sessionId);
    await expectNoPeerFixtureAck(page.page, sessionId, ackMarker);
    const capture = await page.page.evaluate(() => window.__smoke.terminalInputCapture());
    expect(capture.batches).toHaveLength(1);
    expect(capture.batches[0]).toMatchObject({ sessionId, data: [120] });
  } finally {
    await stopPeerStack(stack, page, testInfo);
  }
});

test("device revocation closes the direct peer before a held input can mutate its PTY", async ({ browser }, testInfo) => {
  test.setTimeout(240_000);
  const stack = await startTerminalTestStack(PEER_FAULT_STACK_OPTIONS);
  let page: EnrolledPage | undefined;
  try {
    const fixtureWorker = await stack.startPtyFixtureWorker();
    page = await openPeerSmokePage(browser, stack);
    const sessionId = await createPeerFixtureSession(page.page, fixtureWorker);
    await waitForDirectRoute(page.page, sessionId);
    const peerFaults = stack.peerFaults;
    if (!peerFaults) throw new Error("source peer fault controls were unavailable");
    const browserFingerprint = await browserDeviceFingerprint(page.page, stack);

    const nonce = crypto.randomUUID();
    const ackMarker = await armPeerFixtureKey(page.page, sessionId, nonce);
    const heldInput = peerFaults.holdNextDirectInput(sessionId);
    await beginPeerSmokeInput(page.page, sessionId, "x");
    const held = await heldInput;
    expect(held.inputSeq).toBeGreaterThan(0n);
    const revoked = await stack.client.devicesRevoke({ fingerprint: browserFingerprint });
    expect(revoked.ok).toBe(true);
    held.release();
    const outcome = await settlePeerSmokeInput(page.page);
    expect(outcome.reason ?? "").toMatch(/terminal input route changed|terminal session is unavailable|not be retried/u);
    await waitForPeerRouteLoss(page.page, sessionId);
    await expectNoPeerFixtureAck(page.page, sessionId, ackMarker);
  } finally {
    await stopPeerStack(stack, page, testInfo);
  }
});

test("a healthy Sync input drains before WebRTC promotion and fresh direct input stays exactly once", async ({ browser }, testInfo) => {
  test.setTimeout(300_000);
  const stack = await startTerminalTestStack(PEER_STACK_OPTIONS);
  let page: EnrolledPage | undefined;
  try {
    const fixtureWorker = await stack.startPtyFixtureWorker({ workerLinkOneWayDelayMs: 200 });
    const delayedLink = stack.ptyFixtureWorkerLink;
    if (!delayedLink) throw new Error("fixture worker did not expose its delayed coordinator link");
    page = await openPeerSmokePage(browser, stack, { holdRtcAnswer: true });
    const sessionId = await createPeerFixtureSession(page.page, fixtureWorker);
    await waitForSyncRoute(page.page, sessionId);
    await expect.poll(async () => (await readPeerRoute(page!.page, sessionId)).peerPhase, {
      timeout: 30_000,
      intervals: [50, 100, 250],
    }).toBe("authenticating");
    const oldAckMarker = await armPeerFixtureKey(page.page, sessionId, crypto.randomUUID());
    const heldInput = delayedLink.holdNextInput(sessionId);
    await beginPeerSmokeInput(page.page, sessionId, "x");
    const held = await heldInput;
    expect(held.requestId).not.toBe("");
    await page.page.evaluate(() => {
      window.__releaseTerminalPeerAnswer?.();
      delete window.__releaseTerminalPeerAnswer;
    });
    await expect.poll(async () => {
      const route = await readPeerRoute(page!.page, sessionId);
      return route.activeKind === "sync"
        && route.candidateKind === "webrtc"
        && route.inputPhase === "holding";
    }, { timeout: 30_000, intervals: [50, 100, 250] }).toBe(true);
    await expectTerminalTransportIndicator(page.page, sessionId, "sync");
    held.release();
    const oldOutcome = await settlePeerSmokeInput(page.page);
    expect(oldOutcome).toEqual({ status: "accepted", reason: null });
    await waitForPainted(page.page, sessionId, oldAckMarker);
    await expectMarkersOnce(page.page, sessionId, [oldAckMarker]);
    await waitForDirectRoute(page.page, sessionId);
    await expectTerminalTransportIndicator(page.page, sessionId, "webrtc");
    const directKey = await sendTrustedPeerKey(page.page, sessionId);
    await expectMarkersOnce(page.page, sessionId, [oldAckMarker, directKey.marker]);
  } finally {
    await stopPeerStack(stack, page, testInfo);
  }
});

test("a delayed old Sync input is fenced after peer promotion and cannot reach the PTY", async ({ browser }, testInfo) => {
  test.setTimeout(300_000);
  const stack = await startTerminalTestStack(PEER_FAULT_STACK_OPTIONS);
  let page: EnrolledPage | undefined;
  try {
    const fixtureWorker = await stack.startPtyFixtureWorker({ workerLinkOneWayDelayMs: 200 });
    const delayedLink = stack.ptyFixtureWorkerLink;
    if (!delayedLink) throw new Error("fixture worker did not expose its delayed coordinator link");
    page = await openPeerSmokePage(browser, stack, { holdRtcAnswer: true });
    const sessionId = await createPeerFixtureSession(page.page, fixtureWorker);
    await waitForSyncRoute(page.page, sessionId);
    await expect.poll(async () => (await readPeerRoute(page!.page, sessionId)).peerPhase, {
      timeout: 30_000,
      intervals: [50, 100, 250],
    }).toBe("authenticating");
    const oldAckMarker = await armPeerFixtureKey(page.page, sessionId, crypto.randomUUID());
    await page.page.evaluate(() => window.__smoke.resetTerminalInputCapture());
    const heldInput = delayedLink.holdNextInput(sessionId);
    await beginPeerSmokeInput(page.page, sessionId, "x");
    const held = await heldInput;
    expect(held.requestId).not.toBe("");
    await page.page.evaluate(() => window.__smoke.pauseSyncTransport());
    await page.page.evaluate(() => {
      window.__releaseTerminalPeerAnswer?.();
      delete window.__releaseTerminalPeerAnswer;
    });
    await waitForDirectRoute(page.page, sessionId, "webrtc", { syncMetadata: false });
    held.release();
    await settlePeerSmokeInput(page.page);
    await expectNoPeerFixtureAck(page.page, sessionId, oldAckMarker);
    const capture = await page.page.evaluate(() => window.__smoke.terminalInputCapture());
    expect(capture.batches).toHaveLength(1);
    expect(capture.batches[0]).toMatchObject({ sessionId, data: [120] });
    const directKey = await sendTrustedPeerKey(page.page, sessionId);
    await expectMarkersOnce(page.page, sessionId, [directKey.marker]);
    expect((await readPeerRoute(page.page, sessionId)).activeKind).toBe("webrtc");
  } finally {
    await stopPeerStack(stack, page, testInfo);
  }
});

test("missed direct peer probes fall back through one repaired baseline while preserving history", async ({ browser }, testInfo) => {
  test.setTimeout(300_000);
  await verifyPeerBlackholeFallback(browser, testInfo);
});

for (const carrier of ["sync", "loopback", "webrtc"] as const) {
  test(`${carrier} preserves 2,500 retained history rows through resize, demand, and tab cycles`, async ({
    browser,
    browserName,
  }, testInfo) => {
    test.skip(browserName === "firefox", "scroll-wheel backfill demand is covered by Chromium");
    test.setTimeout(480_000);
    await verifyPeerHistoryContinuity(browser, testInfo, carrier);
  });
}

test("a dropped direct input result is ambiguous and fallback never replays the PTY write", async ({ browser }, testInfo) => {
  test.setTimeout(240_000);
  await verifyDroppedPeerResultIsAmbiguous(browser, testInfo);
});

test("a held peer input is fenced before replacement Sync ownership settles", async ({ browser }, testInfo) => {
  test.setTimeout(240_000);
  await verifyPeerToSyncInputFence(browser, testInfo);
});

test("active direct grant expiry closes the peer during coordinator loss and a renewed route stays usable", async ({ browser }, testInfo) => {
  test.setTimeout(300_000);
  await verifyDirectGrantExpiry(browser, testInfo);
});

test("a deliberately dropped retirement frame relies on grant expiry before stale direct input can write", async ({ browser }, testInfo) => {
  test.setTimeout(240_000);
  await verifyDroppedRetirementExpires(browser, testInfo);
});

test("grant scope shrink fences queued direct input and history while retained PTYs remain usable", async ({ browser }, testInfo) => {
  test.setTimeout(300_000);
  await verifyGrantShrinkFencesInputAndHistory(browser, testInfo);
});

test("a held keeper admission bounds valid direct input flood work before any excess PTY mutation", async ({ browser }, testInfo) => {
  test.setTimeout(300_000);
  await verifyPeerInputBudgetFlood(browser, testInfo);
});

test("malformed direct packet headers close only their peer while another worker remains direct", async ({ browser }, testInfo) => {
  test.setTimeout(480_000);
  await verifyMalformedPeerPacketIsolation(browser, testInfo);
});

test("a paused direct history lane cannot delay trusted control input", async ({ browser }, testInfo) => {
  test.setTimeout(240_000);
  await verifyPausedHistoryDoesNotStarveControl(browser, testInfo);
});
