import { test, expect } from "./fixtures.ts";
import type { RecoverySmokeApi } from "./terminal-smoke-api.ts";
import type { TerminalStreamProbe } from "../../apps/web/src/lib/smoke.ts";
import {
  spawnSmokeShell,
  navigateToSmokeSession,
  waitForStableCellFrames,
  setRecoveryCanary,
  recoveryProbe,
  expectCleanRecovery,
} from "./terminal-helpers.ts";
import { readTerminalStreamProbe } from "./terminal-probe-helpers.ts";

type StreamWatermark = TerminalStreamProbe["browser"]["handler_canonical"];

function sameWatermark(left: StreamWatermark, right: StreamWatermark): boolean {
  return left.grid_epoch === right.grid_epoch && left.seq === right.seq;
}

function advancedWatermark(next: StreamWatermark, previous: StreamWatermark): boolean {
  return next.grid_epoch === previous.grid_epoch
    && next.seq !== null
    && previous.seq !== null
    && next.seq > previous.seq;
}

function replicaAndRendererConverged(probe: TerminalStreamProbe): boolean {
  const { handler_canonical: canonical, dom_reconciled: dom, presentation, replica, view, wire_received: wire } =
    probe.browser;
  return replica.baseline_ready
    && !replica.resync_latched
    && replica.expected_stream_id === view.stream_id
    && sameWatermark(wire, canonical)
    && presentation !== null
    && sameWatermark(presentation.canonical, canonical)
    && sameWatermark(dom, canonical);
}

