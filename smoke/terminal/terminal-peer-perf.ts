// Direct-terminal performance qualification compares real selected carriers under equal PTY work.
// It measures a trusted browser key from real keydown through its painted fixture ACK.
// The helper retains only bounded timing, route, and outcome metrics; no terminal text or endpoint data.
import type { Browser, Page, TestInfo } from "@playwright/test";
import { expect } from "./fixtures.ts";
import { QUALIFY, percentile } from "./perf-probe-fixture.ts";
import { encodePtyFixtureCommand } from "./pty-fixture-protocol.ts";
import { disableTerminalPredictions } from "./perf-terminal-preconditions.ts";
import { applyScaleSplitPairLayout } from "./terminal-scale-browser.ts";
import { attachStackLogs, type EnrolledPage } from "./terminal-local-fast-path-helpers.ts";
import {
  createPeerFixtureSession,
  openPeerSmokePage,
  waitForDirectRoute,
  waitForSyncRoute,
} from "./terminal-peer-helpers.ts";
import { startTerminalTestStack, type TerminalTestStack } from "./stack.ts";
import { expectMarkersOnce, waitForPainted } from "./terminal-multiview-helpers.ts";
import type { RecoverySmokeApi } from "./terminal-smoke-api.ts";

declare const window: Window & { __smoke: RecoverySmokeApi };

type Carrier = "loopback" | "sync" | "webrtc";
type Workload = "isolated" | "visible_peer_flood";
type Location = "local" | "relay_50ms";
type Distribution = { p50: number; p95: number; p99: number; max: number };
type Sample = {
  retained: boolean;
  workload: Workload;
  duration_ms: number;
  worker_epoch: string | null;
  worker_control_rtt_ms: number;
  route_kind: Carrier;
  candidate_type: "host" | "srflx" | "prflx" | "none";
  pending_input_count: number;
  outcomes: { accepted: number; rejected: number; ambiguous: number; capture_dropped: number };
  flood: { marker_missing: number; marker_duplicated: number; marker_out_of_order: number } | null;
};
type Scenario = {
  carrier: Carrier;
  location: Location;
  worker_link_one_way_delay_ms: 0 | 25;
  port_start: number;
};
type ScenarioReport = {
  carrier: Carrier;
  location: Location;
  worker_link_one_way_delay_ms: 0 | 25;
  warmups: number;
  retained_samples: number;
  workload_plan: readonly Workload[];
  timing: Distribution;
  worker_control_rtt: Distribution;
  timing_by_workload: Record<Workload, Distribution>;
  candidate_types: string[];
  integrity: {
    dropout_count: number;
    ambiguity_count: number;
    rejection_count: number;
    capture_drop_count: number;
    painted_ack_count: number;
    flood_marker_missing: number;
    flood_marker_duplicated: number;
    flood_marker_out_of_order: number;
  };
  samples: Sample[];
};
type PeerPerfStack = {
  stack: TerminalTestStack;
  page: EnrolledPage;
  targetSessionId: string;
  floodSessionId: string;
};
type VisiblePeerFlood = {
  sessionId: string;
  prefix: string;
  completion: string;
  input: Promise<void>;
};


const KEY_WARMUPS = 10;
const KEY_SAMPLES = 100;
const VISIBLE_PEER_FLOOD_LINES = 128;
const WORKLOAD_PLAN = ["isolated", "visible_peer_flood"] as const;

export async function probeTerminalPeerPerformance(
  browser: Browser,
  testInfo: TestInfo,
): Promise<void> {
  const loopback = await collectScenario(browser, testInfo, {
    carrier: "loopback", location: "local", worker_link_one_way_delay_ms: 0, port_start: 41_000,
  });
  const syncLocal = await collectScenario(browser, testInfo, {
    carrier: "sync", location: "local", worker_link_one_way_delay_ms: 0, port_start: 41_064,
  });
  const directLocal = await collectScenario(browser, testInfo, {
    carrier: "webrtc", location: "local", worker_link_one_way_delay_ms: 0, port_start: 41_128,
  });
  const syncDelayed = await collectScenario(browser, testInfo, {
    carrier: "sync", location: "relay_50ms", worker_link_one_way_delay_ms: 25, port_start: 41_192,
  });
  const directDelayed = await collectScenario(browser, testInfo, {
    carrier: "webrtc", location: "relay_50ms", worker_link_one_way_delay_ms: 25, port_start: 41_256,
  });
  const comparison = {
    loopback,
    sync_local: syncLocal,
    direct_local: directLocal,
    sync_relay_50ms: syncDelayed,
    direct_relay_50ms: directDelayed,
    qualification_host: QUALIFY,
  };
  await attachJson(testInfo, "terminal-peer-perf.json", comparison);
  if (!QUALIFY) return;
  expect(directLocal.timing.p95).toBeLessThanOrEqual(loopback.timing.p95 + 10);
  expect(syncDelayed.timing.p95 - directDelayed.timing.p95).toBeGreaterThanOrEqual(25);
}

