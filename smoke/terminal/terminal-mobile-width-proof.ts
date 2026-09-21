// Mobile terminal width proof shared by font-settlement and text-inflation smoke cases.
// It measures the live cell grid with the terminal's own ten-cell shape.
// DOM-Range edge checks require the final marker within pane and visual viewport clips.
// Callers provide real shell pages; this module owns no independent geometry model.

import type { Page } from "@playwright/test";
import { expect } from "./fixtures.ts";
import { inputSmokeTerminal } from "./terminal-helpers.ts";

const FIT_TOLERANCE_PX = 1;
export const SETTLE_TIMEOUT_MS = 30_000;
export const SETTLE_INTERVALS = [100, 250];
const MARKER_HEAD = "EDGE_";

/** Ten-cell probe inside the mounted grid, the shape measureTerminalCellBox
 *  uses, so a swapped face is read the way the geometry owner reads it. */
export async function measureCellAdvance(page: Page, sessionId: string): Promise<number> {
  return page.evaluate((id) => {
    const grid = document.querySelector(`[data-testid="terminal-slot-${id}"] .cell-grid`);
    if (!(grid instanceof HTMLElement)) throw new Error(`terminal grid unavailable for ${id}`);
    const probe = document.createElement("span");
    probe.className = "cell-row";
    probe.style.position = "absolute";
    probe.style.visibility = "hidden";
    probe.style.whiteSpace = "pre";
    probe.textContent = "0".repeat(10);
    grid.append(probe);
    const width = probe.getBoundingClientRect().width;
    probe.remove();
    return width / 10;
  }, sessionId);
}

export async function readDimensions(
  page: Page,
  sessionId: string,
): Promise<{ cols: number; rows: number }> {
  return page.evaluate((id) => window.__smoke.terminalDimensions(id), sessionId);
}

/** 1-based viewport rows painted fully inside the terminal clip and the visual
 *  viewport, so a marker placed there is expected to be seen. */
async function readFullyVisibleRows(page: Page, sessionId: string): Promise<number[]> {
  return page.evaluate((id) => {
    const grid = document.querySelector(`[data-testid="terminal-slot-${id}"] .cell-grid`);
    const sheet = grid?.querySelector(".cell-viewport");
    if (!(grid instanceof HTMLElement) || !(sheet instanceof HTMLElement)) {
      throw new Error(`terminal viewport unavailable for ${id}`);
    }
    const gridRect = grid.getBoundingClientRect();
    const visual = window.visualViewport;
    const visualTop = visual?.offsetTop ?? 0;
    const clipTop = Math.max(gridRect.top, visualTop);
    const clipBottom = Math.min(gridRect.bottom, visualTop + (visual?.height ?? innerHeight));
    return Array.from(sheet.querySelectorAll(":scope > .cell-row")).flatMap((row, index) => {
      const rect = row.getBoundingClientRect();
      return rect.width > 0
        && rect.height > 0
        && rect.top >= clipTop + 1
        && rect.bottom <= clipBottom - 1
        ? [index + 1]
        : [];
    });
  }, sessionId);
}

/** The canonical sheet against the pane's own content box, measured the way
 *  lib/terminalCellGeometry.ts measures it: clientWidth minus its padding. */
export async function expectSheetFitsPane(page: Page, sessionId: string, label: string): Promise<void> {
  await expect.poll(() => page.evaluate((id) => {
    const grid = document.querySelector(`[data-testid="terminal-slot-${id}"] .cell-grid`);
    const sheet = grid?.querySelector(".cell-viewport");
    // A pane mid-remount has no sheet to fit; keep polling rather than
    // reporting a layout gap as an overclaim.
    if (!(grid instanceof HTMLElement) || !(sheet instanceof HTMLElement)) return Infinity;
    const styles = getComputedStyle(grid);
    const usableWidth = grid.clientWidth
      - (Number.parseFloat(styles.paddingLeft) || 0)
      - (Number.parseFloat(styles.paddingRight) || 0);
    return sheet.getBoundingClientRect().width - usableWidth;
  }, sessionId), {
    timeout: SETTLE_TIMEOUT_MS,
    intervals: SETTLE_INTERVALS,
    message: `${label}: CSS px the canonical sheet overflows the pane's content box`,
  }).toBeLessThanOrEqual(FIT_TOLERANCE_PX);
}

/** Every viewer reports the same grid, unchanged across consecutive reads. */
export async function waitForConvergedColumns(
  viewers: readonly Page[],
  sessionId: string,
  label: string,
): Promise<number> {
  let previous = "";
  let stableReads = 0;
  await expect.poll(async () => {
    const dimensions = await Promise.all(
      viewers.map((viewer) => readDimensions(viewer, sessionId)),
    );
    const signature = JSON.stringify(dimensions);
    const converged = dimensions.every((value) => value.cols > 0
      && value.cols === dimensions[0].cols
      && value.rows === dimensions[0].rows);
    stableReads = converged && signature === previous ? stableReads + 1 : 0;
    previous = signature;
    return stableReads;
  }, {
    timeout: SETTLE_TIMEOUT_MS,
    intervals: SETTLE_INTERVALS,
    message: `${label}: every viewer settled on one grid`,
  }).toBeGreaterThanOrEqual(2);
  return (await readDimensions(viewers[0], sessionId)).cols;
}