test("dropped initial full renderer delivery replays canonical state on the first delta", async ({ smokePage, stack }, testInfo) => {
  test.skip(!testInfo.project.name.startsWith("chromium"), "desktop cell recovery contract");
  const sessionId = crypto.randomUUID();
  const canary = `initial-full-${sessionId}`;
  const spawned = await smokePage.evaluate(async ({ workerFp, sessionId: id }) => {
    const smoke = (window as unknown as { __smoke: RecoverySmokeApi }).__smoke;
    smoke.dropNextCellFrame(id);
    return smoke.spawnShell(workerFp, "/tmp", id);
  }, { workerFp: stack.workerFp, sessionId });
  expect(spawned.session_id).toBe(sessionId);
  await navigateToSmokeSession(smokePage, sessionId);
  await expect.poll(() => smokePage.evaluate(
    (id) => (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.droppedCellFrameCount(id),
    sessionId,
  )).toBe(1);

  // The persisted seam is consumed only after the replica folds its first
  // accepted full and immediately before renderer delivery. Shell startup may
  // produce the repairing delta before this diagnostic round trip, so the
  // counted suppression—not a racy transient DOM watermark—is the loss proof.
  const droppedBaseline = await readTerminalStreamProbe(smokePage, sessionId);
  expect(droppedBaseline.browser.replica).toMatchObject({
    baseline_ready: true,
    resync_latched: false,
    expected_stream_id: droppedBaseline.browser.view.stream_id,
  });
  expect(droppedBaseline.browser.handler_canonical.seq).not.toBeNull();
  expect(droppedBaseline.browser.handler_canonical.grid_epoch).not.toBeNull();
  expect(sameWatermark(
    droppedBaseline.browser.wire_received,
    droppedBaseline.browser.handler_canonical,
  )).toBe(true);
  await setRecoveryCanary(smokePage, canary);

  await smokePage.evaluate(
    async (id) => (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.input(id, "printf 'INITIAL-RECOVER-%03d\\n' 1\r"),
    sessionId,
  );
  await expect.poll(() => smokePage.evaluate(
    (id) => (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.viewportText(id),
    sessionId,
  ), { timeout: 10_000, intervals: [50] }).toContain("INITIAL-RECOVER-001");
  await expect.poll(async () => {
    const probe = await readTerminalStreamProbe(smokePage, sessionId);
    return {
      canonicalAdvanced: advancedWatermark(
        probe.browser.handler_canonical,
        droppedBaseline.browser.handler_canonical,
      ),
      converged: replicaAndRendererConverged(probe),
    };
  }, { timeout: 10_000, intervals: [50, 100] }).toEqual({
    canonicalAdvanced: true,
    converged: true,
  });
  expectCleanRecovery(await recoveryProbe(smokePage, sessionId, "INITIAL-RECOVER-"), canary, 1, 1);
});

test("dropped streaming delta recovers before the producer goes quiet", async ({ smokePage, stack }, testInfo) => {
  test.skip(!testInfo.project.name.startsWith("chromium"), "desktop cell recovery contract");
  const sessionId = (await spawnSmokeShell(smokePage, stack.workerFp)).session_id;
  await navigateToSmokeSession(smokePage, sessionId);
  await expect.poll(async () => replicaAndRendererConverged(
    await readTerminalStreamProbe(smokePage, sessionId),
  ), { timeout: 10_000, intervals: [50, 100] }).toBe(true);
  await waitForStableCellFrames(smokePage, sessionId);
  const before = await readTerminalStreamProbe(smokePage, sessionId);
  const canary = `middle-delta-${sessionId}`;
  await setRecoveryCanary(smokePage, canary);

  await smokePage.evaluate(async (id) => {
    const smoke = (window as unknown as { __smoke: RecoverySmokeApi }).__smoke;
    smoke.dropNextCellFrame(id);
    await smoke.input(
      id,
      "i=1; while [ \"$i\" -le 80 ]; do printf 'STREAM-RECOVER-%03d\\n' \"$i\"; i=$((i+1)); sleep 0.03; done\r",
    );
  }, sessionId);
  await expect.poll(() => smokePage.evaluate(
    (id) => (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.droppedCellFrameCount(id),
    sessionId,
  )).toBe(1);
  await expect.poll(() => smokePage.evaluate(
    (id) => (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.markerScan(id, "STREAM-RECOVER-").max,
    sessionId,
  ), { timeout: 10_000, intervals: [50] }).toBeGreaterThanOrEqual(20);
  const earlyMax = await smokePage.evaluate(
    (id) => (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.markerScan(id, "STREAM-RECOVER-").max,
    sessionId,
  );
  expect(earlyMax).toBeLessThan(80);
  await expect.poll(() => smokePage.evaluate(
    (id) => (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.markerScan(id, "STREAM-RECOVER-").max,
    sessionId,
  ), { timeout: 15_000, intervals: [50] }).toBe(80);
  await waitForStableCellFrames(smokePage, sessionId);
  await expect.poll(async () => {
    const probe = await readTerminalStreamProbe(smokePage, sessionId);
    return {
      canonicalAdvanced: advancedWatermark(
        probe.browser.handler_canonical,
        before.browser.handler_canonical,
      ),
      converged: replicaAndRendererConverged(probe),
    };
  }, { timeout: 10_000, intervals: [50, 100] }).toEqual({
    canonicalAdvanced: true,
    converged: true,
  });
  expectCleanRecovery(await recoveryProbe(smokePage, sessionId, "STREAM-RECOVER-"), canary, 1, 80);
});

test("dropped final frame is repaired by the applied-sequence heartbeat", async ({ smokePage, stack }, testInfo) => {
  test.skip(!testInfo.project.name.startsWith("chromium"), "desktop cell recovery contract");
  test.setTimeout(75_000);
  const sessionId = (await spawnSmokeShell(smokePage, stack.workerFp)).session_id;
  await navigateToSmokeSession(smokePage, sessionId);
  await smokePage.evaluate(
    async (id) => (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.input(id, "stty -echo; printf 'HEARTBEAT-READY-%03d\\n' 1\r"),
    sessionId,
  );
  await expect.poll(() => smokePage.evaluate(
    (id) => (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.viewportText(id),
    sessionId,
  )).toContain("HEARTBEAT-READY-001");
  await expect.poll(() => smokePage.evaluate((id) => {
    // The test bootstrap installs this typed in-process harness on window.
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    const parts = smokeWindow.__smoke.viewportText(id).split("HEARTBEAT-READY-001");
    // PS1-agnostic: viewportText concatenates rows (and trailing composer UI
    // text), so require the row right after the marker to be a prompt token
    // ending in $ or #. Hardcoding bash-<version>$ broke on CI runners whose
    // profile sets PS1 to user@host:cwd$.
    return /^\S{0,120}[$#](?:\s|$)/.test(parts.length > 1 ? (parts.at(-1) ?? "") : "");
  }, sessionId)).toBe(true);
  await waitForStableCellFrames(smokePage, sessionId);
  await smokePage.evaluate(
    async (id) => (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.input(
      id,
      "read _; printf 'HEARTBEAT-RECOVER-%03d\\n' 1; read _\r",
    ),
    sessionId,
  );
  await waitForStableCellFrames(smokePage, sessionId);
  const ready = await readTerminalStreamProbe(smokePage, sessionId);
  expect(replicaAndRendererConverged(ready)).toBe(true);
  const leaseDeadline = ready.browser.view.lease_deadline_ms;
  if (leaseDeadline === null) throw new Error("active terminal view omitted its lease deadline");
  // Observe one healthy exact heartbeat first. The dropped delivery then has a
  // full heartbeat interval in which its canonical/renderer sequence gap is
  // observable, instead of racing an already-due renewal.
  await expect.poll(async () => (
    await readTerminalStreamProbe(smokePage, sessionId)
  ).browser.view.lease_deadline_ms ?? 0, {
    timeout: 10_000,
    intervals: [100, 250],
  }).toBeGreaterThan(leaseDeadline);
  await waitForStableCellFrames(smokePage, sessionId);
  const before = await readTerminalStreamProbe(smokePage, sessionId);
  const canary = `heartbeat-${sessionId}`;
  await setRecoveryCanary(smokePage, canary);
  const beforeWsGeneration = before.browser.sync.socket_generation;

  await smokePage.evaluate(async (id) => {
    const smoke = (window as unknown as { __smoke: RecoverySmokeApi }).__smoke;
    smoke.dropNextCellFrame(id);
    await smoke.input(id, "go\r");
  }, sessionId);
  await expect.poll(() => smokePage.evaluate(
    (id) => (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.droppedCellFrameCount(id),
    sessionId,
  ), { timeout: 3_000, intervals: [20, 50] }).toBe(1);

  const dropped = await readTerminalStreamProbe(smokePage, sessionId);
  expect(advancedWatermark(
    dropped.browser.handler_canonical,
    before.browser.handler_canonical,
  )).toBe(true);
  const droppedPresentation = dropped.browser.presentation;
  if (!droppedPresentation) throw new Error("dropped final frame omitted renderer presentation state");
  expect(sameWatermark(
    droppedPresentation.canonical,
    before.browser.dom_reconciled,
  )).toBe(true);
  expect(dropped.browser.dom_reconciled).toEqual(before.browser.dom_reconciled);
  expect(await smokePage.evaluate(
    (id) => (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.viewportText(id),
    sessionId,
  )).not.toContain("HEARTBEAT-RECOVER-001");

  await expect.poll(() => smokePage.evaluate(
    (id) => (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.viewportText(id),
    sessionId,
  ), { timeout: 45_000, intervals: [250] }).toContain("HEARTBEAT-RECOVER-001");
  const after = await readTerminalStreamProbe(smokePage, sessionId);
  expect(replicaAndRendererConverged(after)).toBe(true);
  expect(after.browser.handler_canonical.grid_epoch)
    .toBe(dropped.browser.handler_canonical.grid_epoch);
  const droppedSeq = dropped.browser.handler_canonical.seq;
  const recoveredSeq = after.browser.handler_canonical.seq;
  if (droppedSeq === null || recoveredSeq === null) {
    throw new Error("heartbeat recovery omitted its canonical sequence");
  }
  expect(recoveredSeq).toBeGreaterThanOrEqual(droppedSeq);
  expect(after.browser.sync.socket_generation).toBe(beforeWsGeneration);
  expectCleanRecovery(await recoveryProbe(smokePage, sessionId, "HEARTBEAT-RECOVER-"), canary, 1, 1);
  await smokePage.evaluate(async (id) => {
    const smoke = (window as unknown as { __smoke: RecoverySmokeApi }).__smoke;
    await smoke.input(id, "go\r");
    await smoke.input(id, "stty echo\r");
  }, sessionId);
});

