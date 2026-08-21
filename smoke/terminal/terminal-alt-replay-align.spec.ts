import { test, expect } from "./fixtures.ts";
import type { Page } from "@playwright/test";
import { encodePtyFixtureCommand, PTY_FIXTURE_READY } from "./pty-fixture-protocol.ts";
import type { RecoverySmokeApi } from "./terminal-smoke-api.ts";
import {
  spawnPtyFixtureSession,
  navigateToSmokeSession,
  inputSmokeTerminal,
  waitForStableCellFrames,
} from "./terminal-helpers.ts";

// A full-screen TUI's alternate buffer must survive an in-place terminal
// resize. Real TUIs repaint differentially after SIGWINCH: unchanged labels,
// borders and footer cells are not resent, so replacing the core would leave a
// mostly blank screen even while changing gauges continue to update.
//
// The stream also keeps the parser-alignment regression live. Each paced chunk
// ends with a bare `ESC [`; the next chunk begins with its SGR parameters and a
// numeric update. A resize boundary can split the sequence exactly as the
// production htop failure did. The existing core must preserve the static
// image and parked parser state while applying only the differential tail.

const VALUE_ROW = 4;
const VALUE_COL = 19;
const PERCENT_ROW = 3;
const PERCENT_COL = 19;
const ACTIVITY_ROW = 6;
const ACTIVITY_COL = 40;
const FOOTER_ROW = 12;
// Under the fixture's MAX_PENDING_WORK (32), so its stdin never pauses and the
// worker's PTY write never blocks behind the pacing below.
const BURST_CHUNKS = 24;
const BURST_CHUNK_DELAY_MS = 2;

/** One paced batch of mid-sequence-split differential updates. Each command
 *  paints numeric cells only, then leaves a bare `ESC [` for the next command. */
function splitSequenceBurst(
  batch: number,
  valueToken: string,
  percentToken: string,
): string {
  let frames = "";
  for (let index = 0; index < BURST_CHUNKS; index++) {
    const activity = String((batch + index) % 10);
    frames += encodePtyFixtureCommand({
      op: "EMIT",
      newline: false,
      delayMs: BURST_CHUNK_DELAY_MS,
      text: `32m${valueToken}\x1b[0m`
        + `\x1b[${PERCENT_ROW};${PERCENT_COL}H${percentToken}`
        + `\x1b[${ACTIVITY_ROW};${ACTIVITY_COL}H${activity}`
        + `\x1b[${VALUE_ROW};${VALUE_COL}H\x1b[`,
    });
  }
  return frames;
}

function numericBatch(
  round: number,
  batch: number,
  update: { value: string; percent: string },
): string {
  return splitSequenceBurst(round * 200 + batch, update.value, update.percent);
}

/** Completes the trailing `ESC [` the burst leaves dangling. Load-bearing
 *  before any other paint: a chunk that started with `32m` behind a resynced
 *  parser would print those parameters for reasons that have nothing to do with
 *  a rebuild, i.e. a false positive. */
const BURST_TERMINATOR = encodePtyFixtureCommand({ op: "EMIT", newline: false, text: "0m" });

/** Leaves the cursor on the memory value with a bare `ESC [` pending, so the
 *  next burst command completes an SGR run that spans the chunk edge. */
const ARM_SPLIT = encodePtyFixtureCommand({
  op: "EMIT",
  newline: false,
  text: `\x1b[${VALUE_ROW};${VALUE_COL}H\x1b[`,
});

/** Paint an htop-shaped alternate screen exactly once. All static markers fit
 *  inside the smallest viewport used below; later writes touch numeric cells
 *  only. */
function htopFrame(suffix: string): { payload: string; markers: string[] } {
  const markers = [
    `CPU-${suffix}`,
    `MEM-${suffix}`,
    `PROCS-${suffix}`,
    `+--BORDER-${suffix}--+`,
    `PID-${suffix}-USER-CPU%-MEM%-COMMAND`,
    `F1HELP-${suffix}`,
  ];
  const paints = [
    `\x1b[2J\x1b[${PERCENT_ROW};1H${markers[0]}\x1b[${PERCENT_ROW};18H[00.0]`,
    `\x1b[${VALUE_ROW};1H${markers[1]}\x1b[${VALUE_ROW};18H[0000M]`,
    `\x1b[${ACTIVITY_ROW};1H${markers[2]}\x1b[${ACTIVITY_ROW};${ACTIVITY_COL}H0`,
    `\x1b[7;1H${markers[3]}`,
    `\x1b[8;1H${markers[4]}`,
    `\x1b[${FOOTER_ROW};1H\x1b[7m ${markers[5]}  F2SETUP  F3SEARCH  F10QUIT \x1b[27m`,
  ];
  return {
    markers,
    payload: paints
      .map((text) => encodePtyFixtureCommand({ op: "EMIT", text, newline: false }))
      .join(""),
  };
}

