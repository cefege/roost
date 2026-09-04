// This callback measures cold terminal navigation and a retained 20k-line PTY flood.
// The perf spec registers it in the quarantined single-file schedule used by Playwright.
// Deterministic retention and frame invariants gate CI while absolute budgets remain opt-in.

import type { Browser, Page, TestInfo } from "@playwright/test";
import type { SmokeApi } from "../../apps/web/src/lib/smoke.ts";
import { test, expect } from "./fixtures.ts";
import { encodePtyFixtureCommand } from "./pty-fixture-protocol.ts";
import type { TerminalTestStack } from "./stack.ts";
import {
  FLOOD_LINES,
  QUALIFY,
  measureFreshNavigation,
  measureNavigation,
  workerFolder,
  type NavigationMeasurement,
} from "./perf-probe-fixture.ts";

type SmokeWindow = Window & { readonly __smoke: SmokeApi };
declare const window: SmokeWindow;
export async function probeNavigationAndFlood(
  { coldSmokePage, browser, stack }: {
    coldSmokePage: Page;
    browser: Browser;
    stack: TerminalTestStack;
  },
  testInfo: TestInfo,
): Promise<void> {
  test.skip(!testInfo.project.name.startsWith("chromium"), "desktop perf budget");
  test.setTimeout(360_000);

  const fixtureWorker = await stack.startPtyFixtureWorker();
  const spawned = await stack.client.sessionsSpawn({
    workerFp: fixtureWorker.workerFp,
    kind: "shell",
    folder: workerFolder(fixtureWorker),
    cols: 80,
    rows: 24,
  });
  const sessionId = spawned.sessionId;

  const cold = await measureNavigation(coldSmokePage, stack.baseUrl, sessionId);
  const fresh: NavigationMeasurement[] = [];
  await coldSmokePage.evaluate((id) => {
    const smoke = window.__smoke;
    smoke.trackCreatedSession(id);
  }, sessionId);
  for (let sample = 0; sample < 5; sample++) {
    fresh.push(await measureFreshNavigation(browser, stack, sessionId));
  }

  const before = await coldSmokePage.evaluate((id) => {
    const smoke = window.__smoke;
    smoke.resetPerfCounters();
    return smoke.perfProbe(id);
  }, sessionId);
  const floodFrame = encodePtyFixtureCommand({ op: "FLOOD", prefix: "PERFLINE-", count: FLOOD_LINES });
  const flood = await coldSmokePage.evaluate(async ({ id, frame, marker }) => {
    const smoke = window.__smoke;
    const started = performance.now();
    await smoke.input(id, frame);
    const proof = await smoke.waitForPaintedMarker(id, marker, 180_000);
    return { wallMs: proof.monotonicMs - started, proof };
  }, { id: sessionId, frame: floodFrame, marker: `PERFLINE-${FLOOD_LINES}` });

  const after = await coldSmokePage.evaluate((id) => {
    const smoke = window.__smoke;
    return smoke.perfProbe(id);
  }, sessionId);
  const scan = await coldSmokePage.evaluate((id) => {
    const smoke = window.__smoke;
    return smoke.markerScan(id, "PERFLINE-");
  }, sessionId);
  // Outside the timed window: page the authoritative retained range without
  // pulling it into the DOM, then prove the exact surviving marker sequence.
  const retained = await coldSmokePage.evaluate((id) => {
    const smoke = window.__smoke;
    return smoke.retainedMarkerScan(id, "PERFLINE-");
  }, sessionId);

  const report = {
    cold_navigation_ms: cold.navigationToPaintMs,
    cold_driver_to_paint_ms: cold.driverToPaintMs,
    fresh_navigation_ms: fresh.map((sample) => sample.navigationToPaintMs),
    flood_wall_ms: flood.wallMs,
    flood_long_tasks: after.longTaskState === "available"
      ? { state: "available", count: after.longTaskCount, ms: after.longTaskMs }
      : { state: "unavailable" },
    flood_cell_frames: after.cellFrames - before.cellFrames,
    flood_cell_full_frames: after.cellFullFrames - before.cellFullFrames,
    retained_floor: retained.retainedFloor,
    retained_cap: retained.retainedCap,
    retained_total: retained.scrollbackTotal,
    retained_marker_min: retained.markerMin,
    retained_marker_max: retained.markerMax,
    dom_nodes: after.domNodes,
    cell_rows: after.cellRows,
    held_sb_rows: after.heldSbRows,
    heap_mb: after.heapMb,
    phases: cold.phaseTimeline,
  };
  console.log(`[perf] ${JSON.stringify(report)}`);
  await testInfo.attach("perf.json", { body: JSON.stringify(report, null, 2), contentType: "application/json" });

  if (after.longTaskState === "available") {
    expect(after.longTaskMs).toBeLessThan(0.5 * flood.wallMs);
  } else {
    expect(report.flood_long_tasks).toEqual({ state: "unavailable" });
  }
  for (const navigation of [cold, ...fresh]) {
    expect(navigation.phaseTimeline.driverBeforeNavigationEpochMs).toBe(
      navigation.driverBeforeGotoEpochMs,
    );
    expect(navigation.driverToPaintMs).toBeGreaterThanOrEqual(navigation.navigationToPaintMs);
  }
  expect(report.flood_cell_full_frames).toBeLessThanOrEqual(3);
  expect(scan).toMatchObject({ max: FLOOD_LINES, duplicated: [], missing: 0, outOfOrder: 0 });
  expect(retained.rowGapCount).toBe(0);
  expect(retained.rowIndices).toHaveLength(retained.retainedCap);
  if (retained.rowIndices.length > 0) {
    expect(retained.rowIndices[0]).toBe(retained.retainedFloor);
    expect(retained.rowIndices.at(-1)).toBe(retained.scrollbackTotal - 1);
  }
  expect(retained.markerDuplicated).toEqual([]);
  expect(retained.markerMissing).toBe(0);
  expect(retained.markerOutOfOrder).toBe(0);
  expect(retained.markerIds).toHaveLength(retained.retainedCap);
  for (let index = 1; index < retained.markerIds.length; index++) {
    expect(retained.markerIds[index]).toBe(retained.markerIds[index - 1]! + 1);
  }
  if (retained.retainedCap < FLOOD_LINES) expect(retained.markerMin).toBeGreaterThan(1);

  if (QUALIFY) {
    expect(cold.navigationToPaintMs).toBeLessThanOrEqual(1_000);
    for (const sample of fresh) expect(sample.navigationToPaintMs).toBeLessThanOrEqual(1_000);
    expect(flood.wallMs).toBeLessThanOrEqual(7_500);
  }
}
