// Page-geometry tests for the scrollback pager: which absolute rows one demand
// wave asks for. Calls scrollDemandBounds/findDemandBounds directly, with no
// RPC mock and no renderer, so each bound is pinned by arithmetic alone. The
// painted base argument is the renderer's `_paintedSbBase` — the seam between
// the head spacer and the gap elements, which no single page may span.

import { describe, expect, test } from "bun:test";
import {
  BACKFILL_FETCH_ROWS,
  findDemandBounds,
  scrollDemandBounds,
} from "../src/lib/scrollbackDemandBounds.ts";

/** A renderer scroll target: the whole missing interval, plus the part of it
 *  the read-ahead window exposed. */
function target(start: number, end: number, focusRow: number, visibleEnd: number) {
  return { start, end, focusRow, visibleEnd };
}

describe("scrollback demand bounds", () => {
  test("a steady scroll-up page ends at the painted edge and extends a page older", () => {
    // Reader at 4934 with 4935+ painted: the window exposes that blank edge.
    expect(scrollDemandBounds(target(0, 4935, 4434, 4935), 0, 4935)).toEqual({
      focus: 4685, start: 4685, end: 4935,
    });
  });

  test("a deep gap is bounded to the page ending at the reader's newest missing row", () => {
    expect(scrollDemandBounds(target(0, 12_000, 9500, 10_001), 0, 12_000)).toEqual({
      focus: 9751, start: 9751, end: 10_001,
    });
  });

  test("an interval whose newer edge is inside the window stretches newer to a full page", () => {
    // Parked on the OLDEST row of a hole that runs newer: under a page exists
    // older than the exposed edge, so the sliver is not what gets fetched.
    const bounds = scrollDemandBounds(target(100, 500, 100, 102), 0, 0);
    expect(bounds).toEqual({ focus: 100, start: 100, end: 350 });
    expect(bounds!.end - bounds!.start).toBe(BACKFILL_FETCH_ROWS);
  });

  test("a short interval yields only its own rows", () => {
    expect(scrollDemandBounds(target(750, 760, 750, 756), 0, 500)).toEqual({
      focus: 750, start: 750, end: 760,
    });
  });

  test("a retained floor clamps the older edge and the focus a demand may name", () => {
    expect(scrollDemandBounds(target(100, 200, 100, 151), 120, 0)).toEqual({
      focus: 120, start: 120, end: 200,
    });
    // A floor at the interval's newer edge leaves nothing fetchable.
    expect(scrollDemandBounds(target(100, 200, 100, 151), 200, 0)).toBeNull();
  });

  test("a page never spans the painted base: the reader's own side of it wins", () => {
    // The live-stack layout that stalled the pager: head spacer [0, 171), one
    // gap element [171, 671), reader dragged to the very top. [0, 250) covers
    // two placeholders and the renderer refuses it, so the page stops at 171.
    expect(scrollDemandBounds(target(0, 671, 0, 33), 0, 171)).toEqual({
      focus: 0, start: 0, end: 171,
    });
    // Same interval, reader parked ABOVE the base: taking the head side would
    // leave its visible rows blank for a whole wave, so the gap side wins.
    expect(scrollDemandBounds(target(0, 671, 0, 201), 0, 171)).toEqual({
      focus: 171, start: 171, end: 421,
    });
  });

  test("a find page takes the base side its match row sits on, focus included", () => {
    const below = findDemandBounds({ start: 0, end: 671 }, 100, 0, 171);
    expect(below).toEqual({ focus: 100, start: 0, end: 171 });
    const above = findDemandBounds({ start: 0, end: 671 }, 200, 0, 171);
    expect(above).toEqual({ focus: 200, start: 171, end: 421 });
    // A page the focus row is missing from would report a false success:
    // splicePage answers with the focus row, and find scrolls to it.
    for (const bounds of [below, above]) {
      expect(bounds!.focus).toBeGreaterThanOrEqual(bounds!.start);
      expect(bounds!.focus).toBeLessThan(bounds!.end);
    }
  });

  test("a find page advances forward from its focus", () => {
    expect(findDemandBounds({ start: 0, end: 12_000 }, 10_000, 0, 12_000)).toEqual({
      focus: 10_000, start: 10_000, end: 10_250,
    });
    // Under a page of rows older than the match: the bounded head page instead.
    expect(findDemandBounds({ start: 0, end: 300 }, 100, 0, 300)).toEqual({
      focus: 100, start: 0, end: 250,
    });
  });
});
