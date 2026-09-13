// Drives the explicit pre-resume scrollback demand used by the hidden-resume smoke flow.
// It returns the first painted history segment and stable grid locator for later identity checks.
// The caller owns the interruption and recovery assertions after this initial demand completes.

import type { Locator, Page } from "@playwright/test";
import type { PaintedScrollbackProbe } from "./terminal-smoke-api.ts";
import { expectPaintedScrollbackWellFormed } from "./terminal-probe-helpers.ts";
import { waitForStableCellFrames } from "./terminal-helpers.ts";

export interface InitialResumeScrollbackDemand {
  readonly initialGrid: Locator;
  readonly initialPainted: PaintedScrollbackProbe;
  readonly demandedRows: PaintedScrollbackProbe["rows"];
}

export async function demandInitialResumeScrollback(
  page: Page,
  sessionId: string,
): Promise<InitialResumeScrollbackDemand> {
  await waitForStableCellFrames(page, sessionId);
  const initialGrid = page.getByTestId(`terminal-slot-${sessionId}`).locator(".wterm.cell-grid");
  const initialBox = await initialGrid.boundingBox();
  if (!initialBox) throw new Error("resume terminal has no scroll container");
  const initialPainted = await page.evaluate((id) => window.__smoke.paintedScrollback(id), sessionId);
  await page.mouse.move(initialBox.x + initialBox.width / 2, initialBox.y + initialBox.height / 2);
  await page.mouse.wheel(0, -100_000);
  await page.waitForFunction(({ id, count }) =>
    window.__smoke.scrollbackBackfillRequestCount(id) > 0
      && window.__smoke.paintedScrollback(id).rows.length > count,
  { id: sessionId, count: initialPainted.rows.length }, { timeout: 10_000 });
  const demandedPainted = await page.evaluate((id) => window.__smoke.paintedScrollback(id), sessionId);
  expectPaintedScrollbackWellFormed(demandedPainted);
  const demandedRows = demandedPainted.rows.filter((row) =>
    !initialPainted.rows.some((prior) => prior.index === row.index),
  );
  return { initialGrid, initialPainted, demandedRows };
}

export async function returnResumeScrollbackToBottom(page: Page, sessionId: string): Promise<void> {
  await page.evaluate((id) => {
    const container = document.querySelector(`[data-testid="terminal-slot-${id}"] .wterm`);
    if (!(container instanceof HTMLElement)) throw new Error("resume terminal has no scroll container");
    container.scrollTop = container.scrollHeight;
    container.dispatchEvent(new Event("scroll"));
  }, sessionId);
}
