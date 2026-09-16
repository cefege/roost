import { test, expect } from "./fixtures.ts";
import type { Page } from "@playwright/test";
import { encodePtyFixtureCommand, PTY_FIXTURE_READY } from "./pty-fixture-protocol.ts";
import type { RetainedMarkerScan } from "../../apps/web/src/lib/smokeTypes.ts";
import {
  spawnPtyFixtureSession,
  navigateToSmokeSession,
  switchToSmokeSession,
  inputSmokeTerminal,
  waitForStableCellFrames,
} from "./terminal-helpers.ts";

// An inline agent TUI lives on the MAIN screen: it repaints a multi-row card in
// place with cursor-up + rewrite, so a pane that is not applying frames holds a
// generation the worker has already overwritten. This stream's card rows never
// scroll off — only transcript lines above the card enter history — so a card
// row in painted scrollback is a row the worker never sent. Losing foreground
// mid-repaint makes every later delivery a viewport-only canonical checkpoint.

const CARD_ROWS = 8;
/** The card plus the transcript line above it must fit with room to spare: the
 *  repaint's cursor arithmetic is relative and must never reach row 1. */
const MIN_GRID_ROWS = CARD_ROWS + 8;
/** Wide enough that no card row wraps, which would break the row accounting. */
const MIN_GRID_COLS = 48;
const FOREGROUND_GENERATIONS = 3;
const BACKGROUND_ROUNDS = 3;

type CardState = { generation: number; transcript: number };

type CardTokens = {
  header: string;
  body: string;
  genPrefix: string;
  transcriptPrefix: string;
  fillPrefix: string;
};

function cardBlock(tokens: CardTokens, generation: number): string[] {
  const rows = [`+=== ${tokens.header} ===+`];
  for (let row = 1; row <= CARD_ROWS - 2; row++) rows.push(`| ${tokens.body} step-${row} |`);
  rows.push(`+-- ${genMarker(tokens, generation)} --+`);
  return rows;
}

/** One card paint. The cursor starts and ends on the blank bottom row, so a
 *  repaint rewinds exactly CARD_ROWS+1 rows and lands on the rows the card
 *  already owns; its leading newline is the ONLY row this stream ever scrolls
 *  off, and that row is always a transcript line from the top of the grid. The
 *  first paint has no card to rewind onto, so its rows scroll into place. */
function paintCard(tokens: CardTokens, state: CardState, repaint: boolean): string {
  state.transcript += 1;
  state.generation += 1;
  const block = `\r\x1b[2K${tokens.transcriptPrefix}${state.transcript}\r\n`
    + cardBlock(tokens, state.generation).map((row) => `\r\x1b[2K${row}\r\n`).join("");
  return repaint ? `\r\n\x1b[${CARD_ROWS + 1}A${block}` : block;
}

function emit(text: string): string {
  return encodePtyFixtureCommand({ op: "EMIT", text, newline: false });
}

/** Written by the emitter and matched by the assertions, so the padding both
 *  sides agree on has exactly one owner. */
function genMarker(tokens: CardTokens, generation: number): string {
  return `${tokens.genPrefix}${String(generation).padStart(4, "0")}`;
}

function waitForViewportMarker(page: Page, sessionId: string, marker: string): Promise<void> {
  return expect.poll(
    () => page.evaluate((id) => window.__smoke.viewportText(id), sessionId),
    { timeout: 30_000, intervals: [50, 100, 250] },
  ).toContain(marker);
}

/** The worker's own retained history, retried past the scan's own mid-flight
 *  guard: a dark pane's grid keeps scrolling while the scan pages it. */
function retainedScan(
  page: Page,
  sessionId: string,
  prefix: string,
): Promise<RetainedMarkerScan> {
  return page.evaluate(async ({ id, markerPrefix }) => {
    let lastError: unknown = null;
    for (let attempt = 0; attempt < 16; attempt++) {
      try {
        return await window.__smoke.retainedMarkerScan(id, markerPrefix);
      } catch (error) {
        lastError = error;
        const settled = Promise.withResolvers<void>();
        setTimeout(settled.resolve, 200);
        await settled.promise;
      }
    }
    throw lastError;
  }, { id: sessionId, markerPrefix: prefix });
}

