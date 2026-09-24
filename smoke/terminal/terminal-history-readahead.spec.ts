// Pins the history pager's READ-AHEAD contract: the reader pays one demand
// round trip when it first leaves the painted tail, and the viewport steps that
// a wave pre-paid cross painted rows for free. Drives the real stack through
// the PTY fixture worker and the smoke probes (hasPaintedScrollbackRange,
// scrollbackBackfillRequestCount). How many steps are pre-paid is derived from
// the pane geometry this host rendered and the pager's own exported page sizes;
// paint latency is reported, never gated.

import type { Page, TestInfo } from "@playwright/test";
// The pager's own sizes, never restated here: a gate that hard-codes the value
// it is testing stops testing it, so a retune would either fail a correct pager
// or pass a broken one. This module is pure arithmetic — no DOM, no wire.
import {
  BACKFILL_AHEAD_ROWS,
  BACKFILL_FETCH_ROWS,
} from "../../apps/web/src/client/terminal-stream/scrollbackDemandBounds.ts";
import { expect, test } from "./fixtures.ts";
import { encodePtyFixtureCommand } from "./pty-fixture-protocol.ts";
import {
  hoverTerminalGrid,
  navigateToSmokeSession,
  spawnPtyFixtureSession,
  uniqueMarker,
  waitForStableCellFrames,
} from "./terminal-helpers.ts";
import { sendFixtureCommand } from "./terminal-scale-browser.ts";
import { percentile } from "./terminal-switch-perf-helpers.ts";

const FLOOD_ROWS = 5_000;
/** Pages one wave fetches before it rests: the page covering the rows this
 *  wheel exposed, plus enough pages to cover the trigger band that reaches
 *  BACKFILL_AHEAD_ROWS above the reader — the rearm loop in
 *  apps/web/src/renderer/scrollbackBackfill.ts relaunches until the band holds no
 *  missing row. */
const PAGER_WAVE_PAGES = 1 + Math.ceil(BACKFILL_AHEAD_ROWS / BACKFILL_FETCH_ROWS);
/** Bounds the walk's runtime — each step costs a wheel and a settle poll. A
 *  pane pre-paid for more steps than this walks and asserts only this many, and
 *  the summary publishes both counts so a truncated gate is visible. */
const MAX_PREPAID_STEPS_WALKED = 6;
/** Steps walked past the pre-paid lead so the log shows where it ended. */
const REPORTED_TAIL_STEPS = 1;
/** The pager is at rest once its request count holds still for longer than the
 *  BACKFILL_RETRY_MS cadence a stalled page retries on (2s in
 *  apps/web/src/renderer/scrollbackBackfill.ts), so no rearm hides in the window. */
const PAGER_SETTLE_SAMPLE_MS = 250;
const PAGER_SETTLE_STABLE_POLLS = 10;
const PAGER_SETTLE_TIMEOUT_MS = 90_000;
/** Deliberately far above any plausible paint: the gate here is coverage, and a
 *  paint budget must never be what fails on a loaded box. */
const STEP_PAINT_BUDGET_MS = 30_000;

interface HistoryScrollGeometry {
  /** Absolute row count the history layout reaches: the scrollback total. */
  total: number;
  rowHeightPx: number;
  clientHeightPx: number;
  scrollTop: number;
  /** Where one viewport of upward wheel lands. */
  nextScrollTop: number;
  /** Half-open absolute history rows that scroll position will show. */
  exposedStart: number;
  exposedEnd: number;
  paintedBeforeWheel: boolean;
}

interface HistoryScrollStep {
  step: number;
  exposed_start: number;
  exposed_end: number;
  exposed_rows: number;
  painted_before_wheel: boolean;
  painted_ms: number;
  /** Settled request count before the wheel: this step's RPC baseline. */
  rpc_before: number;
  /** Demands the settled chain this wheel started cost the reader. */
  rpc_delta: number;
  /** The same count sampled at paint, before the chain settled. */
  rpc_delta_at_paint: number;
  scroll_top_before: number;
  scroll_top_after: number;
}

/** Geometry plus the window one upward viewport will expose, read in ONE hop so
 *  the pre-wheel state and the target range cannot disagree.
 *
 *  The renderer lays every absolute row — painted or reserved — at
 *  spacerTop + row * rowHeight, and a non-bottom backfill splice writes no
 *  scroll position, so a range computed from the post-wheel scrollTop stays the
 *  range that scroll position shows while the demand resolves. */