function waitForPainted(page: Page, sessionId: string, marker: string) {
  return page.evaluate(({ id, text }) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return smokeWindow.__smoke.waitForPaintedMarker(id, text, 30_000);
  }, { id: sessionId, text: marker });
}

function gridEpoch(page: Page, sessionId: string): Promise<string> {
  return page.evaluate((id) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return smokeWindow.__smoke.cellGridEpoch(id);
  }, sessionId);
}

type StablePaintSnapshot = {
  epoch: string;
  frameCount: number;
  viewport: string;
  grid: string;
  markerCounts: number[];
  markerOffsets: number[];
};

/** A resize can legitimately install more than one stream generation while the
 * settled viewport intent catches up. Convergence is therefore a stable final
 * paint, not equality with the first changed epoch we happen to observe. */
async function waitForResizeRoundConvergence(
  page: Page,
  sessionId: string,
  expected: { value: string; percent: string },
  markers: readonly string[],
  minimumFrameCount: number,
): Promise<StablePaintSnapshot> {
  const stableWindowMs = 1_000;
  const requiredIdenticalSamples = 5;
  let candidateSignature = "";
  let candidateSince = 0;
  let candidateSamples = 0;
  let converged: StablePaintSnapshot | null = null;

  await expect.poll(async () => {
    const snapshot = await page.evaluate(({ id, expectedMarkers }) => {
      const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
      const smoke = smokeWindow.__smoke;
      const grid = document.querySelector(`[data-testid="terminal-slot-${id}"] .cell-grid`);
      const viewport = smoke.viewportText(id);
      return {
        epoch: smoke.cellGridEpoch(id),
        frameCount: smoke.cellFrameCount(id),
        viewport,
        grid: (grid?.textContent ?? "").replace(/\s+/g, " ").trim(),
        markerCounts: expectedMarkers.map((marker) => viewport.split(marker).length - 1),
        markerOffsets: expectedMarkers.map((marker) => viewport.indexOf(marker)),
      };
    }, { id: sessionId, expectedMarkers: [...markers] });
    const markersOrdered = snapshot.markerOffsets.every((offset, index, offsets) =>
      offset >= 0 && (index === 0 || offset > offsets[index - 1]!)
    );
    const complete = snapshot.epoch.length > 0
      && snapshot.frameCount > minimumFrameCount
      && snapshot.viewport.includes(expected.value)
      && snapshot.viewport.includes(expected.percent)
      && snapshot.markerCounts.every((count) => count === 1)
      && markersOrdered;
    const signature = complete ? JSON.stringify(snapshot) : "";
    const now = Date.now();
    if (!complete || signature !== candidateSignature) {
      candidateSignature = signature;
      candidateSince = now;
      candidateSamples = complete ? 1 : 0;
      converged = complete ? snapshot : null;
      return 0;
    }
    candidateSamples += 1;
    converged = snapshot;
    return candidateSamples >= requiredIdenticalSamples ? now - candidateSince : 0;
  }, { timeout: 30_000, intervals: [50, 100, 250] }).toBeGreaterThanOrEqual(stableWindowMs);

  if (converged === null) throw new Error("resize paint converged without a final snapshot");
  return converged;
}

