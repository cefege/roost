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

// A full-screen TUI's repaint replayed across a terminal-core rebuild must not
// leak escape-sequence PARAMETER bytes as literal text, and must not leave the
// grid holding regions the TUI believes it already painted.
//
// THE DEFECT (production: an htop pane that came back showing the literal text
// `32m1969M` at row 0 col 0, plus regions that never repainted again):
// `rebuildTerminalCore` splits the retained raw PTY ring at
// `capture.boundarySeq` and replays it into a FRESH @wterm/core. `boundarySeq`
// is `head_seq` at the keeper's resize dispatch, and head_seq advances by WHOLE
// PTY CHUNKS — so the boundary can fall ANYWHERE, including between `ESC [` and
// `32m`. On the alternate buffer the pre-boundary head is deliberately NOT
// replayed (it holds absolute cursor moves painted for the old width), so the
// replacement core's VT parser is COLD; a tail spliced mid-escape-sequence then
// reads that sequence's remaining bytes as PRINTABLE TEXT. Because the tail is
// the TUI's post-SIGWINCH repaint, a desynced repaint also leaves cells the TUI
// will never rewrite — the stale regions.
//
// THE REPRO drives the boundary into a sequence on purpose. Every chunk the PTY
// fixture writes ENDS with a bare `ESC [`, and its parameters (`32m` plus the
// distinctive `1969M` gauge value) OPEN the next chunk, so wherever the worker
// cuts the stream the cut is mid-sequence. The stream keeps flowing until the
// rebuild has been observed, so the cut is guaranteed to land inside it rather
// than before or after — and the remainder then reaches the replacement core
// either inside the replayed tail or live behind it, which are the same cold
// parser either way.

const VALUE_TOKEN = "1969M"; // uppercase M on purpose: see the leak regexes below
const GAUGE_ROW = 3; // `MEM[` owns cols 1-4, the value cols 7-11, the `]` col 12
const GAUGE_COL = 7;
const SPIN_COL = 14; // one cell that changes on every chunk, so cell frames keep advancing
const MARKER_TOP_ROW = 5;
const MARKER_ROWS = 6;
const FOOTER_ROW = 12;
// Under the fixture's MAX_PENDING_WORK (32), so its stdin never pauses and the
// worker's PTY write never blocks behind the pacing below.
const BURST_CHUNKS = 24;
const BURST_CHUNK_DELAY_MS = 2;

/** One paced batch of mid-sequence-split chunks. Each chunk ends with a bare
 *  `ESC [` whose parameters begin the following chunk, and each paints a
 *  different spinner glyph: a stream that changed no cell would emit no cell
 *  frames, and the rebuild could not then be observed while it is still live. */
function splitSequenceBurst(batch: number): string {
  let frames = "";
  for (let index = 0; index < BURST_CHUNKS; index++) {
    const spinner = (batch + index) % 2 === 0 ? "-" : "=";
    frames += encodePtyFixtureCommand({
      op: "EMIT",
      newline: false,
      delayMs: BURST_CHUNK_DELAY_MS,
      text: `32m${VALUE_TOKEN}\x1b[0m\x1b[${GAUGE_ROW};${SPIN_COL}H${spinner}`
        + `\x1b[${GAUGE_ROW};${GAUGE_COL}H\x1b[`,
    });
  }
  return frames;
}

/** Completes the trailing `ESC [` the burst leaves dangling. Load-bearing
 *  before any other paint: a chunk that started with `32m` behind a resynced
 *  parser would print those parameters for reasons that have nothing to do with
 *  a rebuild, i.e. a false positive. */
const BURST_TERMINATOR = encodePtyFixtureCommand({ op: "EMIT", newline: false, text: "0m" });

/** Leaves the cursor on the gauge value with a bare `ESC [` pending, so the
 *  next burst chunk's `32m` completes an SGR run that spans the chunk edge. */
const ARM_SPLIT = encodePtyFixtureCommand({
  op: "EMIT",
  newline: false,
  text: `\x1b[${GAUGE_ROW};${GAUGE_COL}H\x1b[`,
});

/** The htop-shaped chrome: a CUP-addressed gauge row, numbered body rows, and a
 *  reverse-video function-key footer. Every paint is absolute, so nothing ever
 *  scrolls — the alternate buffer has no scrollback to absorb it — and rows 1-2
 *  are deliberately left untouched, because that is where a cold parser prints
 *  the parameters it failed to consume. */
