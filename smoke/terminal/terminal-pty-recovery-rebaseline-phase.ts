import type { Page } from "@playwright/test";
import type { TerminalStreamProbe } from "../../apps/web/src/lib/smoke.ts";
import { expect } from "./fixtures.ts";
import { encodePtyFixtureCommand } from "./pty-fixture-protocol.ts";
import type { RecoverySmokeApi } from "./terminal-smoke-api.ts";
import {
  spawnSmokeShell,
  switchToSmokeSession,
  inputSmokeTerminal,
} from "./terminal-helpers.ts";
import { attemptPaintedMarker } from "./terminal-paint-helpers.ts";
import {
  readTerminalStreamProbe,
  coordinatorTerminalViewState,
} from "./terminal-probe-helpers.ts";

type RebaselineRecoveryOptions = {
  page: Page;
  sessionId: string;
  suffix: string;
  stackWorkerFp: string;
  presented: TerminalStreamProbe;
  overwriteMarker: string;
};

export async function proveDroppedRebaselineRecovery({
  page,
  sessionId,
  suffix,
  stackWorkerFp,
  presented,
  overwriteMarker,
}: RebaselineRecoveryOptions): Promise<void> {
  // Preserve the painted pane, wait for its explicit inactive transition, and
  // advance the hidden PTY. On return, drop the browser's new-stream baseline;
  // the next delta must latch one resync and converge in place without a socket
  // transition, reload, or lost input.
  const otherSessionId = (await spawnSmokeShell(page, stackWorkerFp)).session_id;
  await switchToSmokeSession(page, otherSessionId);
  await expect.poll(async () => {
    const probe = await readTerminalStreamProbe(page, sessionId);
    const coordinator = coordinatorTerminalViewState(probe);
    return {
      status: probe.browser.view.status,
      active: probe.browser.view.active,
      coordinatorViews: coordinator?.activeViews ?? -1,
      coordinatorGeometry: coordinator?.effective ?? null,
    };
  }, { timeout: 10_000, intervals: [100] }).toEqual({
    status: "accepted",
    active: false,
    coordinatorViews: 0,
    coordinatorGeometry: null,
  });
  const rebaselineRepairMarker = `REBASELINE-REPAIR:${suffix}`;
  await inputSmokeTerminal(
    page,
    sessionId,
    encodePtyFixtureCommand({ op: "EMIT", text: rebaselineRepairMarker }),
  );
  expect((await attemptPaintedMarker(page, sessionId, rebaselineRepairMarker, 250)).proof)
    .toBeNull();
  const recoveryBefore = await page.evaluate((id) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return {
      dropped: smokeWindow.__smoke.droppedCellFrameCount(id),
      fullFrames: smokeWindow.__smoke.cellFullFrameCount(id),
    };
  }, sessionId);
  await page.evaluate((id) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    smokeWindow.__smoke.dropNextCellFrame(id);
  }, sessionId);
  await switchToSmokeSession(page, sessionId);
  await expect.poll(() => page.evaluate((id) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return smokeWindow.__smoke.droppedCellFrameCount(id);
  }, sessionId), { timeout: 10_000, intervals: [50] }).toBe(recoveryBefore.dropped + 1);
  const persistedPaint = await page.evaluate(({ id, marker }) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return smokeWindow.__smoke.waitForPaintedMarker(id, marker, 10_000);
  }, { id: sessionId, marker: overwriteMarker });
  expect(persistedPaint).toMatchObject({ marker: overwriteMarker, frames: 2 });

  const droppedBaseline = await readTerminalStreamProbe(page, sessionId);
  const presentedRevision = presented.browser.view.revision;
  const droppedRevision = droppedBaseline.browser.view.revision;
  if (!presentedRevision || !droppedRevision) {
    throw new Error("terminal view revision missing across inactive rebaseline");
  }
  expect(BigInt(droppedRevision)).toBeGreaterThan(BigInt(presentedRevision));
  expect(droppedBaseline.browser.view).toMatchObject({
    status: "accepted",
    active: true,
  });
  expect(droppedBaseline.browser.replica).toMatchObject({
    expected_stream_id: droppedBaseline.browser.view.stream_id,
    baseline_ready: false,
  });

  const resyncTriggerMarker = `REBASELINE-TRIGGER:${suffix}`;
  await inputSmokeTerminal(
    page,
    sessionId,
    encodePtyFixtureCommand({ op: "EMIT", text: resyncTriggerMarker }),
  );
  const repairedPaint = await page.evaluate(({ id, marker }) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return smokeWindow.__smoke.waitForPaintedMarker(id, marker, 15_000);
  }, { id: sessionId, marker: rebaselineRepairMarker });
  expect(repairedPaint).toMatchObject({ marker: rebaselineRepairMarker, frames: 2 });
  const triggerPaint = await page.evaluate(({ id, marker }) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return smokeWindow.__smoke.waitForPaintedMarker(id, marker, 15_000);
  }, { id: sessionId, marker: resyncTriggerMarker });
  expect(triggerPaint).toMatchObject({ marker: resyncTriggerMarker, frames: 2 });
  await expect.poll(async () => {
    const probe = await readTerminalStreamProbe(page, sessionId);
    const { view, replica } = probe.browser;
    const coordinator = coordinatorTerminalViewState(probe);
    return {
      active: view.active,
      status: view.status,
      revisionPreserved: view.revision === droppedRevision,
      baselineReady: replica.baseline_ready,
      resyncLatched: replica.resync_latched,
      replicaStreamMatched: replica.expected_stream_id === view.stream_id,
      coordinatorViews: coordinator?.activeViews ?? 0,
      coordinatorStreamMatched: coordinator?.streamId === view.stream_id,
      geometryMatched: coordinator?.effective?.cols === view.effective_cols
        && coordinator.effective?.rows === view.effective_rows,
      reconciled: probe.browser.handler_canonical.grid_epoch === probe.browser.dom_reconciled.grid_epoch
        && probe.browser.handler_canonical.seq === probe.browser.dom_reconciled.seq,
    };
  }, { timeout: 15_000, intervals: [50, 100, 250] }).toEqual({
    active: true,
    status: "accepted",
    revisionPreserved: true,
    baselineReady: true,
    resyncLatched: false,
    replicaStreamMatched: true,
    coordinatorViews: 1,
    coordinatorStreamMatched: true,
    geometryMatched: true,
    reconciled: true,
  });
  const recoveryAfter = await page.evaluate((id) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return {
      dropped: smokeWindow.__smoke.droppedCellFrameCount(id),
      fullFrames: smokeWindow.__smoke.cellFullFrameCount(id),
    };
  }, sessionId);
  expect(recoveryAfter.dropped - recoveryBefore.dropped).toBe(1);
  expect(recoveryAfter.fullFrames - recoveryBefore.fullFrames).toBe(1);
}