test("alternate-screen chrome survives shrink and restore with differential redraws", async ({
  smokePage,
  stack,
}, testInfo) => {
  test.skip(!testInfo.project.name.startsWith("chromium"), "desktop terminal geometry + in-place resize contract");
  test.setTimeout(180_000);

  const fixtureWorker = await stack.startPtyFixtureWorker();
  const sessionId = await spawnPtyFixtureSession(smokePage, fixtureWorker);
  await navigateToSmokeSession(smokePage, sessionId);
  await waitForPainted(smokePage, sessionId, PTY_FIXTURE_READY);

  const suffix = crypto.randomUUID().replaceAll("-", "").slice(0, 8).toUpperCase();

  // Layout precondition: every static marker is CUP-addressed inside the
  // smallest grid. The alternate buffer cannot scroll an overflow into history.
  const sizeNonce = `SIZE-${suffix}`;
  await inputSmokeTerminal(
    smokePage,
    sessionId,
    encodePtyFixtureCommand({ op: "REPORT_SIZE", nonce: sizeNonce }),
  );
  const sizeHandle = await smokePage.waitForFunction(({ id, nonce }) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    const smoke = smokeWindow.__smoke;
    const match = smoke.viewportText(id).match(new RegExp(`SIZE:${nonce}:(\\d+)x(\\d+)`));
    return match ? { cols: Number(match[1]), rows: Number(match[2]) } : null;
  }, { id: sessionId, nonce: sizeNonce });
  const size = await sizeHandle.jsonValue();
  if (!size) throw new Error("the fixture never reported a terminal size");
  expect(size.rows).toBeGreaterThanOrEqual(FOOTER_ROW + 8);
  expect(size.cols).toBeGreaterThanOrEqual(ACTIVITY_COL + 16);

  const altNonce = `ALT-${suffix}`;
  await inputSmokeTerminal(smokePage, sessionId, encodePtyFixtureCommand({
    op: "ALT_SCREEN",
    active: true,
    prefix: `ALTPRIME-${suffix}-`,
    count: 1,
    nonce: altNonce,
  }));
  await waitForPainted(smokePage, sessionId, `ALT_READY:${altNonce}`);

  const htop = htopFrame(suffix);
  await inputSmokeTerminal(smokePage, sessionId, htop.payload);
  await waitForPainted(smokePage, sessionId, htop.markers.at(-1)!);
  await waitForStableCellFrames(smokePage, sessionId);

  const viewport = smokePage.viewportSize();
  if (!viewport) throw new Error("desktop project has no viewport to resize");
  const smaller = {
    width: Math.max(640, viewport.width - 240),
    height: Math.max(480, viewport.height - 120),
  };
  expect(smaller.width).toBeLessThan(viewport.width);
  expect(smaller.height).toBeLessThan(viewport.height);

  const updates = [
    { size: smaller, value: "1969M", percent: "42.1" },
    { size: viewport, value: "2077M", percent: "87.6" },
  ];
  const initialEpoch = await gridEpoch(smokePage, sessionId);
  let painted: StablePaintSnapshot | null = null;

  for (const [round, update] of updates.entries()) {
    // Park the live parser mid-SGR before the geometry change. All later paint
    // in this round touches only numeric gauge cells.
    await inputSmokeTerminal(smokePage, sessionId, ARM_SPLIT);
    const epochBefore = await gridEpoch(smokePage, sessionId);
    await smokePage.setViewportSize(update.size);

    // Keep the split stream flowing until the resize reframe is observed. A
    // timer could stop before the ordered boundary and leave the regression
    // untested.
    let reframed = false;
    for (let batch = 0; batch < 200 && !reframed; batch++) {
      await inputSmokeTerminal(
        smokePage,
        sessionId,
        numericBatch(round, batch, update),
      );
      reframed = await gridEpoch(smokePage, sessionId) !== epochBefore;
    }
    expect(reframed, `round ${round}: the viewport change never reframed the terminal stream`).toBe(true);
    const frameCountAtReframe = await smokePage.evaluate(
      (id) => (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.cellFrameCount(id),
      sessionId,
    );

    // Complete bytes that may still be queued behind the observed boundary,
    // then close the final dangling CSI. No static cell is repainted.
    await inputSmokeTerminal(
      smokePage,
      sessionId,
      splitSequenceBurst(200, update.value, update.percent)
        + splitSequenceBurst(201, update.value, update.percent)
        + BURST_TERMINATOR,
    );
    await waitForPainted(smokePage, sessionId, update.value);
    painted = await waitForResizeRoundConvergence(
      smokePage,
      sessionId,
      update,
      htop.markers,
      frameCountAtReframe,
    );
    expect(painted.epoch, `round ${round}: the stable resize generation did not advance`).not.toBe(epochBefore);
    expect(
      painted.frameCount,
      `round ${round}: no differential frame followed the observed resize reframe`,
    ).toBeGreaterThan(frameCountAtReframe);
  }

  if (painted === null) throw new Error("resize rounds completed without a stable paint");
  expect(painted.epoch).not.toBe(initialEpoch);
  expect(painted.markerCounts).toEqual(htop.markers.map(() => 1));
  expect(painted.markerOffsets).toEqual([...painted.markerOffsets].sort((left, right) => left - right));
  expect(painted.viewport).toContain("2077M");
  expect(painted.viewport).toContain("87.6");
  expect(painted.viewport).not.toContain("1969M");
  expect(painted.viewport).not.toContain("42.1");
  expect(painted.viewport).not.toContain("32m");
  // Marker/value tokens contain neither lowercase `m` nor CUP terminators, so
  // either match is an ANSI parameter body rendered as text.
  expect(painted.grid).not.toMatch(/\d+m/);
  expect(painted.grid).not.toMatch(/\d+;\d+[Hf]/);
});
