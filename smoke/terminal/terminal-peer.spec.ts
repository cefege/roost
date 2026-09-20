// Real-stack correctness for terminal direct carriers.
// Each case starts a coordinator, real workers and keepers, then observes the
// elected browser route, painted fixture output, and actual trusted-key input.
// It deliberately never treats a mock forwarding callback as transport proof.

import type { TestInfo } from "@playwright/test";
import { test, expect } from "./fixtures.ts";
import { startTerminalTestStack, type TerminalTestStack } from "./stack.ts";
import {
  createPeerFixtureSession,
  createPeerFixtureSessionInDocument,
  openPeerSmokePage,
  peerRouteIdentity,
  readPeerRoute,
  sendTrustedPeerKey,
  waitForDirectRoute,
  waitForSyncRoute,
  waitForPeerFallback,
  waitForHostPeerTelemetry,
} from "./terminal-peer-helpers.ts";
import {
  attachStackLogs,
  type EnrolledPage,
} from "./terminal-local-fast-path-helpers.ts";
import { coordinatorConstrainedGeometry } from "./terminal-probe-viewer-inputs.ts";
import { navigateToSmokeSession, spawnPtyFixtureSession } from "./terminal-helpers.ts";
import { expectMarkersOnce, forceVisible, waitForPainted, waitForTransition } from "./terminal-multiview-helpers.ts";
import { PTY_FIXTURE_READY } from "./pty-fixture-protocol.ts";
import { verifyLargeDirectPacketAndHistory } from "./terminal-peer-packet-scenarios.ts";

const PEER_STACK_OPTIONS = {
  terminalPeer: {
    coordinatorEnabled: true,
    coordinatorStunUrls: [],
    workerEnabled: true,
  },
} as const;

async function stopPeerStack(
  stack: TerminalTestStack,
  pages: Array<{ close(): Promise<void> } | undefined>,
  testInfo: TestInfo,
): Promise<void> {
  try {
    await attachStackLogs(testInfo, stack);
  } finally {
    try {
      await Promise.all(pages.filter((page): page is { close(): Promise<void> } => page !== undefined)
        .map((page) => page.close()));
    } finally {
      await stack.stop();
    }
  }
}

test("loopback wins before a WebRTC peer is allocated and keeps Sync metadata live", async ({ browser }, testInfo) => {
  test.setTimeout(180_000);
  const stack = await startTerminalTestStack(PEER_STACK_OPTIONS);
  let localPage: EnrolledPage | undefined;
  try {
    const fixtureWorker = await stack.startPtyFixtureWorker();
    localPage = await openPeerSmokePage(browser, stack, {
      origin: stack.localUiUrl(fixtureWorker.workerFp),
    });
    const sessionId = await createPeerFixtureSession(localPage.page, fixtureWorker);
    const route = await waitForDirectRoute(localPage.page, sessionId, "loopback");

    expect(route).toMatchObject({
      activeKind: "loopback",
      proofKind: "loopback",
      candidateKind: null,
      activePeerId: null,
      syncReady: true,
    });
    await expect(localPage.page.getByTestId(`tab-${sessionId}`))
      .toHaveAttribute("data-terminal-transport", "loopback");

    const key = await sendTrustedPeerKey(localPage.page, sessionId);
    await expectMarkersOnce(localPage.page, sessionId, [key.marker]);
    const afterKey = await readPeerRoute(localPage.page, sessionId);
    expect(peerRouteIdentity(afterKey)).toBe(peerRouteIdentity(route));
    expect(afterKey.candidateKind).toBeNull();
  } finally {
    await stopPeerStack(stack, [localPage], testInfo);
  }
});