function readHistoryScrollGeometry(
  page: Page,
  sessionId: string,
): Promise<HistoryScrollGeometry> {
  return page.evaluate((id) => {
    const container = document.querySelector(`[data-testid="terminal-slot-${id}"] .wterm`);
    if (!(container instanceof HTMLElement)) {
      throw new Error(`session ${id} has no terminal scroll container`);
    }
    const spacer = container.querySelector<HTMLElement>(".cell-sb-spacer");
    if (!spacer) throw new Error(`session ${id} lays out no history spacer`);
    const viewportRow = container.querySelector<HTMLElement>(".cell-viewport .cell-row");
    const rowHeightPx = viewportRow?.getBoundingClientRect().height ?? 0;
    if (rowHeightPx <= 0) throw new Error(`session ${id} painted no measurable row`);
    const last = container.querySelector(".cell-scrollback")?.lastElementChild as
      | HTMLElement
      | null;
    if (!last) throw new Error(`session ${id} lays out no history`);
    const total = last.classList.contains("cell-sb-gap")
      ? Number(last.dataset.endRow)
      : Number((last.lastElementChild as HTMLElement | null)?.dataset.rowIndex) + 1;
    if (!Number.isSafeInteger(total) || total <= 0) {
      throw new Error(`session ${id} history layout names no absolute end`);
    }
    const spacerTop = spacer.offsetTop;
    const clientHeightPx = container.clientHeight;
    const scrollTop = container.scrollTop;
    const nextScrollTop = Math.max(0, scrollTop - clientHeightPx);
    const exposedStart = Math.max(
      0,
      Math.floor((nextScrollTop - spacerTop) / rowHeightPx),
    );
    const exposedEnd = Math.min(
      total,
      Math.ceil((nextScrollTop + clientHeightPx - spacerTop) / rowHeightPx),
    );
    return {
      total,
      rowHeightPx,
      clientHeightPx,
      scrollTop,
      nextScrollTop,
      exposedStart,
      exposedEnd,
      paintedBeforeWheel: exposedEnd > exposedStart
        && window.__smoke.hasPaintedScrollbackRange(id, exposedStart, exposedEnd),
    };
  }, sessionId);
}

function readScrollState(page: Page, sessionId: string): Promise<{ scrollTop: number; requests: number }> {
  return page.evaluate((id) => {
    const container = document.querySelector(`[data-testid="terminal-slot-${id}"] .wterm`);
    if (!(container instanceof HTMLElement)) {
      throw new Error(`session ${id} has no terminal scroll container`);
    }
    return {
      scrollTop: Math.round(container.scrollTop),
      requests: window.__smoke.scrollbackBackfillRequestCount(id),
    };
  }, sessionId);
}

/** The request count once the pager stops issuing demands, which is also the
 *  next step's RPC baseline.
 *
 *  One wheel raises one wave and every settle re-derives from live scroll
 *  state, so the chain can still be paging after the rows the reader waited on
 *  are painted. Sampling until the count holds still — never a fixed sleep —
 *  is what makes each step measure a settled pager on a contended host. */
async function waitForSettledPagerRequests(page: Page, sessionId: string): Promise<number> {
  let previous = -1;
  let unchangedPolls = 0;
  await expect.poll(async () => {
    const requests = await page.evaluate(
      (id) => window.__smoke.scrollbackBackfillRequestCount(id),
      sessionId,
    );
    if (requests === previous) unchangedPolls += 1;
    else {
      previous = requests;
      unchangedPolls = 0;
    }
    return unchangedPolls;
  }, {
    timeout: PAGER_SETTLE_TIMEOUT_MS,
    intervals: [PAGER_SETTLE_SAMPLE_MS],
    message: "history pager never stopped issuing demands between wheel steps",
  }).toBeGreaterThanOrEqual(PAGER_SETTLE_STABLE_POLLS);
  return previous;
}

interface PrepaidStepPlan {
  /** The pager constants this plan was derived from, so a CI log alone says
   *  which sizes produced the step count that failed. */
  pager_fetch_rows: number;
  pager_ahead_rows: number;
  /** Pages step 1's read-ahead chain must issue before the pager rests. */
  wave_pages: number;
  rows_per_step: number;
  prepaid_lead_rows: number;
  /** Steps the geometry says the pager pre-paid, before the walk bound. */
  prepaid_steps_derived: number;
  /** Steps this run actually walks and asserts as pre-paid. */
  prepaid_steps: number;
  /** True when the walk bound asserts less than the geometry derived. */
  prepaid_assertion_truncated: boolean;
  total_steps: number;
}

/** How many steps after the first the pager already owes the reader, from the
 *  geometry this host rendered rather than a hard-coded step count.
 *
 *  A wave paints PAGER_WAVE_PAGES pages below the reader, of which the trigger
 *  band already covers BACKFILL_AHEAD_ROWS, so the lead past the band is
 *  PAGER_WAVE_PAGES * BACKFILL_FETCH_ROWS - BACKFILL_AHEAD_ROWS - rowsPerStep
 *  rows and step k re-arms the pager as soon as k * rowsPerStep passes it. A
 *  partial row still moves the reader, so rounding the step UP can only
 *  understate the lead — a floor on the pre-paid steps, never a ceiling. */
