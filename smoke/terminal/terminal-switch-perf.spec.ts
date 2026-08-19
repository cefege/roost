// End-to-end proof of the three switch-latency contracts this suite owns:
// revealing an idle dormant pane emits NOTHING, revealing a pane whose grid
// moved while parked costs exactly ONE viewport-only full frame, and the deck
// keeps a BOUNDED number of panes mounted. Distributions publish on every run;
// absolute millisecond budgets are gated behind ROOST_PERF_QUALIFY=1, because a
// contended box measures the host rather than the product.
//
// Every case is @serial: playwright.config.ts routes /@serial/ to the
// chromium-serial project with fullyParallel:false, and a switch-latency number
// measured while three sibling stacks fight for the same cores is not a
// measurement. The PTY fixture (never a real shell) is equally load-bearing:
// it emits only on command, so "no cell frame arrived" is a statement about the
// product instead of about a shell that happened not to repaint.
//
// Sibling file to perf.spec.ts on purpose — that file sits at its recorded size
// baseline and may only shrink.

import type { Page, TestInfo } from "@playwright/test";
import type { SmokeApi } from "../../apps/web/src/lib/smoke.ts";
import { DECK_WARM_LIMIT } from "../../apps/web/src/lib/deckWarmSet.ts";
import { test, expect } from "./fixtures.ts";
import { encodePtyFixtureCommand, PTY_FIXTURE_READY } from "./pty-fixture-protocol.ts";
import type { TerminalTestWorker } from "./stack.ts";

type SmokeWindow = Window & { readonly __smoke: SmokeApi };
// Playwright serializes callbacks without module closures. This erased binding
// types the browser global once without adding a runtime helper to those callbacks.
declare const window: SmokeWindow;

const QUALIFY = process.env.ROOST_PERF_QUALIFY === "1";

/** apps/shared/src/viewport.ts VIEWER_WITHDRAW_GRACE_MS is 800 ms: the worker
 *  defers a withdraw that long, so a shorter dwell proves nothing about the
 *  unwatched path — emission was still on. 1200 ms clears the grace with margin
 *  while staying far inside the claim TTL. */
const DORMANT_DWELL_MS = 1_200;
/** A reveal's paint proof can resolve off DOM the pane already holds, so a
 *  frame the worker sent anyway could land after the proof. Reading the
 *  counters only after this settle window is what keeps a zero-delta assertion
 *  a statement instead of a race the product wins by being slow. */
const FRAME_SETTLE_MS = 750;
const FLOOD_LINES = 200;

function percentile(values: readonly number[], quantile: number): number {
  if (values.length === 0) return -1;
  const sorted = [...values].sort((left, right) => left - right);
  return sorted[Math.min(sorted.length - 1, Math.floor(quantile * sorted.length))]!;
}

async function navigateAndProve(page: Page, sessionId: string, marker: string): Promise<void> {
  await page.evaluate(({ id, expected }) => {
    const smoke = window.__smoke;
    smoke.navigate(`/s/${id}`);
    return smoke.waitForPaintedMarker(id, expected, 60_000);
  }, { id: sessionId, expected: marker });
}

/** Spawn `count` fixture sessions in ONE folder. Same folder means one pane
 *  with tabs, so switching between them is a park/reveal — the interaction
 *  under measurement — and not a layout change that would mount a second pane
 *  and confound both the frame accounting and the mount bound. */
async function spawnFixtureSessions(
  page: Page,
  worker: TerminalTestWorker,
  count: number,
): Promise<string[]> {
  await page.waitForFunction((fp) => {
    const smoke = window.__smoke;
    return !!smoke.state().workers[fp];
  }, worker.workerFp);
  return page.evaluate(async ({ workerFp, folder, total }) => {
    const smoke = window.__smoke;
    return Promise.all(Array.from({ length: total }, async () =>
      (await smoke.spawnShell(workerFp, folder)).session_id));
  }, {
    workerFp: worker.workerFp,
    // Windows keeper paths arrive with backslashes; the folder key the deck
    // groups panes by is the forward-slash form.
    folder: process.platform === "win32" ? worker.home.replaceAll("\\", "/") : worker.home,
    total: count,
  });
}

/** Emit one marker into a VISIBLE pane and prove it painted. The paint proof
 *  requires visibility, so this is only valid for the pane on screen. */
