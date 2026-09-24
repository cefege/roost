// Real-flow proofs for predictive local echo, both driven by the PTY fixture's
// delayed input echo so a keyboard burst keeps several keystrokes in flight.
// The contradiction case pins that no painted glyph disagrees with the echoed
// row. The @serial burst case pins that a sustained burst neither wipes its own
// predictions (the pane's catch-up watchdog used to, once a second) nor leaves
// a keystroke unpainted past the first confirmed epoch.

import { test, expect } from "./fixtures.ts";
import type { Page } from "@playwright/test";
import type { TerminalTestStack } from "./stack.ts";
import {
  encodePtyFixtureCommand,
  PTY_FIXTURE_READY,
} from "./pty-fixture-protocol.ts";
import {
  inputSmokeTerminal,
  navigateToSmokeSession,
  spawnPtyFixtureSession,
} from "./terminal-helpers.ts";
import { waitForPainted } from "./terminal-multiview-helpers.ts";
import { readTerminalStreamProbe } from "./terminal-probe-helpers.ts";

// 40 printable single-width chars: long enough that the echo delay keeps a
// burst in flight, short enough never to wrap or scroll the row it lands on.
const PAYLOAD = "abcdefghijklmnopqrstuvwxyz0123456789ABCD";
const ECHO_DELAY_MS = 30;
// The burst case's echo delay is bounded on BOTH sides, and the window is
// ECHO_GRACE_MS wide. Slower than the typing interval, or the predicted caret
// rejoins the authoritative column between keystrokes and the sustained-lead
// condition under test never arms. Faster than interval + ECHO_GRACE_MS, or the
// frame carrying keystroke k's echo outlives the grace on keystroke k+1 and
// legitimately contradicts it — a real slow-echoing application, not the defect
// this case pins. 25 ms of slack each way at these values.
const BURST_ECHO_DELAY_MS = 65;
const BURST_KEY_INTERVAL_MS = 40;
// Keystrokes typed before the first echo confirms epoch 1 stay hidden by
// design (the tentative gate), so only later indices are required to paint.
const FIRST_REQUIRED_INDEX = 3;
// A burst typed faster than one echo round-trip can finish entirely inside the
// tentative gate, which paints nothing and leaves the contradiction oracle
// with no records to judge. One keystroke echoed to the grid first confirms
// epoch 1, so the burst that follows is painted rather than swallowed.
const GATE_PRIMER = "PRIMEGATE";

interface PredictionRecord {
  ch: string;
  col: number;
  row: number;
}

test("fast typing never paints a prediction the PTY contradicts", async ({
  smokePage,
  stack,
}, testInfo) => {
  test.skip(!testInfo.project.name.startsWith("chromium"), "trusted keyboard burst");
  test.setTimeout(120_000);

  const sessionId = await openEchoingFixturePane(smokePage, stack, ECHO_DELAY_MS);
  const dimensions = await smokePage.evaluate(
    (id) => window.__smoke.terminalDimensions(id),
    sessionId,
  );
  expect(dimensions.cols, "payload must fit one unwrapped row").toBeGreaterThanOrEqual(60);

  await smokePage.evaluate(() => window.__smoke.resetTerminalInputCapture());
  await smokePage.keyboard.type(GATE_PRIMER);
  await waitForEchoedText(smokePage, sessionId, GATE_PRIMER);
  await waitForPredictionInputIdle(smokePage, sessionId);

  await installPredictionRecorder(smokePage, sessionId);
  await smokePage.keyboard.type(PAYLOAD);
  await waitForEchoedText(smokePage, sessionId, PAYLOAD);

  const finalRows = await readViewportRows(smokePage, sessionId);
  const records = await readPredictionRecords(smokePage);
  const state = await readPredictState(smokePage);
  expect(records.length, "no prediction was painted — the case proves nothing").toBeGreaterThan(0);
  // Naming the rows and the reset tally in the message matters: a disagreement
  // is either a wrong guess or a stale ANCHOR, and only the column the echo
  // actually filled distinguishes them.
  expect(
    contradictedRecords(records, finalRows),
    `a painted prediction disagreed with the echoed row (resets ${state.resetCount}, cleared ${
      state.clearedCount}, last ${state.lastReset}, rows ${JSON.stringify(finalRows.slice(0, 5))})`,
  ).toEqual([]);
});

