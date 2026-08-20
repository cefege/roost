import { test, expect } from "./fixtures.ts";
import type { Page } from "@playwright/test";
import type { RecoverySmokeApi } from "./terminal-smoke-api.ts";
import {
  spawnSmokeShell,
  navigateToSmokeSession,
  waitForStableCellFrames,
  setRecoveryCanary,
  recoveryProbe,
} from "./terminal-helpers.ts";

const MARKER = "WEDGE-";

/** Highest marker actually PAINTED in this pane's rows. A wedged pane pins it. */
function paintedMax(page: Page, sessionId: string): Promise<number> {
  return page.evaluate(({ id, prefix }) => {
    // The smoke bootstrap installs this typed in-process harness on window.
    const smoke = (window as unknown as Window & { __smoke: RecoverySmokeApi }).__smoke;
    return smoke.markerScan(id, prefix).max;
  }, { id: sessionId, prefix: MARKER });
}

function viewportText(page: Page, sessionId: string): Promise<string> {
  return page.evaluate((id) => {
    const smoke = (window as unknown as Window & { __smoke: RecoverySmokeApi }).__smoke;
    return smoke.viewportText(id);
  }, sessionId);
}

function frameCounts(
  page: Page,
  sessionId: string,
): Promise<{ frames: number; fullFrames: number; backfillRequests: number }> {
  return page.evaluate((id) => {
    const smoke = (window as unknown as Window & { __smoke: RecoverySmokeApi }).__smoke;
    return {
      frames: smoke.cellFrameCount(id),
      fullFrames: smoke.cellFullFrameCount(id),
      backfillRequests: smoke.scrollbackBackfillRequestCount(id),
    };
  }, sessionId);
}

function setHidden(page: Page, on: boolean): Promise<void> {
  return page.evaluate((value) => {
    const smoke = (window as unknown as Window & { __smoke: RecoverySmokeApi }).__smoke;
    smoke.forceHidden(value);
  }, on);
}

function runInSession(page: Page, sessionId: string, command: string): Promise<void> {
  return page.evaluate(async ({ id, text }) => {
    const smoke = (window as unknown as Window & { __smoke: RecoverySmokeApi }).__smoke;
    await smoke.input(id, text);
  }, { id: sessionId, text: command });
}

