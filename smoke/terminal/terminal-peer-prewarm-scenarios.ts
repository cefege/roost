// Direct peer pre-warm on a real stack: a peer authenticates for a worker before any pane
// views one of its sessions, and the first pane is elected onto that same peer without a
// negotiation of its own. terminal-peer.spec.ts owns the Playwright test registration.

import type { Browser, Page, TestInfo } from "@playwright/test";
import { expect } from "./fixtures.ts";
import { startTerminalTestStack, type TerminalTestStack } from "./stack.ts";
import { attachStackLogs, type EnrolledPage } from "./terminal-local-fast-path-helpers.ts";
import { openPeerSmokePage, sendTrustedPeerKey, waitForDirectRoute } from "./terminal-peer-helpers.ts";
import { spawnPtyFixtureSession, switchToSmokeSession } from "./terminal-helpers.ts";
import { expectMarkersOnce, waitForPainted } from "./terminal-multiview-helpers.ts";
import { PTY_FIXTURE_READY } from "./pty-fixture-protocol.ts";

declare global {
  interface Window {
    /** Every lane phase the pre-warm scenario saw while its pane opened. */
    prewarmPhaseProbe?: { seen: Array<string | null>; timer: number };
  }
}

const PREWARM_STACK_OPTIONS = {
  terminalPeer: {
    coordinatorEnabled: true,
    coordinatorStunUrls: [],
    workerEnabled: true,
    disableLoopbackProbe: true,
  },
} as const;

async function stopPrewarmScenario(
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

/**
 * Waits until the Rust web client's pre-warm holds an authenticated peer for `sessionId`'s
 * worker. The page must not view the session: a viewed worker is never pre-warmed.
 */
export async function waitForPrewarmedPeer(page: Page, sessionId: string): Promise<void> {
  // `prewarmed` is the Rust route diagnostic's alone, so it is narrowed off the
  // shared route type rather than declared on it.
  await expect.poll(() => page.evaluate((id) => {
    const route: unknown = window.__smoke.terminalBrowserSnapshot(id).route;
    if (route === null || typeof route !== "object") return null;
    const phase = "peer_phase" in route ? route.peer_phase : null;
    const prewarmed = "prewarmed" in route && route.prewarmed === true;
    return `${String(phase)} prewarmed=${prewarmed}`;
  }, sessionId), { timeout: 60_000, intervals: [100, 250, 500] }).toBe("candidate prewarmed=true");
}

/**
 * A worker with an open session that no pane views has an authenticated peer, and the first
 * pane onto that session reaches WebRTC on it: the lane never re-enters gathering, negotiating
 * or authenticating while the pane opens, and the route reports how long the pane waited.
 */
export async function verifyPrewarmedPeerServesFirstPane(browser: Browser, testInfo: TestInfo): Promise<void> {
  const stack = await startTerminalTestStack(PREWARM_STACK_OPTIONS);
  let page: EnrolledPage | undefined;
  try {
    const fixtureWorker = await stack.startPtyFixtureWorker();
    page = await openPeerSmokePage(browser, stack);
    const smokePage = page.page;
    // A session nothing views: no pane, so no view wants a peer on its worker.
    const sessionId = await spawnPtyFixtureSession(smokePage, fixtureWorker);
    await waitForPrewarmedPeer(smokePage, sessionId);

    // Every lane phase while the pane opens. A peer negotiated FOR the pane
    // would pass through gathering, negotiating and authenticating.
    await smokePage.evaluate((id) => {
      const seen: Array<string | null> = [];
      const timer = window.setInterval(() => {
        const phase = window.__smoke.terminalBrowserSnapshot(id).route.peer_phase ?? null;
        if (seen.at(-1) !== phase) seen.push(phase);
      }, 10);
      window.prewarmPhaseProbe = { seen, timer };
    }, sessionId);
    await switchToSmokeSession(smokePage, sessionId);
    await waitForPainted(smokePage, sessionId, PTY_FIXTURE_READY);
    const route = await waitForDirectRoute(smokePage, sessionId);
    const phases = await smokePage.evaluate(() => {
      const probe = window.prewarmPhaseProbe;
      if (!probe) throw new Error("the phase probe was not installed");
      window.clearInterval(probe.timer);
      return probe.seen;
    });
    expect(phases.filter((phase) => phase !== "candidate" && phase !== "active"), phases.join(" -> ")).toEqual([]);
    expect(route.activeKind).toBe("webrtc");
    const timeToDirectMs = await smokePage.evaluate((id) => {
      const active: unknown = window.__smoke.terminalBrowserSnapshot(id).route.active;
      return active !== null && typeof active === "object" && "time_to_direct_ms" in active
        && typeof active.time_to_direct_ms === "number"
        ? active.time_to_direct_ms
        : null;
    }, sessionId);
    expect(timeToDirectMs).not.toBeNull();
    const key = await sendTrustedPeerKey(smokePage, sessionId);
    await expectMarkersOnce(smokePage, sessionId, [key.marker]);
  } finally {
    await stopPrewarmScenario(stack, page, testInfo);
  }
}
