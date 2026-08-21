import type { Page, TestInfo } from "@playwright/test";
import type { SmokeApi } from "../../apps/web/src/lib/smoke.ts";
import { encodePtyFixtureCommand } from "./pty-fixture-protocol.ts";
import type { TerminalTestWorker } from "./stack.ts";

type SmokeWindow = Window & { readonly __smoke: SmokeApi };
// Playwright serializes callbacks without module closures. This erased binding
// types the browser global once without adding a runtime helper to those callbacks.
declare const window: SmokeWindow;

export const QUALIFY = process.env.ROOST_PERF_QUALIFY === "1";

/** Allow explicit inactive membership and the worker emission gate to settle
 * before measuring the next active-view baseline. */
export const INACTIVE_SETTLE_MS = 1_200;
/** A paint proof can resolve from DOM the pane already holds. Reading transport
 * counters only after this window captures the complete baseline that follows
 * activation instead of racing its delivery. */
export const FRAME_SETTLE_MS = 750;
export const FLOOD_LINES = 200;

export function percentile(values: readonly number[], quantile: number): number {
  if (values.length === 0) return -1;
  const sorted = [...values].sort((left, right) => left - right);
  return sorted[Math.min(sorted.length - 1, Math.floor(quantile * sorted.length))]!;
}

export async function navigateAndProve(page: Page, sessionId: string, marker: string): Promise<void> {
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
export async function spawnFixtureSessions(
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
export async function paintMarker(page: Page, sessionId: string, marker: string): Promise<void> {
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

export type CellCounters = {
  frames: number;
  fullFrames: number;
  sbRows: number;
  backfillRequests: number;
  droppedFrames: number;
};

export async function readCounters(page: Page, sessionId: string): Promise<CellCounters> {
  return page.evaluate((id) => {
    const smoke = window.__smoke;
    return {
      frames: smoke.cellFrameCount(id),
      fullFrames: smoke.cellFullFrameCount(id),
      sbRows: smoke.lastFullFrameSbRows(id),
      backfillRequests: smoke.scrollbackBackfillRequestCount(id),
      droppedFrames: smoke.droppedCellFrameCount(id),
    };
  }, sessionId);
}

/** One timed reveal: open the timing endpoint, navigate through the live
 *  router, and finish only on the shared geometric paint proof for `marker`. */
export async function timeReveal(page: Page, sessionId: string, marker: string): Promise<number> {
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
export async function publish(testInfo: TestInfo, report: Record<string, unknown>): Promise<void> {
  console.log(`[perf.switch] ${JSON.stringify(report)}`);
  await testInfo.attach("perf-interaction.json", {
    body: JSON.stringify(report, null, 2),
    contentType: "application/json",
  });
}