test("host-candidate WebRTC multiplexes each worker and preserves crossed browser geometry", async ({ browser }, testInfo) => {
  test.setTimeout(300_000);
  const stack = await startTerminalTestStack({
    ...PEER_STACK_OPTIONS,
    terminalPeer: { ...PEER_STACK_OPTIONS.terminalPeer, disableLoopbackProbe: true },
  });
  let firstPage: EnrolledPage | undefined;
  let secondPage: EnrolledPage | undefined;
  try {
    const firstWorker = await stack.startPtyFixtureWorker();
    const secondWorker = await stack.startSecondPtyFixtureWorker();
    firstPage = await openPeerSmokePage(browser, stack);
    secondPage = await openPeerSmokePage(browser, stack);
    await Promise.all([
      firstPage.page.setViewportSize({ width: 1_600, height: 720 }),
      secondPage.page.setViewportSize({ width: 800, height: 1_000 }),
      forceVisible(firstPage.page, true),
      forceVisible(secondPage.page, true),
    ]);

    const firstWorkerSessionOne = await createPeerFixtureSession(firstPage.page, firstWorker);
    const firstWorkerRouteOne = await waitForDirectRoute(firstPage.page, firstWorkerSessionOne);
    await navigateToSmokeSession(secondPage.page, firstWorkerSessionOne);
    const secondBrowserRoute = await waitForDirectRoute(secondPage.page, firstWorkerSessionOne);
    expect(secondBrowserRoute.activePeerId).not.toBe(firstWorkerRouteOne.activePeerId);
    const firstWorkerTelemetry = await waitForHostPeerTelemetry(firstPage.page, firstWorkerSessionOne);
    expect(firstWorkerTelemetry.activeCandidateType).toBe("host");

    const crossedViews = await waitForTransition(
      [firstPage.page, secondPage.page],
      firstWorkerSessionOne,
      { activeIndices: [0, 1], activeViewCount: 2 },
    );
    expect(coordinatorConstrainedGeometry(crossedViews.probes[0]!)).toEqual({
      cols: crossedViews.control.cols,
      rows: crossedViews.control.rows,
    });

    const firstWorkerSessionTwo = await createPeerFixtureSessionInDocument(firstPage.page, firstWorker);
    const firstWorkerRouteTwo = await waitForDirectRoute(firstPage.page, firstWorkerSessionTwo);
    expect(firstWorkerRouteTwo.activePeerId, JSON.stringify({ firstWorkerRouteOne, firstWorkerRouteTwo })).toBe(firstWorkerRouteOne.activePeerId);
    expect(firstWorkerRouteTwo.activeWorkerEpoch).toBe(firstWorkerRouteOne.activeWorkerEpoch);

    const secondWorkerSessionOne = await createPeerFixtureSessionInDocument(firstPage.page, secondWorker);
    const secondWorkerRouteOne = await waitForDirectRoute(firstPage.page, secondWorkerSessionOne);
    expect(secondWorkerRouteOne.activePeerId).not.toBe(firstWorkerRouteOne.activePeerId);
    const secondWorkerSessionTwo = await createPeerFixtureSessionInDocument(firstPage.page, secondWorker);
    const secondWorkerRouteTwo = await waitForDirectRoute(firstPage.page, secondWorkerSessionTwo);
    expect(secondWorkerRouteTwo.activePeerId).toBe(secondWorkerRouteOne.activePeerId);
    expect(secondWorkerRouteTwo.activeWorkerEpoch).toBe(secondWorkerRouteOne.activeWorkerEpoch);

    await navigateToSmokeSession(firstPage.page, firstWorkerSessionOne);
    await waitForDirectRoute(firstPage.page, firstWorkerSessionOne);
    const trusted = await sendTrustedPeerKey(firstPage.page, firstWorkerSessionOne);
    await expectMarkersOnce(firstPage.page, firstWorkerSessionOne, [trusted.marker]);
    await expectMarkersOnce(secondPage.page, firstWorkerSessionOne, [trusted.marker]);
    for (const route of [
      firstWorkerRouteOne,
      firstWorkerRouteTwo,
      secondWorkerRouteOne,
      secondWorkerRouteTwo,
      secondBrowserRoute,
    ]) {
      expect(route).toMatchObject({ activeKind: "webrtc", proofKind: "webrtc", syncReady: true });
      expect(route.activePeerId).not.toBeNull();
      expect(route.activeWorkerEpoch).not.toBeNull();
    }
  } finally {
    await stopPeerStack(
      stack,
      [firstPage, secondPage],
      testInfo,
    );
  }
});

