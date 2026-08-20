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

function frameCounts(page: Page, sessionId: string): Promise<{ frames: number; fullFrames: number }> {
  return page.evaluate((id) => {
    const smoke = (window as unknown as Window & { __smoke: RecoverySmokeApi }).__smoke;
    return { frames: smoke.cellFrameCount(id), fullFrames: smoke.cellFullFrameCount(id) };
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
// registers with registerCellHandler) rather than by dropping a frame, and that
// choice is the whole point: this pane's watermark is CURRENT. Nothing is
// missing, so needsClaimSnapshot (apps/worker/src/session-viewport.ts) declines
// the reveal claim and no full frame ever arrives to bypass the delta gate and
// clear a stuck latch. Recovery here can only come from the latch not surviving
// the park. A drop-based arm cannot discriminate that: it leaves the watermark
// behind, which always earns an authoritative snapshot on reveal.
test("a repair requested while hidden must not wedge the pane on reveal", async ({ smokePage, stack }, testInfo) => {
  test.skip(!testInfo.project.name.startsWith("chromium"), "desktop cell recovery contract");
  const sessionId = (await spawnSmokeShell(smokePage, stack.workerFp)).session_id;
  await navigateToSmokeSession(smokePage, sessionId);
  await expect.poll(() => smokePage.evaluate((id) => {
    const smoke = (window as unknown as Window & { __smoke: RecoverySmokeApi }).__smoke;
    return smoke.cellFullFrameCount(id);
  }, sessionId)).toBeGreaterThan(0);

  // Baseline while visible: a painted marker floor, then quiet. The pane must
  // enter the park with EVERY frame applied — a frame still in flight at the
  // hide would be dropped by the hidden handler, leaving the watermark behind
  // and earning a snapshot on reveal, which is exactly the self-healing shape
  // this case must avoid.
  await runInSession(smokePage, sessionId, "seq -f 'WEDGE-%04g' 1 12\r");
  await expect.poll(() => paintedMax(smokePage, sessionId), {
    timeout: 15_000,
    intervals: [50],
  }).toBe(12);
  await waitForStableCellFrames(smokePage, sessionId);
  const canary = `repair-latch-${sessionId}`;
  await setRecoveryCanary(smokePage, canary);
  const generation = await smokePage.evaluate(() => {
    const smoke = (window as unknown as Window & { __smoke: RecoverySmokeApi }).__smoke;
    return smoke.syncWsGeneration();
  });
  const parked = await frameCounts(smokePage, sessionId);
  const frozenMax = await paintedMax(smokePage, sessionId);

  try {
    // Close the visibility gate, THEN ask for the repair: sendClaim can only
    // divert this claim to sendPark()/sendWithdraw(), so nothing is in flight
    // and the latch must not be left armed.
    await setHidden(smokePage, true);
    const requested = await smokePage.evaluate((id) => {
      const smoke = (window as unknown as Window & { __smoke: RecoverySmokeApi }).__smoke;
      return smoke.requestCellMountRepair(id);
    }, sessionId);
    // False = no pane had a repair callback registered, so nothing was armed and
    // everything below would pass for the wrong reason.
    expect(requested).toBe(true);

    // The PTY must stay QUIET across the park, and the producer must start only
    // after the reveal. needsClaimSnapshot's third witness is `lastPtyOutMs`: a
    // returning viewer whose PTY moved while it was withdrawn earns an
    // authoritative snapshot, and that snapshot would clear a stuck latch by
    // itself — healing the very wedge this case exists to catch.
    await smokePage.waitForTimeout(600);
    const quiet = await frameCounts(smokePage, sessionId);
    // Nothing streamed to the parked viewer, so the reveal claim below arrives
    // with `wasStreaming` false and a watermark the worker will not repaint.
    expect(quiet).toEqual(parked);
    const frozenViewport = await viewportText(smokePage, sessionId);

    await setHidden(smokePage, false);

    // Output resumes only now, as deltas on top of the grid the pane already
    // holds. A wedged pane drops every one of them.
    await runInSession(
      smokePage,
      sessionId,
      "i=13; while [ \"$i\" -le 400 ]; do printf 'WEDGE-%04d\\n' \"$i\"; i=$((i+1)); sleep 0.1; done\r",
    );

    // THE regression: with the latch still armed, every delta after the reveal is
    // dropped by the continuity gate and the viewport stays on frozenMax forever.
    // Several markers, not one, so a single lucky repaint cannot satisfy it.
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
    // No authoritative snapshot was involved: the worker declined to repaint a
    // current watermark, so those rows were painted by DELTAS through the gate
    // the latch guards. This is what makes the case discriminating — if a full
    // frame had arrived it would have cleared a stuck latch by itself.
    expect(revealed.fullFrames).toBe(parked.fullFrames);

    // Still live afterwards: later deltas keep painting, so the reveal left a
    // streaming pane, not a one-shot repaint.
    await expect.poll(() => paintedMax(smokePage, sessionId), {
      timeout: 15_000,
      intervals: [50],
    }).toBeGreaterThan(revealed.max);
    expect(await viewportText(smokePage, sessionId)).not.toBe(revealed.text);

    // The recovered viewport is intact (no lost, duplicated, or out-of-position
    // rows), the same document recovered it (canary survived — no reload), and it
    // reused the existing Sync socket.
    const probe = await recoveryProbe(smokePage, sessionId, MARKER);
    expect(probe.canary).toBe(canary);
    expect(probe.scan).toMatchObject({ duplicated: [], missing: 0, outOfOrder: 0 });
    expect(probe.atBottom).toBe(true);
    expect(await smokePage.evaluate(() => {
      const smoke = (window as unknown as Window & { __smoke: RecoverySmokeApi }).__smoke;
      return smoke.syncWsGeneration();
    })).toBe(generation);
  } finally {
    await setHidden(smokePage, false).catch(() => undefined);
    await runInSession(smokePage, sessionId, "\u0003").catch(() => undefined);
  }
});
