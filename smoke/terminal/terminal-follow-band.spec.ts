// A reader within a two-row follow band of the live tail is still riding it:
// the pane must keep painting, keep pinning to the new bottom, and self-resume
// when a small gesture comes to rest inside the band. A deliberate gesture past
// the band must still park and still freeze the DOM. These cases pin all three
// against a real coord + worker + keeper + PTY with trusted wheel input, which
// is the only place Chromium's own scroll animation participates.

import type { Page } from "@playwright/test";
import { expect, test } from "./fixtures.ts";
import { encodePtyFixtureCommand } from "./pty-fixture-protocol.ts";
import {
  hoverTerminalGrid,
  paintFloodAtBottom,
  readReaderState,
  readRenderProbe,
  uniqueMarker,
} from "./terminal-helpers.ts";
import { attemptPaintedMarker } from "./terminal-paint-helpers.ts";
import { sendFixtureCommand } from "./terminal-scale-browser.ts";

/** min(width, height) >= 600 keeps windowSizeClass on the desktop shell. */
const BAND_VIEWPORT = { width: 1440, height: 700 };
/** Deep enough that every gesture below has scroll range to move through. */
const FOLLOW_OVERFLOW_ROWS = 40;
/** Smaller than the two-row band at any real row height: the micro-gesture
 *  that must never cost the reader its stream. */
const IN_BAND_FLICK_PX = 20;
/** One deliberate read, far outside the band. */
const DEEP_WHEEL_BURST_PX = 1_200;
/** Well past BOTTOM_FOLLOW_SETTLE_MS, so a band settle that was going to fire
 *  has already fired when the park is re-read. */
const SETTLE_OBSERVATION_MS = 1_000;
const PAINT_BUDGET_MS = 15_000;
const READER_POLL = { timeout: 15_000, intervals: [100, 250] };

async function emitFixtureMarker(page: Page, sessionId: string, label: string): Promise<string> {
  const marker = uniqueMarker(label);
  await sendFixtureCommand(page, sessionId, encodePtyFixtureCommand({ op: "EMIT", text: marker }));
  return marker;
}

/** Place the reader exactly one painted row above the clamp and deliver the
 *  scroll the browser would: one row is inside the band, so this is the jitter
 *  a fractional clamp or a flick that lands short produces. */
async function nudgeOneRowOffBottom(
  page: Page,
  sessionId: string,
): Promise<{ rowPx: number; fromBottom: number }> {
  const nudged = await page.evaluate((id) => {
    const grid = document
      .querySelector(`[data-testid="terminal-slot-${id}"]`)
      ?.querySelector(".cell-grid") as HTMLElement | null;
    if (!grid) return null;
    const rowPx = grid.querySelector(".cell-row")?.getBoundingClientRect().height ?? 0;
    if (rowPx <= 0) return null;
    grid.scrollTop = Math.max(0, grid.scrollHeight - grid.clientHeight) - rowPx;
    grid.dispatchEvent(new Event("scroll"));
    return { rowPx, fromBottom: grid.scrollHeight - grid.clientHeight - grid.scrollTop };
  }, sessionId);
  expect(nudged, "terminal grid has no painted rows to measure").not.toBeNull();
  return nudged!;
}

test("a follow-band reader keeps streaming, self-resumes, and still parks past the band", async ({ smokePage, stack }, testInfo) => {
  test.skip(!testInfo.project.name.startsWith("chromium"), "trusted wheel and desktop scroll geometry");
  test.setTimeout(180_000);
  await smokePage.setViewportSize(BAND_VIEWPORT);
  const worker = await stack.startPtyFixtureWorker();
  const sessionId = await paintFloodAtBottom(smokePage, worker, "BANDFILL-", FOLLOW_OVERFLOW_ROWS);

  // A one-row offset is inside the band: the pane stays live and keeps painting.
  const nudged = await nudgeOneRowOffBottom(smokePage, sessionId);
  expect(nudged.fromBottom, "the nudge did not leave the bottom clamp").toBeGreaterThan(0);
  expect(await readReaderState(smokePage, sessionId))
    .toEqual({ intent: "live", reason: null, atBottom: false, followsBottom: true });

  const bandMarker = await emitFixtureMarker(smokePage, sessionId, "BANDLIVE");
  const bandPaint = await attemptPaintedMarker(smokePage, sessionId, bandMarker, PAINT_BUDGET_MS);
  expect(bandPaint.error, "a reader one row off the tail stopped painting").toBeNull();
  // The band pin re-seated the reader on the new bottom instead of drifting.
  await expect.poll(async () => (await readRenderProbe(smokePage, sessionId)).atBottom, READER_POLL)
    .toBe(true);

  // A REAL trusted flick smaller than the band parks eagerly, then the pane's
  // scroll-idle settle resumes it without the reader doing anything.
  await hoverTerminalGrid(smokePage, sessionId);
  await smokePage.mouse.wheel(0, -IN_BAND_FLICK_PX);
  const flickMarker = await emitFixtureMarker(smokePage, sessionId, "BANDFLICK");
  await expect.poll(() => readReaderState(smokePage, sessionId), READER_POLL)
    .toEqual({ intent: "live", reason: null, atBottom: true, followsBottom: true });
  const flickPaint = await attemptPaintedMarker(smokePage, sessionId, flickMarker, PAINT_BUDGET_MS);
  expect(flickPaint.error, "the settled pane never painted output emitted after the flick").toBeNull();

  // A deliberate gesture past the band is a read: it parks, it stays parked
  // across the settle window, and the DOM stays frozen while it holds.
  await smokePage.mouse.wheel(0, -DEEP_WHEEL_BURST_PX);
  await expect.poll(() => readReaderState(smokePage, sessionId), READER_POLL)
    .toEqual({ intent: "reading", reason: "wheel", atBottom: false, followsBottom: false });
  const parkedMarker = await emitFixtureMarker(smokePage, sessionId, "BANDPARK");
  await smokePage.waitForTimeout(SETTLE_OBSERVATION_MS);
  expect(await readReaderState(smokePage, sessionId))
    .toEqual({ intent: "reading", reason: "wheel", atBottom: false, followsBottom: false });
  const parkedPaint = await attemptPaintedMarker(smokePage, sessionId, parkedMarker);
  expect(parkedPaint.proof, "the band swallowed a deliberate read").toBeNull();

  // Returning to the tail releases the park and flushes what it withheld.
  await smokePage.mouse.wheel(0, 100_000);
  const resumedPaint = await attemptPaintedMarker(smokePage, sessionId, parkedMarker, PAINT_BUDGET_MS);
  expect(resumedPaint.error, "the resumed pane never painted the withheld output").toBeNull();
  await expect.poll(() => readReaderState(smokePage, sessionId), READER_POLL)
    .toEqual({ intent: "live", reason: null, atBottom: true, followsBottom: true });
});