test("an unavailable browser WebRTC API falls back to Sync without a blank terminal", async ({ browser }, testInfo) => {
  test.setTimeout(180_000);
  const stack = await startTerminalTestStack({
    ...PEER_STACK_OPTIONS,
    terminalPeer: { ...PEER_STACK_OPTIONS.terminalPeer, disableLoopbackProbe: true },
  });
  let page: EnrolledPage | undefined;
  try {
    const fixtureWorker = await stack.startPtyFixtureWorker();
    page = await openPeerSmokePage(browser, stack, { rtcUnavailable: true });
    const sessionId = await createPeerFixtureSession(page.page, fixtureWorker);
    const route = await waitForSyncRoute(page.page, sessionId);

    expect(route).toMatchObject({
      activeKind: "sync",
      proofKind: "sync",
      candidateKind: null,
      fallbackReason: "unsupported",
      syncReady: true,
    });
    await expect(page.page.getByTestId(`terminal-slot-${sessionId}`)).toBeVisible();
    await expect(page.page.getByTestId(`tab-${sessionId}`))
      .toHaveAttribute("data-terminal-transport", "sync");
    const trusted = await sendTrustedPeerKey(page.page, sessionId);
    await expectMarkersOnce(page.page, sessionId, [trusted.marker]);
  } finally {
    await stopPeerStack(stack, [page], testInfo);
  }
});


test("disabled peer capability retains usable Sync terminal input without a blank pane", async ({ browser }, testInfo) => {
  test.setTimeout(180_000);
  const stack = await startTerminalTestStack({
    ...PEER_STACK_OPTIONS,
    terminalPeer: { ...PEER_STACK_OPTIONS.terminalPeer, workerEnabled: false, disableLoopbackProbe: true },
  });
  let page: EnrolledPage | undefined;
  try {
    const fixtureWorker = await stack.startPtyFixtureWorker();
    page = await openPeerSmokePage(browser, stack);
    const sessionId = await createPeerFixtureSession(page.page, fixtureWorker);
    const route = await waitForSyncRoute(page.page, sessionId);

    expect(route).toMatchObject({
      activeKind: "sync",
      proofKind: "sync",
      candidateKind: null,
      fallbackReason: "disabled",
      syncReady: true,
    });
    await expect(page.page.getByTestId(`tab-${sessionId}`))
      .toHaveAttribute("data-terminal-transport", "sync");
    const trusted = await sendTrustedPeerKey(page.page, sessionId);
    await expectMarkersOnce(page.page, sessionId, [trusted.marker]);
  } finally {
    await stopPeerStack(stack, [page], testInfo);
  }
});

test("invalid offers, unavailable grants, expired grants, and identity mismatches fall back without recreating the PTY", async ({ browser }, testInfo) => {
  test.setTimeout(480_000);
  const faults = ["invalid_sdp", "missing_grant", "expired_grant", "identity_mismatch"] as const;
  for (const fault of faults) {
    const stack = await startTerminalTestStack({
      ...PEER_STACK_OPTIONS,
      terminalPeer: { ...PEER_STACK_OPTIONS.terminalPeer, enableFaults: true, disableLoopbackProbe: true },
    });
    let page: EnrolledPage | undefined;
    try {
      const fixtureWorker = await stack.startPtyFixtureWorker();
      const peerFaults = stack.peerFaults;
      if (!peerFaults) throw new Error("source peer fault controls were unavailable");
      page = await openPeerSmokePage(browser, stack);
      const sessionId = await spawnPtyFixtureSession(page.page, fixtureWorker);
      await peerFaults.armNextOfferFault(fixtureWorker.label, fault);
      await page.page.goto(`${new URL(page.page.url()).origin}/s/${sessionId}`, { waitUntil: "domcontentloaded" });
      await page.page.waitForFunction(() => window.__smoke !== undefined);
      const [fallback] = await Promise.all([
        waitForPeerFallback(page.page, sessionId, "network_failed"),
        waitForPainted(page.page, sessionId, PTY_FIXTURE_READY),
      ]);

      expect(fallback).toMatchObject({ activeKind: "sync", proofKind: "sync", candidateKind: null });
      await expect(page.page.getByTestId(`tab-${sessionId}`))
        .toHaveAttribute("data-terminal-transport", "sync");
      const trusted = await sendTrustedPeerKey(page.page, sessionId);
      await expectMarkersOnce(page.page, sessionId, [trusted.marker]);
    } finally {
      await stopPeerStack(stack, [page], testInfo);
    }
  }
});

test("multi-megabyte direct cell delivery and direct history demand do not starve trusted control input", async ({ browser }, testInfo) => {
  test.setTimeout(360_000);
  await verifyLargeDirectPacketAndHistory(browser, testInfo);
});
