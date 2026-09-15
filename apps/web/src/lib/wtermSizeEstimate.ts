// Initial PTY cols/rows hint for a session that does not exist yet. The keeper
// reads ROOST_PTY_COLS/ROWS at PTY start, so a TUI (vim) paints at the real
// width from byte 0 instead of the 220×50 keeper default; the resize message
// that follows then redraws without pre-resize wrap artifacts.
//
// Source box and math are the live claim's (terminalCellGeometry.ts), so the
// hint equals the geometry this browser claims a moment later. No mounted
// display box means NO hint: these cols/rows reach the PTY directly, and a
// wrong size is worse than the keeper default. Re-measures per call — a cached
// value survives a window resize or zoom change and lies.

import type { TerminalGeometry } from "@roost/shared/viewport";
import {
  measureTerminalCellBox,
  terminalGeometryForBox,
} from "./terminalCellGeometry.ts";

const DISPLAY_SELECTOR = '[data-testid="terminal-display"]';

/** The box a mounted terminal paints into — the same element the live claim
 *  measures. A parked pane stays laid out off-screen at a retained size
 *  (terminal-deck-geometry.ts), so only a computed-visible box describes what
 *  a new session will get. The focused pane wins a split, because that is the
 *  pane a new tab opens into. */
function mountedTerminalDisplay(): HTMLElement | null {
  let fallback: HTMLElement | null = null;
  for (const box of document.querySelectorAll(DISPLAY_SELECTOR)) {
    const display = box as HTMLElement;
    if (getComputedStyle(display).visibility !== "visible") continue;
    if (display.clientWidth <= 0 || display.clientHeight <= 0) continue;
    if (display.closest('[data-pane-slot][data-focused="true"]')) return display;
    fallback ??= display;
  }
  return fallback;
}

export function estimateWtermSize(): TerminalGeometry | null {
  const display = mountedTerminalDisplay();
  if (!display) return null;
  const cell = measureTerminalCellBox(display);
  if (!cell) return null;
  return terminalGeometryForBox(display, cell);
}