async function paintMarker(page: Page, sessionId: string, marker: string): Promise<void> {
  await page.evaluate(async ({ id, frame, expected }) => {
    const smoke = window.__smoke;
    await smoke.input(id, frame);
    await smoke.waitForPaintedMarker(id, expected, 60_000);
  }, {
    id: sessionId,
    frame: encodePtyFixtureCommand({ op: "EMIT", text: marker }),
    expected: marker,
  });
}

type CellCounters = { frames: number; fullFrames: number; sbRows: number };

async function readCounters(page: Page, sessionId: string): Promise<CellCounters> {
  return page.evaluate((id) => {
    const smoke = window.__smoke;
    return {
      frames: smoke.cellFrameCount(id),
      fullFrames: smoke.cellFullFrameCount(id),
      sbRows: smoke.lastFullFrameSbRows(id),
    };
  }, sessionId);
}

/** One timed reveal: open the timing endpoint, navigate through the live
 *  router, and finish only on the shared geometric paint proof for `marker`. */
async function timeReveal(page: Page, sessionId: string, marker: string): Promise<number> {
  return page.evaluate(async ({ id, expected }) => {
    const smoke = window.__smoke;
    const timingId = await smoke.beginTerminalTiming("reveal", id);
    smoke.navigate(`/s/${id}`);
    const result = await smoke.finishTerminalTiming(timingId, id, expected, 30_000);
    return result.durationMs;
  }, { id: sessionId, expected: marker });
}

/** Publish unconditionally. A run with QUALIFY off still has to leave its
 *  distribution behind, or a regression is invisible until someone reruns on a
 *  pinned box. */
async function publish(testInfo: TestInfo, report: Record<string, unknown>): Promise<void> {
  console.log(`[perf.switch] ${JSON.stringify(report)}`);
  await testInfo.attach("perf-interaction.json", {
    body: JSON.stringify(report, null, 2),
    contentType: "application/json",
  });
}

test("revealing an idle dormant pane costs zero cell frames @serial", async ({
  smokePage,
  stack,
}, testInfo) => {
  test.skip(!testInfo.project.name.startsWith("chromium"), "desktop dormant reveal accounting");
  test.setTimeout(120_000);

  const fixtureWorker = await stack.startPtyFixtureWorker();
  const [idA, idB] = await spawnFixtureSessions(smokePage, fixtureWorker, 2);
  const suffix = crypto.randomUUID().replaceAll("-", "").slice(0, 12);
  const markerA = `IDLE_A:${suffix}`;
  const markerB = `IDLE_B:${suffix}`;

  await navigateAndProve(smokePage, idA, PTY_FIXTURE_READY);
  await paintMarker(smokePage, idA, markerA);
  await navigateAndProve(smokePage, idB, PTY_FIXTURE_READY);
  await paintMarker(smokePage, idB, markerB);

  // Park B by revealing A, then dwell past the deferred withdraw with NO input
  // to B. After this the worker is genuinely not emitting on B's channel and
  // B's grid is provably unmoved (the fixture writes only on command), so the
  // only thing a reveal could add is the claim snapshot that must not exist.
  await navigateAndProve(smokePage, idA, markerA);
  await smokePage.waitForTimeout(DORMANT_DWELL_MS);
  const before = await readCounters(smokePage, idB);

  // B already holds this marker in its parked DOM; the reveal is what makes it
  // painted again, so this times the visibility flip and nothing else.
  const revealMs = await timeReveal(smokePage, idB, markerB);
  await smokePage.waitForTimeout(FRAME_SETTLE_MS);
  const after = await readCounters(smokePage, idB);

  await publish(testInfo, {
    case: "idle_dormant_reveal",
    dwell_ms: DORMANT_DWELL_MS,
    settle_ms: FRAME_SETTLE_MS,
    reveal_ms: revealMs,
    cell_frames_before: before.frames,
    cell_frames_after: after.frames,
    cell_frame_delta: after.frames - before.frames,
    cell_full_frame_delta: after.fullFrames - before.fullFrames,
  });

  expect(after.frames - before.frames).toBe(0);
  expect(after.fullFrames - before.fullFrames).toBe(0);
  if (QUALIFY) expect(revealMs).toBeLessThanOrEqual(150);
});