/** Everything the incident is visible in, read in one pass: the painted DOM rows
 *  carrying a card row, the renderer's own painted history, and the marker
 *  oracles over both. */
function surveyPane(page: Page, sessionId: string, tokens: CardTokens) {
  return page.evaluate(({ id, card }) => {
    const smoke = window.__smoke;
    const grid = document.querySelector(`[data-testid="terminal-slot-${id}"] .cell-grid`);
    const isCardRow = (text: string): boolean => text.includes(card.header)
      || text.includes(card.body)
      || text.includes(card.genPrefix);
    const rows = Array.from(grid?.querySelectorAll(".cell-row") ?? [])
      .map((row) => row.textContent ?? "");
    const historyCardRows = smoke.paintedScrollback(id).rows
      .filter((row) => isCardRow(row.text))
      .map((row) => `${row.index}: ${row.text}`);
    return {
      cardRowIndices: rows.flatMap((text, index) => isCardRow(text) ? [index] : []),
      headerRowIndices: rows.flatMap((text, index) => text.includes(card.header) ? [index] : []),
      historyCardRows: historyCardRows.slice(0, 64),
      historyCardRowCount: historyCardRows.length,
      gen: smoke.markerScan(id, card.genPrefix),
      transcript: smoke.markerScan(id, card.transcriptPrefix),
      fullFrameSbRows: smoke.lastFullFrameSbRows(id),
    };
  }, { id: sessionId, card: tokens });
}

