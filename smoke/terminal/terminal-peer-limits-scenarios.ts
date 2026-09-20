// Direct-peer grant-scope and input-budget scenarios exercise worker-owned
// authorization and keeper lanes through the disposable stack fault control.
// Browser checks observe painted PTY effects and real route settlement, never
// a synthetic terminal reply or an implementation-only counter.

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
} from "./terminal-peer-helpers.ts";
import { encodePtyFixtureCommand } from "./pty-fixture-protocol.ts";
import { inputSmokeTerminal, navigateToSmokeSession, waitForStableCellFrames } from "./terminal-helpers.ts";
import { waitForPainted } from "./terminal-multiview-helpers.ts";
import type { RecoverySmokeApi } from "./terminal-smoke-api.ts";
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

interface PeerBudgetFloodOutcome { accepted: boolean; reason: string | null; }
interface PeerBudgetFloodWindow {
  __peerBudgetFlood?: Promise<PeerBudgetFloodOutcome[]>;
  __peerBudgetFloodSettled?: PeerBudgetFloodOutcome[];
}

async function stopPeerLimitScenario(
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

/** Removes one session from a live grant while its input and history reads wait behind the real keeper lane. */
export async function verifyGrantShrinkFencesInputAndHistory(browser: Browser, testInfo: TestInfo): Promise<void> {
  const stack = await startTerminalTestStack(PEER_FAULT_STACK_OPTIONS);
  let page: EnrolledPage | undefined;
  try {
    const fixtureWorker = await stack.startPtyFixtureWorker();
    page = await openPeerSmokePage(browser, stack);
    const removedSessionId = await createPeerFixtureSession(page.page, fixtureWorker);
    await waitForDirectRoute(page.page, removedSessionId);
    const retainedSessionId = await createPeerFixtureSession(page.page, fixtureWorker);
    await waitForDirectRoute(page.page, retainedSessionId);
    await navigateToSmokeSession(page.page, removedSessionId);
    await waitForDirectRoute(page.page, removedSessionId);
    const peerFaults = stack.peerFaults;
    if (!peerFaults) throw new Error("source peer fault controls were unavailable");

    const historyPrefix = `SCOPE-HISTORY-${crypto.randomUUID().replaceAll("-", "")}-`;
    await inputSmokeTerminal(page.page, removedSessionId, encodePtyFixtureCommand({
      op: "FLOOD",
      prefix: historyPrefix,
      count: 40_000,
    }));
    await waitForPainted(page.page, removedSessionId, `${historyPrefix}40000`);
    await waitForStableCellFrames(page.page, removedSessionId);
    const beforeBackfills = await page.page.evaluate(
      (id) => window.__smoke.scrollbackBackfillRequestCount(id),
      removedSessionId,
    );
    const oldAckMarker = await armPeerFixtureKey(page.page, removedSessionId, crypto.randomUUID());
    const keeperHold = await peerFaults.holdKeeperAdmission(fixtureWorker.label, removedSessionId);
    await beginPeerSmokeInput(page.page, removedSessionId, "x");
    const heldHistory = await peerFaults.holdNextDirectHistoryResponse(
      fixtureWorker.label,
      removedSessionId,
    );
    await waitForDirectRoute(page.page, removedSessionId);
    const scrollBox = await page.page.getByTestId(`terminal-slot-${removedSessionId}`).locator(".wterm").boundingBox();
    if (!scrollBox) throw new Error("scope test terminal scroll container was unavailable");
    await page.page.mouse.move(scrollBox.x + scrollBox.width / 2, scrollBox.y + scrollBox.height / 2);
    await page.page.mouse.wheel(0, -100_000);
    await expect.poll(() => page!.page.evaluate(
      (id) => window.__smoke.scrollbackBackfillRequestCount(id),
      removedSessionId,
    ), { timeout: 30_000, intervals: [50, 100, 250] }).toBeGreaterThan(beforeBackfills);
    const historyRowsWhileHeld = await page.page.evaluate(
      (id) => window.__smoke.paintedScrollback(id).rows,
      removedSessionId,
    );

    expect(await peerFaults.shrinkGrantForSession(fixtureWorker.label, removedSessionId)).toBeGreaterThan(0);
    await keeperHold.release();
    await heldHistory.release();
    await settlePeerSmokeInput(page.page);
    await waitForPeerRouteLoss(page.page, removedSessionId);
    await expectNoPeerFixtureAck(page.page, removedSessionId, oldAckMarker);
    const afterHistoryRows = await page.page.evaluate((id) => window.__smoke.paintedScrollback(id).rows, removedSessionId);
    expect(afterHistoryRows).toEqual(historyRowsWhileHeld);

    await navigateToSmokeSession(page.page, retainedSessionId);
    await waitForDirectRoute(page.page, retainedSessionId);
    const retainedKey = await sendTrustedPeerKey(page.page, retainedSessionId);
    await waitForPainted(page.page, retainedSessionId, retainedKey.marker);
  } finally {
    await stopPeerLimitScenario(stack, page, testInfo);
  }
}

/** Holds a real keeper lane, floods valid direct input, and proves per-port admission remains bounded. */
export async function verifyPeerInputBudgetFlood(browser: Browser, testInfo: TestInfo): Promise<void> {
  const stack = await startTerminalTestStack(PEER_FAULT_STACK_OPTIONS);
  let page: EnrolledPage | undefined;
  try {
    const fixtureWorker = await stack.startPtyFixtureWorker();
    page = await openPeerSmokePage(browser, stack);
    const sessionId = await createPeerFixtureSession(page.page, fixtureWorker);
    await waitForDirectRoute(page.page, sessionId);
    const peerFaults = stack.peerFaults;
    if (!peerFaults) throw new Error("source peer fault controls were unavailable");

    await inputSmokeTerminal(page.page, sessionId, encodePtyFixtureCommand({ op: "ECHO_INPUT" }));
    await waitForPainted(page.page, sessionId, "ECHO_INPUT_ARMED");
    const keeperHold = await peerFaults.holdKeeperAdmission(fixtureWorker.label, sessionId);
    await page.page.evaluate((id) => {
      const runtimeWindow = window as unknown as Window & PeerBudgetFloodWindow & { __smoke: RecoverySmokeApi };
      const settled: PeerBudgetFloodOutcome[] = [];
      runtimeWindow.__peerBudgetFloodSettled = settled;
      runtimeWindow.__peerBudgetFlood = Promise.all(Array.from({ length: 40 }, (_, index) => {
        const payload = `BUDGET${index}|`;
        return runtimeWindow.__smoke.input(id, payload)
          .then(() => ({ accepted: true, reason: null }))
          .catch((error: unknown) => ({
            accepted: false,
            reason: error instanceof Error ? error.message : String(error),
          }))
          .then((outcome) => { settled.push(outcome); return outcome; });
      }));
    }, sessionId);
    await expect.poll(() => page!.page.evaluate(() => {
      const runtimeWindow = window as unknown as Window & PeerBudgetFloodWindow;
      return runtimeWindow.__peerBudgetFloodSettled?.filter((outcome) => !outcome.accepted).length ?? 0;
    }), { timeout: 30_000, intervals: [50, 100, 250] }).toBe(8);
    await keeperHold.release();
    const outcomes = await page.page.evaluate(async () => {
      const runtimeWindow = window as unknown as Window & PeerBudgetFloodWindow;
      const flood = runtimeWindow.__peerBudgetFlood;
      if (!flood) throw new Error("peer budget flood did not start");
      try {
        return await flood;
      } finally {
        delete runtimeWindow.__peerBudgetFlood;
        delete runtimeWindow.__peerBudgetFloodSettled;
      }
    });
    const accepted = outcomes.filter((outcome) => outcome.accepted);
    const rejected = outcomes.filter((outcome) => !outcome.accepted);
    expect(accepted).toHaveLength(32);
    expect(rejected).toHaveLength(8);
    expect(rejected.every((outcome) => outcome.reason === "worker input admission is full")).toBe(true);
    await expect.poll(() => readPeerRoute(page!.page, sessionId), {
      timeout: 30_000,
      intervals: [100, 250, 500],
    }).toMatchObject({ activeKind: "webrtc", pendingInputCount: 0 });
    const text = await page.page.evaluate((id) => window.__smoke.viewportText(id), sessionId);
    expect(text.match(/BUDGET\d+\|/gu)).toHaveLength(32);
  } finally {
    await stopPeerLimitScenario(stack, page, testInfo);
  }
}