test("sustained fast typing never wipes its own predictions @serial", async ({
  smokePage,
  stack,
}, testInfo) => {
  test.skip(!testInfo.project.name.startsWith("chromium"), "trusted keyboard burst");
  test.setTimeout(120_000);

  const sessionId = await openEchoingFixturePane(smokePage, stack, BURST_ECHO_DELAY_MS);
  await installPredictionRecorder(smokePage, sessionId);
  const before = await readPredictState(smokePage);

  // 40 chars at 40 ms is 1.6 s of continuous typing, which crosses the pane's
  // FOREGROUND_DOM_STALL_MS boundary: a watermark that tracked the predicted
  // caret froze here and the watchdog wiped the overlay mid-burst.
  await smokePage.keyboard.type(PAYLOAD, { delay: BURST_KEY_INTERVAL_MS });
  await waitForEchoedText(smokePage, sessionId, PAYLOAD);

  const after = await readPredictState(smokePage);
  // The regression is an EXTERNAL wipe of correct predictions by the pane's
  // DOM-stall watchdog. Asserting on resetCount instead would also fail on a
  // contradiction reset, which is the engine's own documented rule and is
  // timing-bound (the grace leaves |echo − interval| of slack), so a loaded
  // box could fail this case for something that is not the defect.
  expect(
    after.clearedCount - before.clearedCount,
    `the stall watchdog wiped predictions mid-burst (resets ${
      after.resetCount - before.resetCount}, last reason ${after.lastReset})`,
  ).toBe(0);

  const finalRows = await readViewportRows(smokePage, sessionId);
  const records = await readPredictionRecords(smokePage);
  const payloadRow = finalRows.findIndex((text) => text.includes(PAYLOAD));
  expect(payloadRow, "the echoed payload never landed on one row").toBeGreaterThanOrEqual(0);
  const payloadStartCol = finalRows[payloadRow]!.indexOf(PAYLOAD);
  // Every char is unique and its column is fixed, so a missing (index, column)
  // pair means that keystroke was never painted locally at all.
  const painted = (index: number): boolean => records.some(
    (record) => record.ch === PAYLOAD[index] && record.col === payloadStartCol + index,
  );

  // The tentative gate legitimately hides the keystrokes typed before the
  // first echo confirms an epoch, and that prefix grows with link latency — so
  // the invariant is no GAP once the gate has opened, not a fixed start index.
  const firstPainted = [...PAYLOAD].findIndex((_, index) => painted(index));
  expect(firstPainted, "the overlay painted nothing at all").toBeGreaterThanOrEqual(0);
  expect(firstPainted, "predictions stayed hidden far past the first confirmed epoch")
    .toBeLessThanOrEqual(FIRST_REQUIRED_INDEX);
  const unpainted: number[] = [];
  for (let index = firstPainted; index < PAYLOAD.length; index++) {
    if (!painted(index)) unpainted.push(index);
  }
  expect(unpainted, "keystrokes the overlay dropped mid-burst").toEqual([]);
  expect(contradictedRecords(records, finalRows), "a painted prediction disagreed with the echoed row")
    .toEqual([]);

  // The predicted caret must never have blocked DOM reconciliation: that block
  // is what starved the watermark and armed the wipe.
  await expect.poll(async () => {
    const { browser } = await readTerminalStreamProbe(smokePage, sessionId);
    return browser.reconcile_block_reason === null
      && browser.dom_reconciled.grid_epoch === browser.handler_canonical.grid_epoch
      && browser.dom_reconciled.seq === browser.handler_canonical.seq;
  }, { timeout: 10_000, intervals: [50, 100, 250] }).toBe(true);
});

async function waitForPredictionInputIdle(page: Page, sessionId: string): Promise<void> {
  await expect.poll(async () => page.evaluate(async (id) => {
    const capture = window.__smoke.terminalInputCapture();
    const transport = await window.__smoke.probeTerminalTransport(id);
    return {
      accepted: capture.outcomes.accepted > 0,
      rejected: capture.outcomes.rejected,
      ambiguous: capture.outcomes.ambiguous,
      pending: transport.pending_input_count,
    };
  }, sessionId), { timeout: 10_000, intervals: [50, 100, 250] }).toEqual({
    accepted: true,
    rejected: 0,
    ambiguous: 0,
    pending: 0,
  });
}