test("a backgrounded inline TUI repaint never freezes a stale generation into history", async ({
  smokePage,
  stack,
}, testInfo) => {
  test.skip(
    !testInfo.project.name.startsWith("chromium"),
    "desktop terminal geometry + main-screen repaint contract",
  );
  test.setTimeout(240_000);

  const fixtureWorker = await stack.startPtyFixtureWorker();
  const sessionId = await spawnPtyFixtureSession(smokePage, fixtureWorker);
  const parkedSessionId = await spawnPtyFixtureSession(smokePage, fixtureWorker);
  await navigateToSmokeSession(smokePage, sessionId);
  await waitForViewportMarker(smokePage, sessionId, PTY_FIXTURE_READY);

  const runId = crypto.randomUUID().replaceAll("-", "").slice(0, 8).toUpperCase();
  const tokens: CardTokens = {
    header: `MRPHDR-${runId}`,
    body: `MRPBODY-${runId}`,
    genPrefix: `MRPGEN-${runId}-`,
    transcriptPrefix: `MRPTL-${runId}-`,
    fillPrefix: `MRPFILL-${runId}-`,
  };
  const state: CardState = { generation: 0, transcript: 0 };

  const sizeNonce = `MRPSIZE-${runId}`;
  await inputSmokeTerminal(smokePage, sessionId, encodePtyFixtureCommand({
    op: "REPORT_SIZE",
    nonce: sizeNonce,
  }));
  const sizeHandle = await smokePage.waitForFunction(({ id, nonce }) => {
    const match = window.__smoke.viewportText(id).match(new RegExp(`SIZE:${nonce}:(\\d+)x(\\d+)`));
    return match ? { cols: Number(match[1]), rows: Number(match[2]) } : null;
  }, { id: sessionId, nonce: sizeNonce }, { timeout: 30_000 });
  const size = await sizeHandle.jsonValue();
  if (!size) throw new Error("the fixture never reported a terminal size");
  expect(size.rows).toBeGreaterThanOrEqual(MIN_GRID_ROWS);
  expect(size.cols).toBeGreaterThanOrEqual(MIN_GRID_COLS);

  // Fill the grid so the cursor parks on the bottom row with real history under
  // it: the card's first paint must scroll into place, not land mid-screen.
  const fillCount = size.rows + 4;
  await inputSmokeTerminal(smokePage, sessionId, encodePtyFixtureCommand({
    op: "FLOOD",
    prefix: tokens.fillPrefix,
    count: fillCount,
  }));
  await waitForViewportMarker(smokePage, sessionId, `${tokens.fillPrefix}${fillCount}`);

  await inputSmokeTerminal(smokePage, sessionId, emit(paintCard(tokens, state, false)));
  await waitForViewportMarker(smokePage, sessionId, genMarker(tokens, state.generation));

  // The dark window must scroll past a full grid height, so the whole held
  // viewport — card included — falls inside the interval a checkpoint spans.
  const backgroundSteps = size.rows + 4;

  for (let round = 0; round < BACKGROUND_ROUNDS; round++) {
    for (let generation = 0; generation < FOREGROUND_GENERATIONS; generation++) {
      await inputSmokeTerminal(smokePage, sessionId, emit(paintCard(tokens, state, true)));
      await waitForViewportMarker(smokePage, sessionId, genMarker(tokens, state.generation));
    }
    await waitForStableCellFrames(smokePage, sessionId);
    const heldGeneration = state.generation;
    const before = await retainedScan(smokePage, sessionId, tokens.transcriptPrefix);

    await switchToSmokeSession(smokePage, parkedSessionId);
    let burst = "";
    for (let step = 0; step < backgroundSteps; step++) burst += paintCard(tokens, state, true);
    await inputSmokeTerminal(smokePage, sessionId, emit(burst));

    await expect.poll(
      async () => (await retainedScan(smokePage, sessionId, tokens.transcriptPrefix)).scrollbackTotal,
      { timeout: 60_000, intervals: [200, 400] },
    ).toBeGreaterThanOrEqual(before.scrollbackTotal + backgroundSteps);
    const after = await retainedScan(smokePage, sessionId, tokens.transcriptPrefix);
    expect(
      after.scrollbackTotal - before.scrollbackTotal,
      `round ${round}: an in-place repaint must scroll exactly one row per generation`,
    ).toBe(backgroundSteps);
    // Proof the pane really lost foreground: it is still painting the generation
    // it held before the burst, so the reveal has to be a canonical checkpoint.
    const dark = await surveyPane(smokePage, sessionId, tokens);
    expect(
      dark.gen.max,
      `round ${round}: the backgrounded pane kept applying frames, so no checkpoint is under test`,
    ).toBe(heldGeneration);

    await switchToSmokeSession(smokePage, sessionId);
    await waitForViewportMarker(smokePage, sessionId, genMarker(tokens, state.generation));
    await waitForStableCellFrames(smokePage, sessionId);
  }

  const survey = await surveyPane(smokePage, sessionId, tokens);
  // A viewport-only checkpoint is the delivery under test; one carrying its own
  // history rows would prove something else.
  expect(survey.fullFrameSbRows, "the checkpoint was not viewport-only").toBe(0);

  const retainedGen = await retainedScan(smokePage, sessionId, tokens.genPrefix);
  expect(
    retainedGen.markerIds,
    "the worker's own history carried a card row, so this stream cannot prove fabrication",
  ).toEqual([]);

  // Soft, so one corrupted pane reports every invariant it broke: which rows
  // were fabricated, how often the block repeats, and which generations stuck.
  expect.soft(
    survey.historyCardRows,
    "painted scrollback holds card rows the worker never scrolled off",
  ).toEqual([]);
  expect.soft(survey.historyCardRowCount).toBe(0);
  // The card exists once, on the rows it owns: one unbroken run at the tail.
  expect.soft(
    survey.cardRowIndices.length,
    "the repainted card block is painted more than once",
  ).toBe(CARD_ROWS);
  expect.soft(survey.cardRowIndices).toEqual(
    Array.from({ length: CARD_ROWS }, (_, offset) => survey.cardRowIndices[0]! + offset),
  );
  expect.soft(survey.headerRowIndices, "the card header repeats across rows").toHaveLength(1);
  expect.soft(survey.gen, "a generation marker is painted more than once or out of order")
    .toMatchObject({
      total: 1,
      unique: 1,
      duplicated: [],
      outOfOrder: 0,
      min: state.generation,
      max: state.generation,
    });
  expect.soft(survey.transcript).toMatchObject({ duplicated: [], outOfOrder: 0 });
});