/** Right edge of the marker's last glyph, read from a DOM Range over exactly
 *  that text and compared with the terminal clip intersected with the visual
 *  viewport. A clipped column still owns its row text, so presence in the grid
 *  is never visibility. */
export async function expectLastColumnVisible(
  page: Page,
  sessionId: string,
  marker: string,
  label: string,
): Promise<void> {
  await expect.poll(
    () => page.evaluate(
      ({ id, text }) => window.__smoke.viewportText(id).includes(text),
      { id: sessionId, text: marker },
    ),
    {
      timeout: SETTLE_TIMEOUT_MS,
      intervals: SETTLE_INTERVALS,
      message: `${label}: the grid painted ${marker}`,
    },
  ).toBe(true);
  const edge = await page.evaluate(({ id, text }) => {
    const grid = document.querySelector(`[data-testid="terminal-slot-${id}"] .cell-grid`);
    const sheet = grid?.querySelector(".cell-viewport");
    if (!(grid instanceof HTMLElement) || !(sheet instanceof HTMLElement)) return null;
    for (const row of Array.from(sheet.querySelectorAll(":scope > .cell-row"))) {
      const walker = document.createTreeWalker(row, NodeFilter.SHOW_TEXT);
      const nodes: Text[] = [];
      let rowText = "";
      for (let node = walker.nextNode(); node; node = walker.nextNode()) {
        const chunk = node as Text;
        if (chunk.data.length === 0) continue;
        nodes.push(chunk);
        rowText += chunk.data;
      }
      const start = rowText.indexOf(text);
      if (start < 0) continue;
      const end = start + text.length;
      const range = document.createRange();
      let offset = 0;
      let anchored = false;
      for (const node of nodes) {
        const next = offset + node.data.length;
        if (!anchored && start >= offset && start < next) {
          range.setStart(node, start - offset);
          anchored = true;
        }
        if (end > offset && end <= next) range.setEnd(node, end - offset);
        offset = next;
      }
      const markerRect = range.getBoundingClientRect();
      if (!anchored || markerRect.width <= 0 || markerRect.height <= 0) continue;
      const gridRect = grid.getBoundingClientRect();
      const visual = window.visualViewport;
      const visualLeft = visual?.offsetLeft ?? 0;
      const clipRight = Math.min(gridRect.right, visualLeft + (visual?.width ?? innerWidth));
      return {
        markerRight: markerRect.right,
        clipRight,
        overflowPx: markerRect.right - clipRight,
        scrollLeft: grid.scrollLeft,
      };
    }
    return null;
  }, { id: sessionId, text: marker });
  if (!edge) throw new Error(`${label}: ${marker} left the grid before it could be measured`);
  expect(
    edge.overflowPx,
    `${label}: last glyph right edge ${edge.markerRight} against clip ${edge.clipRight}`,
  ).toBeLessThanOrEqual(FIT_TOLERANCE_PX);
  expect(edge.scrollLeft, `${label}: the terminal never scrolled horizontally`).toBe(0);
}

/** Address the shell's cursor at the settled grid's final columns and prove the
 *  glyph landing there is inside every viewer's clip. The marker reaches the
 *  PTY as two arguments, so the shell's echo of the command line cannot contain
 *  it and text presence alone can never satisfy the proof. */
export async function paintAndProveLastColumn(
  viewers: readonly Page[],
  sender: Page,
  sessionId: string,
  label: string,
): Promise<number> {
  // Fit first: a viewer still painting a superseded grid overflows its pane,
  // so the settled sheet is what makes the column read the final one.
  for (const viewer of viewers) await expectSheetFitsPane(viewer, sessionId, label);
  const cols = await waitForConvergedColumns(viewers, sessionId, label);
  const perViewerRows = await Promise.all(
    viewers.map((viewer) => readFullyVisibleRows(viewer, sessionId)),
  );
  const shared = perViewerRows.reduce<readonly number[]>(
    (rows, viewerRows) => rows.filter((row) => viewerRows.includes(row)),
    perViewerRows[0] ?? [],
  );
  if (shared.length === 0) throw new Error(`${label}: no row is fully visible in every viewer`);
  const row = shared[Math.floor(shared.length / 2)];
  const tail = crypto.randomUUID().replaceAll("-", "").slice(0, 8).toUpperCase();
  const marker = `${MARKER_HEAD}${tail}`;
  const column = cols - marker.length + 1;
  expect(column, `${label}: the settled grid is wider than one marker`).toBeGreaterThan(1);
  await inputSmokeTerminal(
    sender,
    sessionId,
    `printf '\\033[${row};${column}H%s%s' '${MARKER_HEAD}' '${tail}'\r`,
  );
  for (const viewer of viewers) await expectLastColumnVisible(viewer, sessionId, marker, label);
  return cols;
}