function framePaint(markerPrefix: string): string {
  const paints = [
    `\x1b[0m\x1b[${GAUGE_ROW};1HMEM[`,
    `\x1b[${GAUGE_ROW};${GAUGE_COL + VALUE_TOKEN.length}H]`,
  ];
  for (let row = 1; row <= MARKER_ROWS; row++) {
    paints.push(`\x1b[${MARKER_TOP_ROW + row - 1};1H${markerPrefix}${row}`);
  }
  paints.push(`\x1b[${FOOTER_ROW};1H\x1b[7m F1HELP  F2SETUP  F3SEARCH  F10QUIT \x1b[27m`);
  return paints
    .map((text) => encodePtyFixtureCommand({ op: "EMIT", text, newline: false }))
    .join("");
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

test("an alt-screen repaint replayed across a core rebuild leaks no escape parameters", async ({
  smokePage,
  stack,
}, testInfo) => {
  test.skip(!testInfo.project.name.startsWith("chromium"), "desktop terminal geometry + rebuild contract");
  test.setTimeout(180_000);

  const fixtureWorker = await stack.startPtyFixtureWorker();
  const sessionId = await spawnPtyFixtureSession(smokePage, fixtureWorker);
  await navigateToSmokeSession(smokePage, sessionId);
  await waitForPainted(smokePage, sessionId, PTY_FIXTURE_READY);

  const suffix = crypto.randomUUID().replaceAll("-", "").slice(0, 8).toUpperCase();
  const markerPrefix = `ALTROW-${suffix}-`;

  // Layout precondition, not a measurement: every paint below is CUP-addressed
  // into a fixed row budget and the alternate buffer cannot scroll an overflow
  // into history, so a grid too small to hold the frame would assert nothing.
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
  expect(size.rows).toBeGreaterThanOrEqual(FOOTER_ROW + 2);
  expect(size.cols).toBeGreaterThanOrEqual(SPIN_COL + 4);

  // The alternate buffer is the branch under test: it is the one where the ring
  // head is never replayed, so the replacement core's parser starts cold.
  const altNonce = `ALT-${suffix}`;
  await inputSmokeTerminal(smokePage, sessionId, encodePtyFixtureCommand({
    op: "ALT_SCREEN",
    active: true,
    prefix: `ALTPRIME-${suffix}-`,
    count: 1,
    nonce: altNonce,
  }));
  await waitForPainted(smokePage, sessionId, `ALT_READY:${altNonce}`);

  const viewport = smokePage.viewportSize();
  if (!viewport) throw new Error("desktop project has no viewport to resize");
  // Width-only steps: rows (and therefore the layout constants above) survive,
  // while cols move far enough that the SPA claims a new geometry — the
  // transaction that installs the resize capture and rebuilds the core.
  const widths = [viewport.width - 160, viewport.width - 320];

  for (const [round, width] of widths.entries()) {
    // Pre-boundary frame, left mid-SGR: `ESC [` is the last byte the retired
    // core ever sees, exactly as the production chunk boundary fell.
    await inputSmokeTerminal(smokePage, sessionId, framePaint(markerPrefix) + ARM_SPLIT);
    await waitForPainted(smokePage, sessionId, `${markerPrefix}${MARKER_ROWS}`);
    const epochBefore = await gridEpoch(smokePage, sessionId);

    await smokePage.setViewportSize({ width, height: viewport.height });
    // Keep the split stream flowing until the rebuild is OBSERVED. Stopping on
    // a timer instead would let the boundary land after the last chunk, where
    // nothing is spliced and the case would pass without exercising anything.
    let rebuilt = false;
    for (let batch = 0; batch < 200 && !rebuilt; batch++) {
      await inputSmokeTerminal(smokePage, sessionId, splitSequenceBurst(batch));
      rebuilt = await gridEpoch(smokePage, sessionId) !== epochBefore;
    }
    expect(rebuilt, `round ${round}: the viewport change never rebuilt the terminal core`).toBe(true);
    const epochAfter = await gridEpoch(smokePage, sessionId);

    // Two more batches so the sequence the worker cut is completed by bytes that
    // reach the REPLACEMENT core, then one terminator to close the last dangling
    // `ESC [`, then the TUI's own full repaint of the chrome — all in one FIFO
    // payload, because a burst chunk landing after the chrome would leak its
    // parameters for a reason this spec is not about.
    await inputSmokeTerminal(
      smokePage,
      sessionId,
      splitSequenceBurst(0) + splitSequenceBurst(1) + BURST_TERMINATOR + framePaint(markerPrefix),
    );
    await waitForPainted(smokePage, sessionId, `${markerPrefix}${MARKER_ROWS}`);
    await waitForStableCellFrames(smokePage, sessionId);

    const painted = await smokePage.evaluate(({ id, prefix }) => {
      const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
      const smoke = smokeWindow.__smoke;
      const grid = document.querySelector(`[data-testid="terminal-slot-${id}"] .cell-grid`);
      return {
        viewport: smoke.viewportText(id),
        grid: (grid?.textContent ?? "").replace(/\s+/g, " ").trim(),
        scan: smoke.markerScan(id, prefix),
        epoch: smoke.cellGridEpoch(id),
      };
    }, { id: sessionId, prefix: markerPrefix });

    // The evidence belongs to the rebuild this round provoked: a further
    // rebuild would have wiped rows 1-2 (its own head is unreplayed too) and
    // silently answered a different question.
    expect(painted.epoch, `round ${round}: a second rebuild replaced the grid under the assertions`)
      .toBe(epochAfter);
    // The exact production artifact: `ESC [` was consumed by the retired core
    // and its parameters printed as text by the cold one.
    expect(painted.viewport).not.toContain(`32m${VALUE_TOKEN}`);
    expect(painted.viewport).not.toContain("32m");
    // ...and the coloured value is still THERE — not merely deleted.
    expect(painted.viewport).toContain(VALUE_TOKEN);
    // No parameter body of any sequence rendered as text. Nothing this spec
    // paints holds a lowercase `m` or a `;` (the gauge value ends in an
    // uppercase M and the marker suffix is uppercase hex), so either match is a
    // leaked SGR or CUP body.
    expect(painted.grid).not.toMatch(/\d+m/);
    expect(painted.grid).not.toMatch(/\d+;\d+[Hf]/);
    // The repaint owns the whole grid: no row lost to a desynced parser, none
    // duplicated by a double replay, none left out of position.
    expect(painted.scan).toMatchObject({ duplicated: [], missing: 0, outOfOrder: 0 });
    expect(painted.scan.total).toBe(MARKER_ROWS);
  }
});
