// Bottom-follow must survive a layout change that GROWS a wheel-parked pane's
// box. The DOM is frozen while a reader is parked, so a box that grows past the
// frozen content height leaves no scroll range at all: the box can never fire
// another scroll event, and the mutation itself is the only remaining signal
// that the reader has nowhere left to read. These cases pin both halves against
// a real coord + worker + keeper + PTY — a grow that covers the frozen content
// resumes live painting, and a grow that leaves scroll range keeps the reader
// exactly where the gesture parked it.

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
import {
  expectCanonicalAdvanceHeld,
  readTerminalStreamProbe,
  waitForCanonicalAdvance,
} from "./terminal-probe-helpers.ts";
import { sendFixtureCommand } from "./terminal-scale-browser.ts";

/** Both viewports keep min(width, height) >= 600 so windowSizeClass never flips
 *  the app to the compact shell: pane geometry is the only thing that moves. */
const PARK_VIEWPORT = { width: 1440, height: 700 };
/** A burst must not be able to exhaust a few rows of scroll range on its own:
 *  the wheel listener enters reader mode only while the box can still move, and
 *  a fully clamped gesture deliberately stays live. */
const SHALLOW_WHEEL_BURST_PX = 40;
/** One deep burst parks mid-history with range left above and below. */
const DEEP_WHEEL_BURST_PX = 1_200;
/** Height beyond the frozen content so the grown box covers it outright instead
 *  of landing on the boundary. */
const GROW_OVERSHOOT_PX = 80;
/** The measured window delta, kept well inside a deep history's scroll range. */
const RETAINED_RANGE_GROW_PX = 200;
const SHALLOW_OVERFLOW_ROWS = 6;
const DEEP_OVERFLOW_ROWS = 600;
const RESUME_PAINT_BUDGET_MS = 15_000;

/** Park with REAL wheel bursts. A scrollTop write parks with reason
 *  "native_scroll"; every real wheel or touch gesture parks with "wheel" or
 *  "touch", and the resume path must treat all three the same, so the assertion
 *  on the reason is the point of the gesture. */
async function parkByWheel(
  page: Page,
  sessionId: string,
  bursts: number,
  burstPx: number,
): Promise<void> {
  await hoverTerminalGrid(page, sessionId);
  for (let burst = 0; burst < bursts; burst++) await page.mouse.wheel(0, -burstPx);
  await expect.poll(() => readReaderState(page, sessionId), {
    timeout: 15_000,
    intervals: [100, 250],
  }).toEqual({ intent: "reading", reason: "wheel", atBottom: false, followsBottom: false });
}

/** The reader is parked, so the pane must swallow live output: the browser
 *  canonical advances while the painted DOM stays exactly where it froze. */
async function expectFrozenWhileParked(page: Page, sessionId: string): Promise<void> {
  const before = await readTerminalStreamProbe(page, sessionId);
  const heldMarker = uniqueMarker("PARKHELD");
  await sendFixtureCommand(page, sessionId, encodePtyFixtureCommand({ op: "EMIT", text: heldMarker }));
  const pending = await waitForCanonicalAdvance(page, sessionId, before);
  expectCanonicalAdvanceHeld(before, pending, { readerReason: "wheel", selectionHold: false });
  const held = await attemptPaintedMarker(page, sessionId, heldMarker);
  expect(held.proof, "a parked pane painted live output").toBeNull();
}

/** Grow the window and wait for the PTY to answer the new geometry, so the box
 *  mutation has demonstrably been observed before anything is asserted on it. */
async function growWindowHeight(page: Page, sessionId: string, growPx: number): Promise<void> {
  const canonicalRows = async () =>
    (await readTerminalStreamProbe(page, sessionId)).browser.presentation?.rows.canonical ?? 0;
  const parkedRows = await canonicalRows();
  await page.setViewportSize({ width: PARK_VIEWPORT.width, height: PARK_VIEWPORT.height + growPx });
  await expect.poll(canonicalRows, {
    timeout: 30_000,
    intervals: [100, 250],
  }).toBeGreaterThan(parkedRows);
}

