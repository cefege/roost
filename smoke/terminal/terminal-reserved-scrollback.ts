// Proves a pane's unfetched history stays reachable: one explicit demand over
// an interval the pane never painted paints the WORKER's own rows for exactly
// those indices. Real-flow reveal and resume specs use it to hold the line
// between painted history — only ever rows the worker sent — and the scroll
// space standing in for history nobody fetched. Reads the pane's DOM geometry
// and smoke painted-scrollback probes plus the stack's authorized client.

import { expect } from "./fixtures.ts";
import type { Page } from "@playwright/test";
import type { TerminalTestStack } from "./stack.ts";
import type { PaintedScrollbackProbe } from "./terminal-smoke-api.ts";

/** Half-open range of absolute worker row indices. */
export interface ScrollbackInterval {
  start: number;
  end: number;
}

/** Newest absolute row a pane's history layout reaches, painted or reserved. */
export function readScrollbackLayoutEnd(page: Page, sessionId: string): Promise<number> {
  return page.evaluate((id) => {
    const container = document.querySelector(`[data-testid="terminal-slot-${id}"] .wterm`);
    if (!(container instanceof HTMLElement)) {
      throw new Error(`session ${id} has no terminal scroll container`);
    }
    if (container.classList.contains("alt-active")) {
      throw new Error(`session ${id} is on the alt screen, which lays out no history`);
    }
    const last = container.querySelector(".cell-scrollback")?.lastElementChild as
      | HTMLElement
      | null;
    if (!last) return 0;
    if (last.classList.contains("cell-sb-gap")) {
      const end = Number(last.dataset.endRow);
      if (!Number.isSafeInteger(end)) {
        throw new Error(`session ${id} reserved a history gap with no absolute end`);
      }
      return end;
    }
    const newest = last.lastElementChild as HTMLElement | null;
    const index = Number(newest?.dataset.rowIndex);
    if (!Number.isSafeInteger(index)) {
      throw new Error(`session ${id} painted a history row with no absolute index`);
    }
    return index + 1;
  }, sessionId);
}

/** The interval a grid scrolled past while a pane was away: the layout has to
 *  reach the live base the worker moved to. Only the reach is an oracle — the
 *  renderer folds leading gaps into its head spacer and evicts painted rows
 *  past its hold cap, so reserved rows are not conserved as gap rows — and
 *  what the pane painted inside the interval is the caller's own oracle. */
export function expectTransitionedInterval(
  beforeEnd: number,
  afterEnd: number,
): ScrollbackInterval {
  expect(
    afterEnd,
    "history does not reach the live base the worker moved to",
  ).toBeGreaterThan(beforeEnd);
  return { start: beforeEnd, end: afterEnd };
}

/** One explicit demand over a reserved interval paints the worker's own rows
 *  for exactly those indices, and the worker still numbers its live base at the
 *  interval's end. */
export async function expectReservedIntervalPaintsWorkerRows(
  page: Page,
  client: TerminalTestStack["client"],
  sessionId: string,
  interval: ScrollbackInterval,
): Promise<void> {
  const gridEpoch = await page.evaluate((id) => window.__smoke.cellGridEpoch(id), sessionId);
  const retained = await client.sessionsGetScrollbackCells({
    sessionId,
    endRow: BigInt(interval.end),
    maxRows: interval.end - interval.start,
    gridEpoch,
  });
  expect({ start: Number(retained.startRow), end: Number(retained.endRow) }).toEqual(interval);
  expect(Number(retained.scrollbackTotal)).toBe(interval.end);
  expect(await demandScrollbackInterval(page, sessionId, interval)).toEqual(
    retained.rows.map((row) => ({
      index: row.index,
      text: row.spans.map((span) => span.text).join(""),
    })),
  );
}

/** Walk the reader over the interval by its own wheel gestures — the renderer
 *  owns the scroll position of a live pane, so a written scrollTop does not
 *  survive — until every row of it is painted. Each round re-parks on the
 *  OLDEST row still missing, so an interval wider than one history page is
 *  filled by successive demand waves, and a wave that fetches from a focus
 *  older than the interval still advances the walk. The rows may also already
 *  be painted by an authoritative delivery, which the renderer answers from
 *  what it holds instead of refetching. */
