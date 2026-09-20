// Input-handoff smoke helpers keep one real browser input pending across a
// harness-owned fault, then inspect its actual transport outcome and PTY paint.
// They do not manufacture worker frames or bypass the terminal input router.
// Fault ownership remains in the disposable stack; this file only drives it.

import type { Page } from "@playwright/test";
import { expect } from "./fixtures.ts";
import { encodePtyFixtureCommand } from "./pty-fixture-protocol.ts";
import { waitForPainted } from "./terminal-multiview-helpers.ts";
import type { RecoverySmokeApi } from "./terminal-smoke-api.ts";
import { readPeerRoute } from "./terminal-peer-helpers.ts";

const PEER_INPUT_RESULT_TIMEOUT_MS = 20_000;

export interface PeerInputAttemptResult {
  status: "accepted" | "rejected";
  reason: string | null;
}

interface PeerInputAttemptWindow {
  __peerSmokeInputAttempt?: Promise<PeerInputAttemptResult>;
}

/** Arms the fixture's next raw byte so a held browser `x` has a visible PTY-side effect. */
export async function armPeerFixtureKey(page: Page, sessionId: string, nonce: string): Promise<string> {
  const marker = `ARMED:${nonce}`;
  const frame = encodePtyFixtureCommand({ op: "ARM_KEY", nonce });
  await page.evaluate(async ({ id, command }) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    await smokeWindow.__smoke.input(id, command);
  }, { id: sessionId, command: frame });
  await waitForPainted(page, sessionId, marker);
  return `ACK:${nonce}`;
}

/** Starts a router-owned input and returns before its worker result settles. */
export async function beginPeerSmokeInput(page: Page, sessionId: string, payload: string): Promise<void> {
  await page.evaluate(({ id, input }) => {
    const runtimeWindow = window as unknown as Window & PeerInputAttemptWindow & { __smoke: RecoverySmokeApi };
    if (runtimeWindow.__peerSmokeInputAttempt) {
      throw new Error("peer smoke input attempt is already pending");
    }
    runtimeWindow.__peerSmokeInputAttempt = runtimeWindow.__smoke.input(id, input)
      .then(() => ({ status: "accepted" as const, reason: null }))
      .catch((error: unknown) => ({
        status: "rejected" as const,
        reason: error instanceof Error ? error.message : String(error),
      }));
  }, { id: sessionId, input: payload });
}

/** Reads one started input outcome, preserving rejection text emitted by the real input lane. */
export async function settlePeerSmokeInput(page: Page): Promise<PeerInputAttemptResult> {
  return page.evaluate(async (timeoutMs) => {
    const runtimeWindow = window as unknown as Window & PeerInputAttemptWindow;
    const attempt = runtimeWindow.__peerSmokeInputAttempt;
    if (!attempt) throw new Error("peer smoke input attempt was not started");
    let timer = 0;
    const timeout = new Promise<never>((_, reject) => {
      timer = window.setTimeout(() => reject(new Error("peer smoke input did not settle")), timeoutMs);
    });
    try {
      return await Promise.race([attempt, timeout]);
    } finally {
      window.clearTimeout(timer);
      delete runtimeWindow.__peerSmokeInputAttempt;
    }
  }, PEER_INPUT_RESULT_TIMEOUT_MS);
}

/** Delivers an actual keyboard byte after the test has armed a harness input hold. */
export async function pressHeldPeerKey(page: Page, sessionId: string): Promise<void> {
  await page.getByTestId(`terminal-slot-${sessionId}`).click();
  await expect.poll(() => page.evaluate((id) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return smokeWindow.__smoke.paneFocused(id).focused;
  }, sessionId), { timeout: 10_000, intervals: [50, 100] }).toBe(true);
  await page.keyboard.press("x");
}

/** A transport rejection settles before the worker can emit the armed ACK. */
export async function expectNoPeerFixtureAck(page: Page, sessionId: string, marker: string): Promise<void> {
  const text = await page.evaluate((id) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return smokeWindow.__smoke.viewportText(id);
  }, sessionId);
  expect(text).not.toContain(marker);
  expect(text).not.toContain("FIXTURE_ERROR:");
}

/** Waits for a retired direct route to stop owning the session and drains its input lane. */
export async function waitForPeerRouteLoss(page: Page, sessionId: string): Promise<void> {
  await expect.poll(async () => {
    const route = await readPeerRoute(page, sessionId);
    return route.activeKind !== "webrtc" && route.pendingInputCount === 0;
  }, { timeout: 60_000, intervals: [100, 250, 500] }).toBe(true);
}