function derivePrepaidStepPlan(geometry: HistoryScrollGeometry): PrepaidStepPlan {
  const rowsPerStep = Math.max(1, Math.ceil(geometry.clientHeightPx / geometry.rowHeightPx));
  const prepaidLeadRows = PAGER_WAVE_PAGES * BACKFILL_FETCH_ROWS
    - BACKFILL_AHEAD_ROWS - rowsPerStep;
  const derivedSteps = Math.max(1, Math.floor(prepaidLeadRows / rowsPerStep));
  const prepaidSteps = Math.min(MAX_PREPAID_STEPS_WALKED, derivedSteps);
  return {
    pager_fetch_rows: BACKFILL_FETCH_ROWS,
    pager_ahead_rows: BACKFILL_AHEAD_ROWS,
    wave_pages: PAGER_WAVE_PAGES,
    rows_per_step: rowsPerStep,
    prepaid_lead_rows: prepaidLeadRows,
    prepaid_steps_derived: derivedSteps,
    prepaid_steps: prepaidSteps,
    prepaid_assertion_truncated: prepaidSteps < derivedSteps,
    total_steps: 1 + prepaidSteps + REPORTED_TAIL_STEPS,
  };
}

/** Publish unconditionally: a probe that leaves no distribution behind cannot
 *  be compared against the next run. */
async function publishHistoryLatency(
  testInfo: TestInfo,
  report: Record<string, unknown>,
): Promise<void> {
  await testInfo.attach("history-latency.json", {
    body: JSON.stringify(report, null, 2),
    contentType: "application/json",
  });
}