// The wedged-pane defect this guards: CellTerminal.tsx armed its
// `awaitingFullFrame` latch OPTIMISTICALLY — before knowing whether the repair
// claim actually transmitted. A pane that requests a full-frame repair while it
// is NOT visible/in-layout has that claim DIVERTED to sendPark()/sendWithdraw()
// by sendClaim's visibility gate, and the park never cleared the latch. The pane
// then sat armed with nothing in flight, the delta gate in the cell handler
// dropped EVERY subsequent frame, and the one retry that could have re-asked
// (the apply-failed path) was neutered by requestFullFrame's own re-entrancy
// guard. Result: whole regions of the viewport frozen forever with no self-heal
// — the production report behind this spec.
//
// The repair is requested through the mount-repair callback
// (__smoke.requestCellMountRepair → the `() => requestFullFrame(0)` a pane
// registers with registerCellHandler) rather than by dropping a frame. The
// pane's watermark is current, but the visibility gate must still accept and
// divert the request without arming a latch for a repair that was not sent.
// Returning to the document now intentionally requests one viewport-only full
// repair. That forced repair must reconcile without leaving the pane wedged,
// after which ordinary deltas must continue painting without a reload.
test("a hidden repair request must not wedge streaming across forced document-resume repair", async ({ smokePage, stack }, testInfo) => {
  test.skip(!testInfo.project.name.startsWith("chromium"), "desktop cell recovery contract");
  const sessionId = (await spawnSmokeShell(smokePage, stack.workerFp)).session_id;
  await navigateToSmokeSession(smokePage, sessionId);
  await expect.poll(() => smokePage.evaluate((id) => {
    const smoke = (window as unknown as Window & { __smoke: RecoverySmokeApi }).__smoke;
    return smoke.cellFullFrameCount(id);
  }, sessionId)).toBeGreaterThan(0);

  // Baseline while visible: a painted marker floor, then quiet. Enter the park
  // with every frame applied so the forced document-resume repair is the only
  // frame transition attributable to returning from the hidden interval.
  await runInSession(smokePage, sessionId, "seq -f 'WEDGE-%04g' 1 12\r");
  await expect.poll(() => paintedMax(smokePage, sessionId), {
    timeout: 15_000,
    intervals: [50],
  }).toBe(12);
  await waitForStableCellFrames(smokePage, sessionId);
  const canary = `repair-latch-${sessionId}`;
  await setRecoveryCanary(smokePage, canary);
  const parked = await frameCounts(smokePage, sessionId);
  const frozenMax = await paintedMax(smokePage, sessionId);

  try {
    // Close the visibility gate, THEN ask for the repair. sendClaim can only
    // divert this claim to sendPark()/sendWithdraw(), so no repair is in flight
    // and the accepted request must not arm a doomed in-flight latch.
    await setHidden(smokePage, true);
    const requested = await smokePage.evaluate((id) => {
      const smoke = (window as unknown as Window & { __smoke: RecoverySmokeApi }).__smoke;
      return smoke.requestCellMountRepair(id);
    }, sessionId);
    // False = no pane had a repair callback registered, so nothing was armed and
    // everything below would pass for the wrong reason.
    expect(requested).toBe(true);

    // Keep the PTY quiet across the park so the hidden interval contributes no
    // cell traffic. This makes the single full frame below specifically the
    // intentional document-resume repair rather than output-driven recovery.
    await smokePage.waitForTimeout(600);
    const quiet = await frameCounts(smokePage, sessionId);
    expect(quiet).toEqual(parked);
    const frozenViewport = await viewportText(smokePage, sessionId);

    await setHidden(smokePage, false);

    // Foreground return intentionally forces exactly one authoritative repair.
    // It is viewport-only and must not trigger a scrollback backfill.
    await expect.poll(() => smokePage.evaluate((id) => {
      const smoke = (window as unknown as Window & { __smoke: RecoverySmokeApi }).__smoke;
      return smoke.cellFullFrameCount(id);
    }, sessionId), {
      timeout: 15_000,
      intervals: [50],
    }).toBe(parked.fullFrames + 1);
    expect(await smokePage.evaluate((id) => {
      const smoke = (window as unknown as Window & { __smoke: RecoverySmokeApi }).__smoke;
      return smoke.lastFullFrameSbRows(id);
    }, sessionId)).toBe(0);
    expect((await frameCounts(smokePage, sessionId)).backfillRequests).toBe(parked.backfillRequests);

    // Once the forced repair has reconciled, output resumes as deltas on top of
    // that repaired grid. A wedged pane would stop applying the stream.
    await runInSession(
      smokePage,
      sessionId,
      "i=13; while [ \"$i\" -le 400 ]; do printf 'WEDGE-%04d\\n' \"$i\"; i=$((i+1)); sleep 0.1; done\r",
    );

    // The regression surface: the hidden request plus forced resume repair must
    // leave the continuity gate open. Several markers, not one, prove repeated
    // paints rather than a single lucky repaint.
    await expect.poll(() => paintedMax(smokePage, sessionId), {
      timeout: 20_000,
      intervals: [50],
    }).toBeGreaterThanOrEqual(frozenMax + 3);
    const revealed = await smokePage.evaluate(({ id, prefix }) => {
      const smoke = (window as unknown as Window & { __smoke: RecoverySmokeApi }).__smoke;
      return {
        max: smoke.markerScan(id, prefix).max,
        fullFrames: smoke.cellFullFrameCount(id),
        text: smoke.viewportText(id),
      };
    }, { id: sessionId, prefix: MARKER });
    expect(revealed.text).not.toBe(frozenViewport);
    // Exactly the one intentional resume repair was involved; the repeatedly
    // advancing rows after it were painted by the continuing delta stream.
    expect(revealed.fullFrames).toBe(parked.fullFrames + 1);

    // Still live afterwards: later deltas keep painting, so the reveal left a
    // streaming pane, not a one-shot repaint.
    await expect.poll(() => paintedMax(smokePage, sessionId), {
      timeout: 15_000,
      intervals: [50],
    }).toBeGreaterThan(revealed.max);
    expect(await viewportText(smokePage, sessionId)).not.toBe(revealed.text);

    // The recovered viewport is intact (no lost, duplicated, or out-of-position
    // rows), and the canary proves the same document recovered without reload.
    const probe = await recoveryProbe(smokePage, sessionId, MARKER);
    expect(probe.canary).toBe(canary);
    expect(probe.scan).toMatchObject({ duplicated: [], missing: 0, outOfOrder: 0 });
    expect(probe.atBottom).toBe(true);
  } finally {
    await setHidden(smokePage, false).catch(() => undefined);
    await runInSession(smokePage, sessionId, "\u0003").catch(() => undefined);
  }
});