async function demandScrollbackInterval(
  page: Page,
  sessionId: string,
  interval: ScrollbackInterval,
): Promise<PaintedScrollbackProbe["rows"]> {
  const grid = page.getByTestId(`terminal-slot-${sessionId}`).locator(".wterm.cell-grid");
  const box = await grid.boundingBox();
  if (!box) throw new Error(`session ${sessionId} has no terminal scroll container`);
  await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
  let state = await readIntervalDemandState(page, sessionId, interval);
  let previous = { requests: -1, missingRow: -1 };
  const deadline = Date.now() + 20_000;
  while (!state.held && Date.now() < deadline) {
    const stalled = state.requests === previous.requests
      && state.missingRow === previous.missingRow;
    if (Math.abs(state.scrollDeltaPx) > 1) {
      await page.mouse.wheel(0, -state.scrollDeltaPx);
    } else if (stalled && state.rowHeightPx > 0) {
      // Parked on target with no wave in flight: a scroll demand only starts on
      // a scroll event, so move the reader to re-arm one.
      await page.mouse.wheel(0, -2 * state.rowHeightPx);
    }
    previous = { requests: state.requests, missingRow: state.missingRow };
    await page.waitForTimeout(300);
    state = await readIntervalDemandState(page, sessionId, interval);
  }
  expect(
    state,
    `demanding rows [${interval.start}, ${interval.end}) never painted them`,
  ).toMatchObject({ held: true });
  const painted = await page.evaluate(
    ({ id, start, end }) => window.__smoke.paintedScrollbackRange(id, start, end),
    { id: sessionId, start: interval.start, end: interval.end },
  );
  if (!painted) throw new Error(`rows [${interval.start}, ${interval.end}) are not painted`);
  return painted;
}

/** Demand progress, the scroll the next wave needs, and the state a stalled
 *  demand has to be explained by. */
function readIntervalDemandState(page: Page, sessionId: string, interval: ScrollbackInterval) {
  return page.evaluate(({ id, start, end }) => {
    const container = document.querySelector(`[data-testid="terminal-slot-${id}"] .wterm`);
    if (!(container instanceof HTMLElement)) {
      throw new Error(`session ${id} has no terminal scroll container`);
    }
    const spacer = container.querySelector<HTMLElement>(".cell-sb-spacer");
    const viewportRow = container.querySelector<HTMLElement>(".cell-viewport .cell-row");
    const rowHeightPx = viewportRow?.getBoundingClientRect().height ?? 0;
    let missingRow = -1;
    for (let row = start; row < end; row++) {
      if (!window.__smoke.hasPaintedScrollbackRange(id, row, row + 1)) {
        missingRow = row;
        break;
      }
    }
    // The renderer lays every absolute row — painted or reserved — at
    // spacerTop + row * rowHeight, so that law places the target row whether a
    // gap or the collapsed head spacer holds it. Parking it two rows inside the
    // bottom of the window keeps the reader clear of the bottom-follow band,
    // which suppresses demands, and makes a demand focus no row newer than it.
    let scrollDeltaPx = 0;
    if (missingRow >= 0 && spacer && rowHeightPx > 0) {
      const target = Math.max(
        0,
        spacer.offsetTop + (missingRow + 2) * rowHeightPx - container.clientHeight,
      );
      scrollDeltaPx = Math.round(container.scrollTop - target);
    }
    const painted = window.__smoke.paintedScrollback(id);
    const presentation = window.__smoke.terminalBrowserSnapshot(id).presentation;
    return {
      held: window.__smoke.hasPaintedScrollbackRange(id, start, end),
      missingRow,
      requests: window.__smoke.scrollbackBackfillRequestCount(id),
      fullFrameSbRows: window.__smoke.lastFullFrameSbRows(id),
      frames: window.__smoke.cellFrameCount(id),
      scrollDeltaPx,
      rowHeightPx,
      scrollTop: Math.round(container.scrollTop),
      scrollHeight: Math.round(container.scrollHeight),
      clientHeight: container.clientHeight,
      reserved: [...container.querySelectorAll<HTMLElement>(".cell-sb-gap")]
        .map((gap) => `${gap.dataset.startRow}..${gap.dataset.endRow}`),
      paintedFrom: painted.rows[0]?.index ?? -1,
      paintedTo: painted.rows.at(-1)?.index ?? -1,
      readerIntent: presentation?.reader_intent ?? null,
      followsBottom: presentation?.follows_bottom ?? null,
    };
  }, { id: sessionId, start: interval.start, end: interval.end });
}