async function collectScenario(
  browser: Browser,
  testInfo: TestInfo,
  scenario: Scenario,
): Promise<ScenarioReport> {
  let resources: PeerPerfStack | undefined;
  const samples: Sample[] = [];
  let failed = false;
  try {
    resources = await startScenario(browser, scenario);
    const runId = crypto.randomUUID().replaceAll("-", "").slice(0, 12);
    for (let index = 0; index < KEY_WARMUPS + KEY_SAMPLES; index++) {
      samples.push(await timeTrustedPaintedKey(resources, scenario.carrier, WORKLOAD_PLAN[index % WORKLOAD_PLAN.length]!, runId, index));
    }
    const report = summarizeScenario(scenario, samples);
    assertDeterministicScenario(report);
    await attachJson(testInfo, `terminal-peer-perf-${scenario.carrier}-${scenario.location}.json`, report);
    return report;
  } catch (error) {
    failed = true;
    await attachJson(testInfo, `terminal-peer-perf-${scenario.carrier}-${scenario.location}-partial.json`, {
      scenario,
      samples,
    });
    throw error;
  } finally {
    if (resources) await stopScenario(resources, testInfo, failed);
  }
}

async function startScenario(browser: Browser, scenario: Scenario): Promise<PeerPerfStack> {
  const peerEnabled = scenario.carrier !== "sync";
  const stack = await startTerminalTestStack({
    terminalPeer: {
      coordinatorEnabled: true,
      coordinatorStunUrls: [],
      workerEnabled: peerEnabled,
      ...(peerEnabled ? {
        workerPortRange: { min: scenario.port_start, max: scenario.port_start + 31 },
      } : {}),
      disableLoopbackProbe: scenario.carrier !== "loopback",
    },
  });
  let page: EnrolledPage | undefined;
  try {
    const worker = await stack.startPtyFixtureWorker({
      workerLinkOneWayDelayMs: scenario.worker_link_one_way_delay_ms,
    });
    page = await openPeerSmokePage(browser, stack, scenario.carrier === "loopback"
      ? { origin: stack.localUiUrl(worker.workerFp) }
      : {});
    await disableTerminalPredictions([page.page], [worker.workerFp]);
    const targetSessionId = await createPeerFixtureSession(page.page, worker);
    const floodSessionId = await createPeerFixtureSession(page.page, worker);
    await applyScaleSplitPairLayout({
      page: page.page,
      stack,
      firstSessionId: targetSessionId,
      secondSessionId: floodSessionId,
    });
    await Promise.all([
      waitForCarrier(page.page, targetSessionId, scenario.carrier),
      waitForCarrier(page.page, floodSessionId, scenario.carrier),
    ]);
    if (scenario.carrier === "webrtc") {
      await expect.poll(async () => page!.page.evaluate((sessionId) =>
        window.__smoke.terminalBrowserSnapshot(sessionId).route.active?.candidate_type ?? "none", targetSessionId),
      { timeout: 30_000, intervals: [100, 250, 500] }).toBe("host");
    }
    return { stack, page, targetSessionId, floodSessionId };
  } catch (error) {
    await page?.close().catch(() => undefined);
    await stack.stop().catch(() => undefined);
    throw error;
  }
}

async function stopScenario(
  resources: PeerPerfStack,
  testInfo: TestInfo,
  failed: boolean,
): Promise<void> {
  if (failed) await attachStackLogs(testInfo, resources.stack);
  await resources.page.close();
  await resources.stack.stop();
}


async function waitForCarrier(page: Page, sessionId: string, carrier: Carrier): Promise<void> {
  if (carrier === "sync") {
    await waitForSyncRoute(page, sessionId);
    return;
  }
  await waitForDirectRoute(page, sessionId, carrier);
}