test("revealing a pane that moved while parked costs exactly one viewport-only snapshot @serial", async ({
  smokePage,
  stack,
}, testInfo) => {
  test.skip(!testInfo.project.name.startsWith("chromium"), "desktop parked-motion snapshot accounting");
  test.setTimeout(120_000);

  const fixtureWorker = await stack.startPtyFixtureWorker();
  const [idA, idB] = await spawnFixtureSessions(smokePage, fixtureWorker, 2);
  const suffix = crypto.randomUUID().replaceAll("-", "").slice(0, 12);
  const markerA = `MOVED_A:${suffix}`;
  const floodPrefix = `SWITCHLINE-${suffix}-`;
  const lastFloodMarker = `${floodPrefix}${FLOOD_LINES}`;

  await navigateAndProve(smokePage, idB, PTY_FIXTURE_READY);
  await navigateAndProve(smokePage, idA, PTY_FIXTURE_READY);
  await paintMarker(smokePage, idA, markerA);
  await smokePage.waitForTimeout(DORMANT_DWELL_MS);
  const parked = await readCounters(smokePage, idB);

  // Input goes through the transport (sendTerminalInput), not through B's
  // renderer, so these rows land on a pane that is parked and withdrawn: they
  // exist only on the worker until the reveal pulls them. That is what makes
  // this the "grid moved while unwatched" branch instead of deltas the client
  // already applied. B is hidden, so there is no painted marker to wait on —
  // wait for the transport to settle, then read the counters.
  await smokePage.evaluate(async ({ id, frame }) => {
    const smoke = window.__smoke;
    await smoke.input(id, frame);
  }, {
    id: idB,
    frame: encodePtyFixtureCommand({ op: "FLOOD", prefix: floodPrefix, count: FLOOD_LINES }),
  });
  await smokePage.waitForTimeout(1_000);
  const before = await readCounters(smokePage, idB);

  const revealMs = await timeReveal(smokePage, idB, lastFloodMarker);
  await smokePage.waitForTimeout(FRAME_SETTLE_MS);
  const after = await readCounters(smokePage, idB);

  await publish(testInfo, {
    case: "parked_motion_reveal",
    flood_lines: FLOOD_LINES,
    settle_ms: FRAME_SETTLE_MS,
    reveal_ms: revealMs,
    parked_flood_frame_delta: before.frames - parked.frames,
    parked_flood_full_frame_delta: before.fullFrames - parked.fullFrames,
    cell_frame_delta: after.frames - before.frames,
    cell_full_frame_delta: after.fullFrames - before.fullFrames,
    last_full_frame_sb_rows: after.sbRows,
  });

  expect(after.fullFrames - before.fullFrames).toBe(1);
  // SB_SNAPSHOT_HISTORY_ROWS = 0: the authoritative frame carries the viewport
  // and nothing else, so history stays demand-pulled.
  expect(after.sbRows).toBe(0);
  if (QUALIFY) expect(revealMs).toBeLessThanOrEqual(300);
});

test("the deck mounts a bounded number of panes @serial", async ({
  smokePage,
  stack,
}, testInfo) => {
  test.skip(!testInfo.project.name.startsWith("chromium"), "desktop deck mount bound");
  test.setTimeout(240_000);

  const fixtureWorker = await stack.startPtyFixtureWorker();
  const sessionIds = await spawnFixtureSessions(smokePage, fixtureWorker, 16);
  for (const sessionId of sessionIds) await navigateAndProve(smokePage, sessionId, PTY_FIXTURE_READY);

  // One folder is one pane, so exactly one session is slotted and the warm set
  // adds at most DECK_WARM_LIMIT more. Unbounded warmth would leave all 16
  // mounted and make every future switch pay their forced layout.
  const mountedSlots = await smokePage.locator('[data-testid^="terminal-slot-"]').count();

  // The two most recently visited sessions are the two most recently slotted,
  // so they are the two the warm policy can never evict — alternating between
  // them measures the steady-state switch cost at the mount ceiling.
  const older = sessionIds.at(-2)!;
  const newer = sessionIds.at(-1)!;
  const revealSamples: number[] = [];
  for (let sample = 0; sample < 20; sample++) {
    revealSamples.push(await timeReveal(smokePage, sample % 2 === 0 ? older : newer, PTY_FIXTURE_READY));
  }
  const p50 = percentile(revealSamples, 0.5);
  const p95 = percentile(revealSamples, 0.95);

  await publish(testInfo, {
    case: "bounded_deck",
    sessions_spawned: sessionIds.length,
    mounted_slots: mountedSlots,
    mount_ceiling: 1 + DECK_WARM_LIMIT,
    alternating_reveal_ms: revealSamples,
    alternating_reveal_p50_ms: p50,
    alternating_reveal_p95_ms: p95,
  });

  expect(mountedSlots).toBeLessThanOrEqual(1 + DECK_WARM_LIMIT);
  if (QUALIFY) expect(p95).toBeLessThanOrEqual(150);
});