/** A fixture pane whose PTY echoes typed bytes back after `echoDelayMs`, with
 *  the SRTT display gate removed so predictions paint on a loopback link too.
 *  No link options: stack-fixture-worker memoizes them at first start and
 *  sibling specs own the current values. */
async function openEchoingFixturePane(
  page: Page,
  stack: TerminalTestStack,
  echoDelayMs: number,
): Promise<string> {
  const fixtureWorker = await stack.startPtyFixtureWorker();
  const sessionId = await spawnPtyFixtureSession(page, fixtureWorker);
  // useSmokePage builds a fresh context per test, so this preference cannot
  // leak into a sibling spec.
  await page.context().addInitScript(() => {
    localStorage.setItem("roostPredict", "always");
  });
  await navigateToSmokeSession(page, sessionId);
  await waitForPainted(page, sessionId, PTY_FIXTURE_READY);
  await inputSmokeTerminal(
    page,
    sessionId,
    encodePtyFixtureCommand({ op: "ECHO_INPUT", delayMs: echoDelayMs }),
  );
  await waitForPainted(page, sessionId, "ECHO_INPUT_ARMED");
  return sessionId;
}

/** Record every prediction the overlay paints, as it is painted: the
 *  authoritative row text afterwards is the oracle each record is checked
 *  against. */
async function installPredictionRecorder(page: Page, sessionId: string): Promise<void> {
  await page.evaluate((id) => {
    const slot = document.querySelector(`[data-testid="terminal-slot-${id}"]`);
    const viewport = slot?.querySelector(".cell-viewport");
    if (!viewport) throw new Error(`no cell viewport for ${id}`);
    const records: PredictionRecord[] = [];
    (window as unknown as { __predictRecords: PredictionRecord[] }).__predictRecords = records;
    const observer = new MutationObserver((mutations) => {
      for (const mutation of mutations) {
        for (const node of mutation.addedNodes) {
          if (!(node instanceof HTMLElement)) continue;
          if (!node.classList.contains("cell-predict-ch")) continue;
          records.push({
            ch: node.textContent ?? "",
            col: Number.parseInt(node.style.left, 10),
            row: Number.parseInt(node.style.top, 10),
          });
        }
      }
    });
    observer.observe(viewport, { childList: true, subtree: true });
  }, sessionId);
}

function readPredictionRecords(page: Page): Promise<PredictionRecord[]> {
  return page.evaluate(
    () => (window as unknown as { __predictRecords: PredictionRecord[] }).__predictRecords,
  );
}

/** Viewport rows carry no row-index attribute; their DOM order IS their row
 *  order, and the overlay is a sibling of the rows, not a child. */
function readViewportRows(page: Page, sessionId: string): Promise<string[]> {
  return page.evaluate((id) => {
    const slot = document.querySelector(`[data-testid="terminal-slot-${id}"]`);
    const viewport = slot?.querySelector(".cell-viewport");
    if (!viewport) return [];
    return Array.from(viewport.querySelectorAll(".cell-row"))
      .map((row) => row.textContent ?? "");
  }, sessionId);
}

async function waitForEchoedText(page: Page, sessionId: string, text: string): Promise<void> {
  await expect
    .poll(async () => (await readViewportRows(page, sessionId)).some((row) => row.includes(text)), {
      timeout: 30_000,
      intervals: [50, 100, 250],
    })
    .toBe(true);
}

function contradictedRecords(
  records: readonly PredictionRecord[],
  finalRows: readonly string[],
): PredictionRecord[] {
  return records.filter((record) => (finalRows[record.row] ?? "")[record.col] !== record.ch);
}

type PredictDebugState = {
  resetCount: number;
  clearedCount: number;
  lastReset: string | null;
};

type PredictDebugWindow = { __roostPredictDebug?: () => PredictDebugState | null };

/** Throws rather than defaulting: a missing debug seam would make every reset
 *  delta vacuously zero and the whole case meaningless. */
function readPredictState(page: Page): Promise<PredictDebugState> {
  return page.evaluate(() => {
    const state = (window as unknown as PredictDebugWindow).__roostPredictDebug?.();
    if (!state) throw new Error("__roostPredictDebug is not installed");
    return {
      resetCount: state.resetCount,
      clearedCount: state.clearedCount,
      lastReset: state.lastReset,
    };
  });
}