async function expectLiveAtBottom(page: Page, sessionId: string, marker: string): Promise<void> {
  const painted = await attemptPaintedMarker(page, sessionId, marker, RESUME_PAINT_BUDGET_MS);
  expect(painted.error, "the pane never painted PTY output emitted after the box grew").toBeNull();
  await expect.poll(() => readReaderState(page, sessionId), {
    timeout: 15_000,
    intervals: [100, 250],
  }).toEqual({ intent: "live", reason: null, atBottom: true, followsBottom: true });
}

test("a wheel-parked pane resumes when the box grows past its frozen content", async ({ smokePage, stack }, testInfo) => {
  test.skip(!testInfo.project.name.startsWith("chromium"), "trusted wheel and desktop scroll geometry");
  test.setTimeout(180_000);
  await smokePage.setViewportSize(PARK_VIEWPORT);
  const worker = await stack.startPtyFixtureWorker();
  const sessionId = await paintFloodAtBottom(smokePage, worker, "GROWFILL-", SHALLOW_OVERFLOW_ROWS);

  const seated = await readRenderProbe(smokePage, sessionId);
  const seatedRange = seated.scrollHeight - seated.clientHeight;
  expect(seatedRange, "flood left the pane with no scroll range to park in").toBeGreaterThan(0);
  await parkByWheel(
    smokePage,
    sessionId,
    Math.ceil(seatedRange / SHALLOW_WHEEL_BURST_PX) + 4,
    SHALLOW_WHEEL_BURST_PX,
  );
  await expect.poll(async () => (await readRenderProbe(smokePage, sessionId)).scrollTop, {
    timeout: 10_000,
    intervals: [50],
  }).toBe(0);
  await expectFrozenWhileParked(smokePage, sessionId);

  const frozen = await readRenderProbe(smokePage, sessionId);
  await growWindowHeight(smokePage, sessionId, frozen.scrollHeight - frozen.clientHeight + GROW_OVERSHOOT_PX);
  // The whole frozen content now fits inside the box, so the box has no scroll
  // range left and can never fire another scroll event.
  const grown = await readRenderProbe(smokePage, sessionId);
  expect(grown.clientHeight, "the window grow did not reach the frozen content height")
    .toBeGreaterThanOrEqual(frozen.scrollHeight);

  const resumeMarker = uniqueMarker("GROWLIVE");
  await sendFixtureCommand(smokePage, sessionId, encodePtyFixtureCommand({ op: "EMIT", text: resumeMarker }));
  await expectLiveAtBottom(smokePage, sessionId, resumeMarker);
});

test("a wheel-parked pane with scroll range left stays parked across a box grow", async ({ smokePage, stack }, testInfo) => {
  test.skip(!testInfo.project.name.startsWith("chromium"), "trusted wheel and desktop scroll geometry");
  test.setTimeout(180_000);
  await smokePage.setViewportSize(PARK_VIEWPORT);
  const worker = await stack.startPtyFixtureWorker();
  const sessionId = await paintFloodAtBottom(smokePage, worker, "KEEPFILL-", DEEP_OVERFLOW_ROWS);

  await parkByWheel(smokePage, sessionId, 1, DEEP_WHEEL_BURST_PX);
  const frozen = await readRenderProbe(smokePage, sessionId);
  expect(frozen.scrollTop, "deep park landed on the history edge").toBeGreaterThan(0);
  expect(frozen.scrollHeight - frozen.clientHeight, "history is too short to outlast the grow")
    .toBeGreaterThan(RETAINED_RANGE_GROW_PX);
  await growWindowHeight(smokePage, sessionId, RETAINED_RANGE_GROW_PX);

  // Range survived the grow, so the reader still has somewhere to read: the
  // box mutation must not move it, and live output must stay swallowed.
  await expectFrozenWhileParked(smokePage, sessionId);
  expect(await readReaderState(smokePage, sessionId))
    .toEqual({ intent: "reading", reason: "wheel", atBottom: false, followsBottom: false });

  const recoveredMarker = uniqueMarker("KEEPLIVE");
  await sendFixtureCommand(smokePage, sessionId, encodePtyFixtureCommand({ op: "EMIT", text: recoveredMarker }));
  await hoverTerminalGrid(smokePage, sessionId);
  await smokePage.mouse.wheel(0, 100_000);
  await expectLiveAtBottom(smokePage, sessionId, recoveredMarker);
});
