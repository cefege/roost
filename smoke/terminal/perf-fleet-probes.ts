// Fleet terminal qualification composes real fixture workers, browser panes, and
// smoke timing proofs without adding a second data-plane or fixture protocol.
// perf.spec.ts owns registration; this module owns bounded load and exact cleanup.
import type { Browser, Page, TestInfo } from "@playwright/test";
import { expect, test } from "./fixtures.ts";
import { encodePtyFixtureCommand } from "./pty-fixture-protocol.ts";
import { QUALIFY, percentile } from "./perf-probe-fixture.ts";
import { pressPlatformShortcut } from "./terminal-helpers.ts";
import { readTerminalStreamProbe } from "./terminal-probe-helpers.ts";
import { attachFleetFailure, captureFleetPresentation, disposeFleetReaderTrace, installFleetReaderTrace } from "./perf-fleet-diagnostics.ts";
import { activateFleetTerminal, confirmFleetVisibleMarkers, expectFleetLiveTarget, prepareFleetPeerFloods, verifyCompleteFleetFlood, type FleetFloodWorkload, type FleetPeer } from "./perf-fleet-peer-flood.ts";
import type { TerminalTestStack, TerminalTestWorker } from "./stack.ts";
import {
  applyScaleSplitPairLayout,
  assertScale,
  closeScaleDocuments,
  cleanupScaleSessions,
  createScaleDocuments,
  readScaleInputCapture,
  resetScaleInputCapture,
  scaleRunId,
  sendFixtureCommand,
  spawnScaleSessions,
  waitForPaintedScaleMarker,
  type ScaleDocument,
  type ScaleSession,
  type ScaleSmokeWindow,
} from "./terminal-scale-browser.ts";
import { waitForWorkerCapacity, type ScaleWorkerCapacity } from "./terminal-scale-preflight.ts";
const KEY_WARMUPS = 10;
const KEY_SAMPLES = 100;
const DRAIN_WARMUPS = 3;
const DRAIN_SAMPLES = 20;
const LOADED_WINDOW_MS = 20_000;
type Workload = FleetFloodWorkload;
type FleetTopology = {
  workers: { direct: TerminalTestWorker; delayed: TerminalTestWorker };
  capacities: ScaleWorkerCapacity[];
  sessions: ScaleSession[];
  visible: FleetPeer[];
  direct: FleetPeer;
  delayed: FleetPeer;
  unmounted: ScaleSession[];
  shape: { activePanes: number; hiddenPanes: number; rowDepth: number; spanDepth: number };
};
type KeySample = { retained: boolean; durationMs: number; workload: Workload; [key: string]: unknown };
type DrainSample = { retained: boolean; workload: string; drainMs: number; [key: string]: unknown };
type Distribution = { p50: number; p95: number; p99: number; max: number };
type TypingReport = { samples: KeySample[]; retained: number[]; stats: Distribution; byWorkload: Partial<Record<Workload, Distribution>>; durationMs: number };
type DrainReport = { samples: DrainSample[]; byWorkload: Record<string, Distribution & { samples: number[] }> };
function distribution(values: readonly number[]): Distribution {
  return { p50: percentile(values, 0.5), p95: percentile(values, 0.95), p99: percentile(values, 0.99), max: Math.max(...values) };
}
async function attach(testInfo: TestInfo, name: string, value: unknown): Promise<void> {
  await testInfo.attach(name, {
    body: JSON.stringify(value, (_, item) => typeof item === "bigint" ? item.toString() : item, 2),
    contentType: "application/json",
  });
}
async function prepareTrustedKey(target: FleetPeer, peer: FleetPeer): Promise<void> {
  const page = target.document.page;
  await activateFleetTerminal(peer);
  await activateFleetTerminal(target);
  await pressPlatformShortcut(page, "commandPalette", "k");
  await expect(page.getByTestId("command-palette")).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(page.getByTestId("command-palette")).toHaveCount(0);
  await activateFleetTerminal(target);
  await expectFleetLiveTarget(target);
}
async function timeTrustedKey(
  topology: FleetTopology,
  target: FleetPeer,
  workload: Workload,
  phase: string,
  runId: string,
  index: number,
): Promise<KeySample> {
  const peer = topology.visible.find((candidate) => candidate.document.page === target.document.page
    && candidate.session.id !== target.session.id);
  assertScale(peer, "interactive pane has no paired streaming peer");
  await prepareTrustedKey(target, peer);
  const nonce = `${phase}-${index}-${crypto.randomUUID().replaceAll("-", "").slice(0, 8)}`;
  const armFrame = encodePtyFixtureCommand({ op: "ARM_KEY", nonce });
  const armDispatchMonotonicMs = await sendFixtureCommand(target.document.page, target.session.id, armFrame);
  const armed = await waitForPaintedScaleMarker(target.document.page, target.session.id, `ARMED:${nonce}`);
  const preparedPeerFloods = await prepareFleetPeerFloods({
    visible: topology.visible,
    unmounted: topology.unmounted,
    target,
    workload,
    runId,
    sample: index,
  });
  await resetScaleInputCapture(target.document.page);
  const timingId = await target.document.page.evaluate((sessionId) => {
    const smokeWindow = window as unknown as ScaleSmokeWindow;
    return smokeWindow.__smoke.beginTerminalTiming("trusted_key", sessionId);
  }, target.session.id);
  const peerFloods = preparedPeerFloods.dispatch();
  let keyDown = false;
  try {
    await target.document.page.keyboard.down("x");
    keyDown = true;
  } finally {
    if (keyDown) await target.document.page.keyboard.up("x");
  }
  const timing = await target.document.page.evaluate(({ id, timingId: idForTiming, marker }) => {
    const smokeWindow = window as unknown as ScaleSmokeWindow;
    return smokeWindow.__smoke.finishTerminalTiming(idForTiming, id, marker);
  }, { id: target.session.id, timingId, marker: `ACK:${nonce}` });
  expect(timing.trustedKey).toBe(true);
  expect(timing.durationMs).toBeGreaterThanOrEqual(0);
  const capture = await readScaleInputCapture(target.document.page);
  expect(capture.droppedBatches).toBe(0);
  expect(capture.batches.some((batch) => batch.sessionId === target.session.id && batch.data.includes("x".charCodeAt(0)))).toBe(true);
  const peerPaints = await Promise.all(await peerFloods);
  // Documents have independent monotonic clocks; epoch stamps establish ordering.
  const peerOutputAfterKey = workload === "isolated"
    ? peerPaints.length === 0
    : peerPaints.some((peerPaint) => peerPaint.completion.epochMs >= timing.startedEpochMs);
  expect(peerOutputAfterKey).toBe(true);
  const metrics = await target.document.page.evaluate((id) => {
    const smokeWindow = window as unknown as ScaleSmokeWindow;
    return { predictiveEchoMode: localStorage.getItem("roostPredict") ?? "adaptive", perf: smokeWindow.__smoke.perfProbe(id) };
  }, target.session.id);
  return {
    retained: index >= KEY_WARMUPS,
    phase,
    workload,
    sessionId: target.session.id,
    workerFp: target.session.worker.workerFp,
    linkDelayMs: target === topology.delayed ? 25 : 0,
    frame: { kind: "trusted_key", bytes: 1, armBytes: Buffer.byteLength(armFrame) },
    armDispatchMonotonicMs,
    armed,
    timing,
    durationMs: timing.durationMs,
    peerPaints,
    peerOutputAfterKey,
    metrics,
  };
}
async function collectTyping(
  topology: FleetTopology,
  target: FleetPeer,
  phase: string,
  workloads: readonly Workload[],
  minimumMs: number,
): Promise<TypingReport> {
  const runId = scaleRunId();
  const startedAt = Date.now();
  const samples: KeySample[] = [];
  for (let index = 0; index < KEY_WARMUPS + KEY_SAMPLES; index++) {
    samples.push(await timeTrustedKey(topology, target, workloads[index % workloads.length]!, phase, runId, index));
  }
  while (Date.now() - startedAt < minimumMs) {
    const workload = workloads.find((candidate) => candidate !== "isolated") ?? "isolated";
    const preparedPeerFloods = await prepareFleetPeerFloods({
      visible: topology.visible,
      unmounted: topology.unmounted,
      target,
      workload,
      runId,
      sample: samples.length,
    });
    const pending = await preparedPeerFloods.dispatch();
    await Promise.all(pending);
    if (pending.length === 0) await target.document.page.waitForTimeout(50);
  }
  const retained = samples.filter((sample) => sample.retained).map((sample) => sample.durationMs);
  expect(samples.filter((sample) => !sample.retained)).toHaveLength(KEY_WARMUPS);
  expect(retained).toHaveLength(KEY_SAMPLES);
  return { samples, retained, stats: distribution(retained), byWorkload: Object.fromEntries(workloads.map((workload) => [workload, distribution(samples.filter((sample) => sample.retained && sample.workload === workload).map((sample) => sample.durationMs))])), durationMs: Date.now() - startedAt };
}
const DRAIN_WORKLOADS = [
  { name: "plain_8KiB", marker: "a", bytes: 8 * 1024, styled: false },
  { name: "plain_64KiB", marker: "b", bytes: 64 * 1024, styled: false },
  { name: "plain_1MiB", marker: "c", bytes: 1024 * 1024, styled: false },
  { name: "styled_sgr", marker: "d", bytes: 64 * 1024, styled: true },
] as const;
function drainPlan(
  runId: string,
  workload: typeof DRAIN_WORKLOADS[number],
  index: number,
) {
  const marker = `F${runId}${workload.marker}${index}-`;
  const styleStart = workload.styled ? "\x1b[38;5;208m" : "";
  const styleEnd = workload.styled ? "\x1b[0m" : "";
  const filler = "x".repeat(6);
  const prefix = `${styleStart}${marker}${filler}${styleEnd}`;
  const count = Math.ceil(workload.bytes / Buffer.byteLength(`${prefix}99999\r\n`));
  const frame = encodePtyFixtureCommand({ op: "FLOOD", prefix, count });
  return { frame, count, scanPrefix: `${marker}${filler}`, estimatedBytes: count * Buffer.byteLength(`${prefix}${count}\r\n`) };
}
async function drainSample(peer: FleetPeer, workload: typeof DRAIN_WORKLOADS[number], runId: string, index: number): Promise<DrainSample> {
  const plan = drainPlan(runId, workload, index);
  const nonce = `${runId}-${workload.name}-${index}-${crypto.randomUUID().replaceAll("-", "").slice(0, 8)}`;
  const completionMarker = `FLEET-DRAIN:${nonce}`;
  const drainFrame = encodePtyFixtureCommand({ op: "EMIT", text: completionMarker });
  const command = plan.frame + drainFrame;
  const beforeDispatch = await captureFleetPresentation(peer);
  const floodDispatchMonotonicMs = await sendFixtureCommand(peer.document.page, peer.session.id, command);
  try {
    const proof = await waitForPaintedScaleMarker(peer.document.page, peer.session.id, completionMarker);
    const drainMs = proof.monotonicMs - floodDispatchMonotonicMs;
    const integrity = await verifyCompleteFleetFlood(
      peer,
      plan.scanPrefix,
      plan.count,
      workload.bytes < 1024 * 1024,
    );
    return {
      retained: index >= DRAIN_WARMUPS,
      workload: workload.name,
      sessionId: peer.session.id,
      workerFp: peer.session.worker.workerFp,
      frame: { kind: workload.styled ? "styled_sgr_flood" : "plain_flood", bytes: Buffer.byteLength(command), outputBytes: plan.estimatedBytes, lines: plan.count, drainBytes: Buffer.byteLength(drainFrame) },
      floodDispatchMonotonicMs,
      paintMonotonicMs: proof.monotonicMs,
      flood_dispatch_to_final_paint_ms: drainMs,
      drainMs,
      proof,
      integrity,
    };
  } catch (error) {
    await attachFleetFailure(peer, completionMarker, beforeDispatch, "fleet-drain-failure.json");
    throw error;
  }
}
async function collectDrains(topology: FleetTopology): Promise<DrainReport> {
  const runId = scaleRunId();
  const samples: DrainSample[] = [];
  for (const workload of DRAIN_WORKLOADS) {
    for (let index = 0; index < DRAIN_WARMUPS + DRAIN_SAMPLES; index++) {
      const peer = topology.visible[index % topology.visible.length]!;
      samples.push(await drainSample(peer, workload, runId, index));
    }
  }
  const byWorkload = Object.fromEntries(DRAIN_WORKLOADS.map((workload) => {
    const retained = samples.filter((sample) => sample.workload === workload.name && sample.retained).map((sample) => sample.drainMs);
    expect(retained).toHaveLength(DRAIN_SAMPLES);
    return [workload.name, { samples: retained, ...distribution(retained) }];
  }));
  return { samples, byWorkload };
}
async function readShape(documents: readonly ScaleDocument[], ids: readonly string[]) {
  const pages = await Promise.all(documents.map((scaleDocument) => scaleDocument.page.evaluate((sessionIds) => {
    const slots = sessionIds.map((id) => document.querySelector<HTMLElement>(`[data-testid="terminal-slot-${CSS.escape(id)}"]`));
    const visible = slots.filter((slot) => { const rect = slot?.getBoundingClientRect(); return slot !== null && getComputedStyle(slot).visibility !== "hidden" && (rect?.width ?? 0) > 0 && (rect?.height ?? 0) > 0; }).length;
    return { visible, rows: slots.reduce((total, slot) => total + (slot?.querySelectorAll(".cell-row").length ?? 0), 0), spans: slots.reduce((total, slot) => total + (slot?.querySelectorAll(".cell-row span").length ?? 0), 0), splitRows: document.querySelectorAll("[data-testid^='pane-divider-'][data-dir='row']").length };
  }, [...ids])));
  const activePanes = pages.reduce((total, page) => total + page.visible, 0);
  expect(pages.every((page) => page.visible === 2 && page.splitRows === 1)).toBe(true);
  return { activePanes, hiddenPanes: 8 - activePanes, rowDepth: Math.max(...pages.map((page) => page.rows)), spanDepth: Math.max(...pages.map((page) => page.spans)) };
}
async function createFleet(browser: Browser, smokePage: Page, stack: TerminalTestStack, sessions: ScaleSession[], documents: ScaleDocument[]): Promise<FleetTopology> {
  const [direct, delayed] = await Promise.all([
    stack.startPtyFixtureWorker(),
    stack.startSecondPtyFixtureWorker({ workerLinkOneWayDelayMs: 25 }),
  ]);
  const capacities = await Promise.all([waitForWorkerCapacity(stack, direct), waitForWorkerCapacity(stack, delayed)]);
  assertScale(capacities.every(({ available }) => available >= 4), "each fleet worker needs four free terminal cores");
  await spawnScaleSessions({ stack, workers: [direct, delayed], capacities, count: 8, runId: scaleRunId(), createdSessions: sessions });
  const directSessions = sessions.filter((session) => session.worker.workerFp === direct.workerFp);
  const delayedSessions = sessions.filter((session) => session.worker.workerFp === delayed.workerFp);
  expect(sessions).toHaveLength(8);
  expect(directSessions).toHaveLength(4);
  expect(delayedSessions).toHaveLength(4);
  documents.push(...await createScaleDocuments({ browser, stack, contexts: 2, pagesPerContext: 1, workerFps: [direct.workerFp, delayed.workerFp], initialPage: smokePage }));
  const [directDocument, delayedDocument] = documents;
  assertScale(directDocument && delayedDocument, "fleet did not create two viewer documents");
  await applyScaleSplitPairLayout({ page: directDocument.page, stack, firstSessionId: directSessions[0]!.id, secondSessionId: directSessions[1]!.id });
  await applyScaleSplitPairLayout({ page: delayedDocument.page, stack, firstSessionId: delayedSessions[0]!.id, secondSessionId: delayedSessions[1]!.id });
  const directPeer = { document: directDocument, session: directSessions[1]! };
  const delayedPeer = { document: delayedDocument, session: delayedSessions[1]! };
  const directPeerTarget = { document: directDocument, session: directSessions[0]! };
  const delayedPeerTarget = { document: delayedDocument, session: delayedSessions[0]! };
  const visible = [directPeerTarget, directPeer, delayedPeerTarget, delayedPeer];
  await confirmFleetVisibleMarkers(visible, scaleRunId());
  return { workers: { direct, delayed }, capacities, sessions, visible, direct: directPeerTarget, delayed: delayedPeerTarget, unmounted: [...directSessions.slice(2), ...delayedSessions.slice(2)], shape: await readShape(documents, visible.map((peer) => peer.session.id)) };
}
function topologyReport(topology: FleetTopology) {
  return {
    workerCount: 2,
    workers: {
      direct: { fp: topology.workers.direct.workerFp, label: topology.workers.direct.label, workerLinkOneWayDelayMs: 0 },
      delayed: { fp: topology.workers.delayed.workerFp, label: topology.workers.delayed.label, workerLinkOneWayDelayMs: 25 },
    },
    capacities: topology.capacities,
    sessionCount: topology.sessions.length,
    unmountedSessionIds: topology.unmounted.map((session) => session.id),
    activePanes: topology.shape.activePanes,
    hiddenPanes: topology.shape.hiddenPanes,
    rowDepth: topology.shape.rowDepth,
    spanDepth: topology.shape.spanDepth,
  };
}
async function recoverPausedViewer(topology: FleetTopology): Promise<Record<string, unknown>> {
  const healthy = topology.direct;
  const paused = topology.delayed;
  let transportPaused = false;
  const before = await paused.document.page.evaluate((id) => {
    const smokeWindow = window as unknown as ScaleSmokeWindow & { __fleetPauseCanary?: object };
    smokeWindow.__fleetPauseCanary = { alive: true };
    return { href: location.href, generation: smokeWindow.__smoke.syncWsGeneration(), fullFrames: smokeWindow.__smoke.cellFullFrameCount(id) };
  }, paused.session.id);
  await paused.document.page.evaluate(() => (window as unknown as ScaleSmokeWindow).__smoke.pauseSyncTransport());
  transportPaused = true;
  try {
    const healthyMarker = `FLEET-HEALTHY:${scaleRunId()}`;
    await sendFixtureCommand(healthy.document.page, healthy.session.id, encodePtyFixtureCommand({ op: "EMIT", text: healthyMarker }));
    const healthyProof = await waitForPaintedScaleMarker(healthy.document.page, healthy.session.id, healthyMarker);
    const recoveryMarker = `FLEET-RECOVER:${scaleRunId()}`;
    await sendFixtureCommand(healthy.document.page, paused.session.id, encodePtyFixtureCommand({ op: "EMIT", text: recoveryMarker }));
    await healthy.document.page.waitForTimeout(250);
    expect(await paused.document.page.evaluate(({ id, marker }) => {
      const smokeWindow = window as unknown as ScaleSmokeWindow;
      return smokeWindow.__smoke.viewportText(id).includes(marker);
    }, { id: paused.session.id, marker: recoveryMarker })).toBe(false);
    await paused.document.page.evaluate(() => (window as unknown as ScaleSmokeWindow).__smoke.resumeSyncTransport());
    transportPaused = false;
    const recoveryProof = await waitForPaintedScaleMarker(paused.document.page, paused.session.id, recoveryMarker);
    const after = await paused.document.page.evaluate((id) => {
      const smokeWindow = window as unknown as ScaleSmokeWindow & { __fleetPauseCanary?: object };
      return { href: location.href, sameDocument: smokeWindow.__fleetPauseCanary !== undefined, generation: smokeWindow.__smoke.syncWsGeneration(), fullFrames: smokeWindow.__smoke.cellFullFrameCount(id) };
    }, paused.session.id);
    expect(after.href).toBe(before.href);
    expect(after.sameDocument).toBe(true);
    expect(after.generation).toBeGreaterThan(before.generation);
    expect(after.fullFrames).toBe(before.fullFrames + 1);
    const stream = await readTerminalStreamProbe(paused.document.page, paused.session.id);
    expect(stream.browser.replica.baseline_ready).toBe(true);
    expect(stream.browser.replica.resync_latched).toBe(false);
    expect(stream.browser.handler_canonical).toEqual(stream.browser.dom_reconciled);
    return { before, after, healthyProof, recoveryProof, topologyChange: { pausedViewer: paused.session.id, healthyViewer: healthy.session.id, activePanes: topology.shape.activePanes } };
  } finally {
    if (transportPaused) await paused.document.page.evaluate(() => (window as unknown as ScaleSmokeWindow).__smoke.resumeSyncTransport()).catch(() => undefined);
  }
}
async function runFleetPhase(
  browser: Browser,
  smokePage: Page,
  stack: TerminalTestStack,
  testInfo: TestInfo,
  phase: "composition" | "delayed",
): Promise<void> {
  const sessions: ScaleSession[] = [];
  const documents: ScaleDocument[] = [];
  let diagnosticPeers: readonly FleetPeer[] = [];
  try {
    const topology = await createFleet(browser, smokePage, stack, sessions, documents);
    diagnosticPeers = topology.visible;
    await installFleetReaderTrace(diagnosticPeers);
    if (phase === "composition") {
      const typing = await collectTyping(topology, topology.direct, "direct_loaded", ["same_worker", "other_worker", "together"], LOADED_WINDOW_MS);
      const drains = await collectDrains(topology);
      await attach(testInfo, "perf-fleet-composition.json", { phase, topology: topologyReport(topology), typing, drains, correctness: { roundRobin: true, mounted: 4, unmounted: topology.unmounted.length } });
      if (QUALIFY) {
        expect(typing.byWorkload.together!.p95).toBeLessThanOrEqual(75);
        expect(typing.byWorkload.together!.p99).toBeLessThanOrEqual(150);
        expect(drains.byWorkload.plain_8KiB!.p95).toBeLessThanOrEqual(100);
        expect(drains.byWorkload.plain_64KiB!.p95).toBeLessThanOrEqual(150);
        expect(drains.byWorkload.plain_1MiB!.p95).toBeLessThanOrEqual(500);
      }
    } else {
      const isolated = await collectTyping(topology, topology.delayed, "delayed_isolated", ["isolated"], 0);
      const loaded = await collectTyping(topology, topology.delayed, "delayed_loaded", ["same_worker", "other_worker", "together"], LOADED_WINDOW_MS);
      const recovery = await recoverPausedViewer(topology);
      await attach(testInfo, "perf-fleet-delayed.json", { phase, topology: topologyReport(topology), isolated, loaded, recovery, correctness: { roundRobin: true, mounted: 4, unmounted: topology.unmounted.length, proxiedWorker: topology.workers.delayed.workerFp, workerLinkOneWayDelayMs: 25 } });
      if (QUALIFY) {
        expect(loaded.byWorkload.together!.p95).toBeLessThanOrEqual(125);
        expect(loaded.byWorkload.together!.p99).toBeLessThanOrEqual(200);
      }
    }
  } finally {
    const cleanupErrors: string[] = [];
    await disposeFleetReaderTrace(diagnosticPeers);
    await closeScaleDocuments(documents).catch((error) => cleanupErrors.push(`documents: ${String(error)}`));
    await cleanupScaleSessions(stack, sessions).catch((error) => cleanupErrors.push(`sessions: ${String(error)}`));
    if (cleanupErrors.length > 0) throw new Error(`fleet cleanup failed: ${cleanupErrors.join("; ")}`);
  }
}
export async function probeFleetComposition(
  { smokePage, browser, stack }: { smokePage: Page; browser: Browser; stack: TerminalTestStack },
  testInfo: TestInfo,
): Promise<void> {
  test.skip(!testInfo.project.name.startsWith("chromium"), "desktop fleet qualification");
  test.setTimeout(900_000);
  await runFleetPhase(browser, smokePage, stack, testInfo, "composition");
}
export async function probeFleetDelayedWorkerLink(
  { smokePage, browser, stack }: { smokePage: Page; browser: Browser; stack: TerminalTestStack },
  testInfo: TestInfo,
): Promise<void> {
  test.skip(!testInfo.project.name.startsWith("chromium"), "desktop delayed-link qualification");
  test.setTimeout(900_000);
  await runFleetPhase(browser, smokePage, stack, testInfo, "delayed");
}