async function timeTrustedPaintedKey(
  resources: PeerPerfStack,
  carrier: Carrier,
  workload: Workload,
  runId: string,
  index: number,
): Promise<Sample> {
  const page = resources.page.page;
  const nonce = `${runId}-${index}-${crypto.randomUUID().replaceAll("-", "").slice(0, 8)}`;
  const arm = encodePtyFixtureCommand({ op: "ARM_KEY", nonce });
  await page.evaluate(async ({ sessionId, frame }) => {
    await window.__smoke.input(sessionId, frame);
  }, { sessionId: resources.targetSessionId, frame: arm });
  await waitForPainted(page, resources.targetSessionId, `ARMED:${nonce}`);
  await page.getByTestId(`terminal-slot-${resources.targetSessionId}`).click();
  await expect.poll(() => page.evaluate((sessionId) => window.__smoke.paneFocused(sessionId).focused,
    resources.targetSessionId), { timeout: 10_000, intervals: [50, 100] }).toBe(true);
  await page.evaluate(() => window.__smoke.resetTerminalInputCapture());
  const transport = await page.evaluate((sessionId) => window.__smoke.probeTerminalTransport(sessionId), resources.targetSessionId);
  expect(transport.transport_kind).toBe(carrier);
  expect(transport.pending_input_count).toBe(0);
  expect(transport.worker_epoch).not.toBeNull();
  expect(transport.worker_control_rtt_ms).toBeGreaterThanOrEqual(0);
  if (carrier === "webrtc") expect(transport.candidate_type).toBe("host");
  else expect(transport.candidate_type).toBe("none");
  const timingId = await page.evaluate((sessionId) => window.__smoke.beginTerminalTiming("trusted_key", sessionId), resources.targetSessionId);
  const flood = workload === "visible_peer_flood"
    ? startVisiblePeerFlood(page, resources.floodSessionId, `${runId}-${index}`)
    : null;
  await page.keyboard.press("x");
  const timing = await page.evaluate(({ sessionId, timingId: id, marker }) =>
    window.__smoke.finishTerminalTiming(id, sessionId, marker, 30_000), {
    sessionId: resources.targetSessionId,
    timingId,
    marker: `ACK:${nonce}`,
  });
  expect(timing.trustedKey).toBe(true);
  await expectMarkersOnce(page, resources.targetSessionId, [`ACK:${nonce}`]);
  const floodIntegrity = flood ? await finishVisiblePeerFlood(page, flood) : null;
  const expectedAccepted = flood ? 2 : 1;
  await expect.poll(async () => {
    const capture = await page.evaluate(() => window.__smoke.terminalInputCapture());
    return capture.outcomes.accepted >= expectedAccepted
      && capture.outcomes.rejected === 0
      && capture.outcomes.ambiguous === 0
      ? capture
      : null;
  }, { timeout: 10_000, intervals: [50, 100] }).not.toBeNull();
  const capture = await page.evaluate(() => window.__smoke.terminalInputCapture());
  expect(capture.droppedBatches).toBe(0);
  expect(capture.outcomes.accepted).toBe(expectedAccepted);
  expect(capture.outcomes.rejected).toBe(0);
  expect(capture.outcomes.ambiguous).toBe(0);
  const settledTransport = await page.evaluate((sessionId) => window.__smoke.probeTerminalTransport(sessionId), resources.targetSessionId);
  expect(settledTransport.transport_kind).toBe(carrier);
  expect(settledTransport.pending_input_count).toBe(0);
  return {
    retained: index >= KEY_WARMUPS,
    workload,
    duration_ms: timing.durationMs,
    worker_control_rtt_ms: settledTransport.worker_control_rtt_ms,
    route_kind: settledTransport.transport_kind,
    worker_epoch: settledTransport.worker_epoch,
    candidate_type: settledTransport.candidate_type,
    pending_input_count: settledTransport.pending_input_count,
    outcomes: {
      accepted: capture.outcomes.accepted,
      rejected: capture.outcomes.rejected,
      ambiguous: capture.outcomes.ambiguous,
      capture_dropped: capture.droppedBatches,
    },
    flood: floodIntegrity,
  };
}

function startVisiblePeerFlood(page: Page, sessionId: string, markerStem: string): VisiblePeerFlood {
  const prefix = `PEERPERF-${markerStem}-`;
  const completion = `PEERPERF-DONE-${markerStem}`;
  const frame = encodePtyFixtureCommand({ op: "FLOOD", prefix, count: VISIBLE_PEER_FLOOD_LINES })
    + encodePtyFixtureCommand({ op: "EMIT", text: completion });
  const input = page.evaluate(async ({ targetSessionId, command }) => {
    await window.__smoke.input(targetSessionId, command);
  }, { targetSessionId: sessionId, command: frame });
  return { sessionId, prefix, completion, input };
}