test("read-ahead pre-pays the history a reader scrolls into", async ({
  smokePage,
  stack,
}, testInfo) => {
  test.skip(!testInfo.project.name.startsWith("chromium"), "trusted wheel requires Chromium");
  test.setTimeout(480_000);

  const fixtureWorker = await stack.startPtyFixtureWorker();
  const sessionId = await spawnPtyFixtureSession(smokePage, fixtureWorker);
  await navigateToSmokeSession(smokePage, sessionId);
  await waitForStableCellFrames(smokePage, sessionId);
  const gridRows = await smokePage.evaluate(
    (id) => window.__smoke.terminalDimensions(id).rows,
    sessionId,
  );
  expect(gridRows, "fixture pane painted no grid rows").toBeGreaterThan(0);

  const prefix = `${uniqueMarker("HISTLAT")}-`;
  const floodStartedAt = performance.now();
  await sendFixtureCommand(
    smokePage,
    sessionId,
    encodePtyFixtureCommand({ op: "FLOOD", prefix, count: FLOOD_ROWS }),
  );
  await expect.poll(() => smokePage.evaluate(({ id, marker }) => ({
    max: window.__smoke.markerScan(id, marker).max,
    atBottom: window.__smoke.renderProbe(id).atBottom,
  }), { id: sessionId, marker: prefix }), { timeout: 120_000, intervals: [250] })
    .toEqual({ max: FLOOD_ROWS, atBottom: true });
  await waitForStableCellFrames(smokePage, sessionId);
  const floodMs = performance.now() - floodStartedAt;

  // DOM depth alone cannot see upstream loss: the renderer holds a bounded
  // window, so the flood is only "in history" once the retained range pages
  // back to row 0 with every marker present.
  const retained = await smokePage.evaluate(
    ({ id, marker }) => window.__smoke.retainedMarkerScan(id, marker),
    { id: sessionId, marker: prefix },
  );
  expect(retained).toMatchObject({
    markerMin: 1,
    markerMissing: 0,
    markerDuplicated: [],
    retainedFloor: 0,
    retainedFloorReason: "none",
  });
  expect(retained.scrollbackTotal + gridRows).toBeGreaterThanOrEqual(FLOOD_ROWS);

  await hoverTerminalGrid(smokePage, sessionId);
  const steps: HistoryScrollStep[] = [];
  const paneGeometry = await readHistoryScrollGeometry(smokePage, sessionId);
  const plan = derivePrepaidStepPlan(paneGeometry);
  console.log(`[perf.history-latency] ${JSON.stringify({
    case: "history_scroll_plan",
    row_height_px: paneGeometry.rowHeightPx,
    viewport_px: paneGeometry.clientHeightPx,
    ...plan,
  })}`);
  let settledRequests = await waitForSettledPagerRequests(smokePage, sessionId);
  for (let step = 1; step <= plan.total_steps; step++) {
    const baselineRequests = settledRequests;
    const geometry = await readHistoryScrollGeometry(smokePage, sessionId);
    expect(
      geometry.exposedEnd,
      `step ${step} exposed no history rows above scrollTop ${geometry.scrollTop}`,
    ).toBeGreaterThan(geometry.exposedStart);

    const wheelPx = Math.round(geometry.clientHeightPx);
    const startedAt = performance.now();
    await smokePage.mouse.wheel(0, -wheelPx);
    await smokePage.waitForFunction(
      ({ id, start, end }) => window.__smoke.hasPaintedScrollbackRange(id, start, end),
      { id: sessionId, start: geometry.exposedStart, end: geometry.exposedEnd },
      { timeout: STEP_PAINT_BUDGET_MS, polling: "raf" },
    );
    const paintedMs = performance.now() - startedAt;
    const after = await readScrollState(smokePage, sessionId);
    // A demand this wheel raised can be issued after the paint the reader
    // waited on, so the step's cost is only whole once its chain is at rest —
    // and that same count is the next step's baseline.
    settledRequests = await waitForSettledPagerRequests(smokePage, sessionId);

    steps.push({
      step,
      exposed_start: geometry.exposedStart,
      exposed_end: geometry.exposedEnd,
      exposed_rows: geometry.exposedEnd - geometry.exposedStart,
      painted_before_wheel: geometry.paintedBeforeWheel,
      painted_ms: Math.round(paintedMs * 100) / 100,
      rpc_before: baselineRequests,
      rpc_delta: settledRequests - baselineRequests,
      rpc_delta_at_paint: after.requests - baselineRequests,
      scroll_top_before: Math.round(geometry.scrollTop),
      scroll_top_after: after.scrollTop,
    });
    console.log(`[perf.history-latency] ${JSON.stringify(steps.at(-1))}`);
  }

  const paintedMsValues = steps.map((entry) => entry.painted_ms);
  const summary = {
    case: "history_scroll_latency",
    flood_rows: FLOOD_ROWS,
    flood_ms: Math.round(floodMs),
    grid_rows: gridRows,
    scrollback_total: retained.scrollbackTotal,
    row_height_px: paneGeometry.rowHeightPx,
    viewport_px: paneGeometry.clientHeightPx,
    steps: plan.total_steps,
    step_budget_ms: STEP_PAINT_BUDGET_MS,
    per_step_ms: paintedMsValues,
    per_step_rpcs: steps.map((entry) => entry.rpc_delta),
    median_ms: percentile(paintedMsValues, 0.5),
    max_ms: Math.max(...paintedMsValues),
    total_rpcs: steps.reduce((sum, entry) => sum + entry.rpc_delta, 0),
    steps_crossing_unpainted_history: steps.filter((entry) => !entry.painted_before_wheel).length,
    ...plan,
  };
  console.log(`[perf.history-latency] ${JSON.stringify(summary)}`);
  await publishHistoryLatency(testInfo, { summary, plan, steps });

  expect(steps).toHaveLength(plan.total_steps);
  // A viewport-only checkpoint leaves the flood's history unpainted, so the
  // first step off the tail must really cross a gap.
  expect(
    steps[0]?.painted_before_wheel,
    "step 1 crossed already-painted rows: this run exercised no read-ahead at all",
  ).toBe(false);
  // The lead the pre-paid steps spend is only real if step 1's chain actually
  // fetched it, so pin the chain's depth against the pager's own sizes.
  expect(
    steps[0]?.rpc_delta,
    `step 1's read-ahead chain must settle after ${plan.wave_pages} demands: one page for the `
      + `rows it exposed plus the ${plan.pager_ahead_rows}-row band above the reader at `
      + `${plan.pager_fetch_rows} rows per page`,
  ).toBe(plan.wave_pages);
  const prepaid = steps.slice(1, 1 + plan.prepaid_steps);
  expect(
    prepaid.map((entry) => entry.painted_before_wheel),
    `steps 2-${1 + plan.prepaid_steps} land inside the ${plan.prepaid_lead_rows}-row lead one `
      + `wave pre-pays at ${plan.rows_per_step} rows per step (${plan.prepaid_steps_derived} `
      + `derived, ${plan.prepaid_steps} walked), so their rows must already be painted when the `
      + `wheel turns; steps past ${1 + plan.prepaid_steps} are reported only`,
  ).toEqual(prepaid.map(() => true));
  expect(
    prepaid.map((entry) => entry.rpc_delta),
    `a pre-paid step must cost the reader no demand round trip: steps 2-${1 + plan.prepaid_steps} `
      + `read rows the wave from step 1 already fetched (${plan.prepaid_lead_rows} rows of lead)`,
  ).toEqual(prepaid.map(() => 0));
});
