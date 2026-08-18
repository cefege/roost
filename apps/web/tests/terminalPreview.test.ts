// terminalBrowserStreamSnapshot — the browser half of the layered probe.
//
// Runs a real CellGridRenderer through terminalPreview.ts's renderer registry,
// so the reported range is the one this document actually holds. No jsdom (by
// design, per cellRenderer.test.ts); the fake DOM is helpers/cellRendererFakeDom.ts.

import { describe, test, expect } from "bun:test";
import { CellGridRenderer } from "../src/lib/cellRenderer.ts";
import { registerRenderer } from "../src/lib/terminalPreview.ts";
import { terminalBrowserStreamSnapshot } from "../src/lib/terminalDiagSnapshot.ts";
import {
  makeContainer,
  row,
  seedHeldHistory,
} from "./helpers/cellRendererFakeDom.ts";

// ── the browser half of the layered history snapshot ──────────────────────
// The worker's diagnostic snapshot reports the CORE's live range and the RING's
// byte bounds; neither says what the browser actually holds, so "the browser is
// missing history" and "the worker never had it" were the same observation. The
// probe carries the held range in the SAME payload as the worker's two, read off
// the very anchor the paging controller addresses history with.
describe("terminalBrowserStreamSnapshot — the range this document holds", () => {
  const nRows = (n: number, from = 0) =>
    Array.from({ length: n }, (_, i) => row(from + i, `r${from + i}`));

  test("reports the held range, not the frame's total, and follows a prepend", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    // 250 painted rows of a 750-row history: the shape a full frame always has.
    seedHeldHistory(r, 80, [row(0, "v")], nRows(250, 500), 750);
    const release = registerRenderer("probe-session", r);

    const held = terminalBrowserStreamSnapshot("probe-session").history;
    expect(held).toEqual({
      grid_epoch: "test-grid:0",
      sb_base: 500,
      total: 750,
      cols: 80,
      rows_held: 250,
      floor: null,
    });

    // A backfill page moves the held range down; the probe must move with it or a
    // stale sb_base reads as history the browser does not actually have.
    r.prependScrollback(nRows(100, 400));
    const after = terminalBrowserStreamSnapshot("probe-session").history;
    expect(after.sb_base).toBe(400);
    expect(after.rows_held).toBe(350);
    expect(after.total).toBe(750);
    release();
  });

  test("an unregistered session reports explicit nulls rather than zeros", () => {
    // A pane that never mounted holds NO range. Zeros would read as "holds all of
    // history from row 0", which is the opposite of the truth.
    expect(terminalBrowserStreamSnapshot("never-mounted").history).toEqual({
      grid_epoch: null,
      sb_base: null,
      total: null,
      cols: null,
      rows_held: 0,
      floor: null,
    });
  });
});