async function finishVisiblePeerFlood(
  page: Page,
  flood: VisiblePeerFlood,
): Promise<NonNullable<Sample["flood"]>> {
  await flood.input;
  await waitForPainted(page, flood.sessionId, flood.completion);
  await expectMarkersOnce(page, flood.sessionId, [flood.completion]);
  const integrity = await page.evaluate(async ({ sessionId, prefix }) =>
    window.__smoke.retainedMarkerScan(sessionId, prefix), {
    sessionId: flood.sessionId,
    prefix: flood.prefix,
  });
  const visible = await page.evaluate(({ sessionId, prefix }) => window.__smoke.markerScan(sessionId, prefix), {
    sessionId: flood.sessionId, prefix: flood.prefix,
  });
  expect(integrity.markerMin).toBe(1);
  expect(integrity.markerMissing).toBe(0);
  expect(integrity.markerDuplicated).toEqual([]);
  expect(integrity.markerOutOfOrder).toBe(0);
  expect(visible).toMatchObject({
    max: VISIBLE_PEER_FLOOD_LINES,
    missing: 0,
    duplicated: [],
    outOfOrder: 0,
  });
  return {
    marker_missing: integrity.markerMissing,
    marker_duplicated: integrity.markerDuplicated.length,
    marker_out_of_order: integrity.markerOutOfOrder,
  };
}

function summarizeScenario(scenario: Scenario, samples: readonly Sample[]): ScenarioReport {
  const retained = samples.filter((sample) => sample.retained);
  expect(retained).toHaveLength(KEY_SAMPLES);
  const durations = retained.map((sample) => sample.duration_ms);
  const controlRtts = retained.map((sample) => sample.worker_control_rtt_ms);
  const byWorkload = Object.fromEntries(WORKLOAD_PLAN.map((workload) => [
    workload,
    distribution(retained.filter((sample) => sample.workload === workload).map((sample) => sample.duration_ms)),
  ])) as Record<Workload, Distribution>;
  return {
    carrier: scenario.carrier,
    location: scenario.location,
    worker_link_one_way_delay_ms: scenario.worker_link_one_way_delay_ms,
    warmups: KEY_WARMUPS,
    retained_samples: retained.length,
    workload_plan: WORKLOAD_PLAN,
    timing: distribution(durations),
    worker_control_rtt: distribution(controlRtts),
    timing_by_workload: byWorkload,
    candidate_types: [...new Set(retained.map((sample) => sample.candidate_type))].sort(),
    integrity: {
      dropout_count: 0,
      ambiguity_count: retained.reduce((total, sample) => total + sample.outcomes.ambiguous, 0),
      rejection_count: retained.reduce((total, sample) => total + sample.outcomes.rejected, 0),
      capture_drop_count: retained.reduce((total, sample) => total + sample.outcomes.capture_dropped, 0),
      painted_ack_count: retained.length,
      flood_marker_missing: retained.reduce((total, sample) => total + (sample.flood?.marker_missing ?? 0), 0),
      flood_marker_duplicated: retained.reduce((total, sample) => total + (sample.flood?.marker_duplicated ?? 0), 0),
      flood_marker_out_of_order: retained.reduce((total, sample) => total + (sample.flood?.marker_out_of_order ?? 0), 0),
    },
    samples: [...samples],
  };
}

function assertDeterministicScenario(report: ScenarioReport): void {
  expect(report.retained_samples).toBe(KEY_SAMPLES);
  expect(report.samples.every((sample) => sample.route_kind === report.carrier)).toBe(true);
  expect(report.samples.every((sample) => sample.pending_input_count === 0)).toBe(true);
  expect(report.integrity).toMatchObject({
    dropout_count: 0,
    ambiguity_count: 0,
    rejection_count: 0,
    capture_drop_count: 0,
    flood_marker_missing: 0,
    flood_marker_duplicated: 0,
    flood_marker_out_of_order: 0,
  });
  if (report.carrier === "webrtc") expect(report.candidate_types).toEqual(["host"]);
  else expect(report.candidate_types).toEqual(["none"]);
}

function distribution(values: readonly number[]): Distribution {
  return {
    p50: percentile(values, 0.5),
    p95: percentile(values, 0.95),
    p99: percentile(values, 0.99),
    max: Math.max(...values),
  };
}

async function attachJson(testInfo: TestInfo, name: string, value: unknown): Promise<void> {
  await testInfo.attach(name, {
    body: JSON.stringify(value, null, 2),
    contentType: "application/json",
  });
}
