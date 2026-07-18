// Guards the drag-to-tile "doesn't trigger" regression: PaneStrip armed the tab
// drag on HORIZONTAL delta only, so a straight-down split-drag never started.
// dragArmed must arm on travel in ANY direction. The straight-down case below
// returns false under the old X-only gate (dx=0) — that's the regression.

import { describe, test, expect } from "bun:test";
import { dragArmed, DRAG_THRESHOLD_PX } from "../src/lib/dragThreshold.ts";

describe("dragArmed", () => {
  const start = { x: 100, y: 100 };

  test("arms on straight-down travel (the split-drag regression)", () => {
    expect(dragArmed(start, 100, 100 + DRAG_THRESHOLD_PX + 3)).toBe(true);
  });

  test("arms in every other direction past threshold", () => {
    expect(dragArmed(start, 100, 91)).toBe(true);   // up 9px
    expect(dragArmed(start, 109, 100)).toBe(true);  // right 9px
    expect(dragArmed(start, 91, 100)).toBe(true);   // left 9px
    expect(dragArmed(start, 106, 108)).toBe(true);  // diagonal: hypot(6,8)=10
  });

  test("does not arm below threshold in any direction", () => {
    expect(dragArmed(start, 100, 104)).toBe(false); // down 4px
    expect(dragArmed(start, 102, 102)).toBe(false); // diagonal ~2.83px
    expect(dragArmed(start, 100, 100)).toBe(false); // no movement
  });
});
