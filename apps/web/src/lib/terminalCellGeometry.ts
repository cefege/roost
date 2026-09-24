// One implementation of pixels → terminal cols/rows for this browser.
// Both the live membership claim (components/cell-terminal-viewport.ts) and the
// pre-spawn PTY size hint (wtermSizeEstimate.ts) measure through here, so a
// hint can never disagree with the claim that follows it. Callers own only
// their source box; this module owns the cell probe, the padding subtraction,
// and the refusal rule: a non-positive result is NO measurement, never 1×1.

import type { TerminalGeometry } from "@roost/protocol/viewport";

/** Per-cell advance in CSS px. Fractional on purpose: rounding the advance
 *  before the division loses a column across a full pane width. */
export interface TerminalCellBox {
  width: number;
  height: number;
}

// Ten cells per probe so sub-pixel advance averages out instead of
// accumulating into a wrong column count.
const CELL_PROBE_TEXT = "0".repeat(10);

/** Measure one cell INSIDE a mounted `.cell-grid` box. The probe has to be a
 *  descendant of that box: `.cell-grid .cell-row` (styles/sidebar.css) carries
 *  the row's line-height and `height: 1.2em`, so a probe parented anywhere
 *  else measures the UA `line-height: normal` and under-counts rows. */
export function measureTerminalCellBox(
  grid: HTMLElement,
): TerminalCellBox | null {
  const probe = document.createElement("span");
  probe.className = "cell-row";
  probe.style.position = "absolute";
  probe.style.visibility = "hidden";
  probe.style.whiteSpace = "pre";
  probe.textContent = CELL_PROBE_TEXT;
  grid.appendChild(probe);
  const rect = probe.getBoundingClientRect();
  grid.removeChild(probe);
  if (rect.width === 0 || rect.height === 0) return null;
  return { width: rect.width / CELL_PROBE_TEXT.length, height: rect.height };
}

/** Cols/rows the box can actually paint, or `null` when it is mid-layout or
 *  narrower than one cell. Null is the answer, not 1×1: a bogus geometry
 *  reaches the keeper PTY on spawn and every other viewer's smallest common
 *  geometry once claimed. `clientWidth` includes padding, so subtract it. */
export function terminalGeometryForBox(
  box: HTMLElement,
  cell: TerminalCellBox,
): TerminalGeometry | null {
  if (cell.width <= 0 || cell.height <= 0) return null;
  const styles = getComputedStyle(box);
  const paddingLeft = Number.parseFloat(styles.paddingLeft) || 0;
  const paddingRight = Number.parseFloat(styles.paddingRight) || 0;
  const paddingTop = Number.parseFloat(styles.paddingTop) || 0;
  const paddingBottom = Number.parseFloat(styles.paddingBottom) || 0;
  const usableWidth = box.clientWidth - paddingLeft - paddingRight;
  const usableHeight = box.clientHeight - paddingTop - paddingBottom;
  const cols = Math.floor(usableWidth / cell.width);
  const rows = Math.floor(usableHeight / cell.height);
  return cols > 0 && rows > 0 ? { cols, rows } : null;
}
