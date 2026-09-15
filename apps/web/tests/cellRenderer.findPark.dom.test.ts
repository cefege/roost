// CellGridRenderer DOM tripwire — the find park's scroll contract.
//
// A find park owns the anchor it scrolled to, so it survives scrolling around
// history and the renderer's own navigation write. A USER scroll that lands on
// the exact bottom is the universal return to live and must release it. Closing
// the bar ends the interval without moving the view: the park downgrades to an
// ordinary scroll park, because closeFind() itself never resumes the reader.

import { describe, test, expect } from "bun:test";
import { CellGridRenderer } from "../src/lib/cellRenderer.ts";
import { spansText, type CellGridFrame, type CellRow } from "@roost/shared/cell";
import { createTerminalFind } from "../src/lib/terminalFindController.ts";
import {
  PAD_TOP,
  ROW_PX,
  makeContainer,
  row,
  deltaFrame,
  seedHeldHistory,
  vpEl,
} from "./helpers/cellRendererFakeDom.ts";

const openBar = (renderer: CellGridRenderer) => createTerminalFind({
  sessionId: "session-1",
  renderer: () => renderer,
  backfill: () => null,
});

describe("CellGridRenderer DOM — find park scroll contract", () => {
  const nRows = (n: number, from = 0) =>
    Array.from({ length: n }, (_, i) => row(from + i, `s${from + i}`));
  const appDelta = (append: CellRow[], total: number, seq: number): CellGridFrame =>
    ({ ...deltaFrame(80, 1, [row(0, "v")], append, seq), scrollbackTotal: total });
  const newerFrame = (): CellGridFrame =>
    ({ ...appDelta([row(400, "new")], 401, 3), viewportRows: [row(0, "latest-v")] });
  // A pane parked on a find hit in mid-history, with a newer frame retained.
  const findPark = () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    seedHeldHistory(r, 80, [row(0, "v")], nRows(400));
    r.scrollToScrollbackRow(50);
    r.handleScroll(); // the find-owned write's own event
    r.apply(newerFrame());
    return { c, r };
  };

  test("a find park survives a scroll that does not reach the bottom", () => {
    const { c, r } = findPark();
    c.scrollTop = PAD_TOP + 120 * ROW_PX;
    c.resetScrollTopWrites();

    expect(r.handleScroll()).toEqual({ reconciled: false, anchorChanged: false });

    expect(r.readerIntent).toBe("reading");
    expect(r.readerReason).toBe("find");
    expect(r.currentFrame!.seq).toBe(2);
    expect(c.scrollTopWrites).toBe(0);
  });

  test("a renderer-owned write that lands at the bottom keeps the find park", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    seedHeldHistory(r, 80, [row(0, "v")], nRows(400));
    c.scrollTop = PAD_TOP + 50 * ROW_PX;
    r.handleScroll(); // a user scroll parks the reader off the bottom

    r.scrollToScrollbackRow(399); // a tail hit clamps the find write onto the bottom
    expect(c.scrollTop).toBe(c.scrollHeight - c.clientHeight);
    r.apply(newerFrame());
    c.resetScrollTopWrites();

    expect(r.handleScroll()).toEqual({ reconciled: false, anchorChanged: false });

    expect(r.readerIntent).toBe("reading");
    expect(r.readerReason).toBe("find");
    expect(r.currentFrame!.seq).toBe(2);
    expect(c.scrollTopWrites).toBe(0);
  });

  test("a user scroll to the exact bottom resumes a find park", () => {
    const { c, r } = findPark();
    c.scrollTop = c.scrollHeight - c.clientHeight;

    expect(r.handleScroll()).toEqual({ reconciled: true, anchorChanged: true });

    expect(r.readerIntent).toBe("live");
    expect(r.readerReason).toBeNull();
    expect(r.currentFrame!.seq).toBe(3);
    expect(spansText(r.currentFrame!.viewportRows[0]!.spans)).toBe("latest-v");
    expect(r.reconcileBlockReason()).toBeNull();
  });

  // A find park parked at the tail, whose box grew: the browser clamps it onto
  // the smaller maximum and dispatches a scroll the user never performed.
  const clampedTailPark = (grownBoxPx: number) => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    seedHeldHistory(r, 80, [row(0, "v")], nRows(400));
    c.scrollTop = PAD_TOP + 50 * ROW_PX;
    r.handleScroll();
    r.scrollToScrollbackRow(399); // a tail hit lands the park on the bottom
    r.handleScroll(); // observes the pre-grow maximum
    r.apply(newerFrame());
    c.clientHeight = grownBoxPx;
    c.scrollTop = Math.max(0, c.scrollHeight - c.clientHeight); // the browser's own clamp
    c.resetScrollTopWrites();
    return { c, r };
  };

  test("a box-grow clamp onto the bottom keeps a find park", () => {
    const { c, r } = clampedTailPark(700); // scroll range remains

    expect(r.handleScroll()).toEqual({ reconciled: false, anchorChanged: false });

    expect(r.readerIntent).toBe("reading");
    expect(r.readerReason).toBe("find");
    expect(r.currentFrame!.seq).toBe(2);
    expect(vpEl(c).textContent).toBe("v");
    expect(c.scrollTopWrites).toBe(0);
  });

  test("a clamp that leaves no scroll range resumes a find park", () => {
    const { c, r } = clampedTailPark(6500); // taller than the frozen content
    expect(c.scrollHeight).toBeLessThanOrEqual(c.clientHeight);

    expect(r.handleScroll().reconciled).toBe(true);

    expect(r.readerIntent).toBe("live");
    expect(r.currentFrame!.seq).toBe(3);
    expect(vpEl(c).textContent).toBe("latest-v");
  });

  // The suppression is one-shot by construction: every observed event records
  // the maximum it saw, so the gesture after a clamp sees an unchanged one.
  test("a gesture after a clamp still resumes a find park", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    seedHeldHistory(r, 80, [row(0, "v")], nRows(400));
    c.scrollTop = PAD_TOP + 50 * ROW_PX;
    r.handleScroll(); // observes the pre-grow maximum
    r.scrollToScrollbackRow(300); // a mid-history hit, well off the bottom
    r.handleScroll();
    r.apply(newerFrame());

    c.clientHeight = 1800; // the maximum drops below the parked position
    c.scrollTop = Math.max(0, c.scrollHeight - c.clientHeight);
    c.resetScrollTopWrites();
    expect(r.handleScroll()).toEqual({ reconciled: false, anchorChanged: false });
    expect(r.readerReason).toBe("find");
    expect(vpEl(c).textContent).toBe("v");
    expect(c.scrollTopWrites).toBe(0);

    expect(r.handleScroll().reconciled).toBe(true); // same maximum: a gesture
    expect(r.readerIntent).toBe("live");
    expect(vpEl(c).textContent).toBe("latest-v");

    // Nothing lingers: a fresh park on this geometry resumes like any other.
    r.scrollToScrollbackRow(300);
    r.handleScroll();
    c.scrollTop = Math.max(0, c.scrollHeight - c.clientHeight);
    expect(r.handleScroll()).toEqual({ reconciled: false, anchorChanged: false });
    expect(r.readerIntent).toBe("live");
    expect(r.readerReason).toBeNull();
  });
  test("closing the find bar ends the park without moving or painting", () => {
    const { c, r } = findPark();
    const find = openBar(r);
    find.openFind();
    const parkedTop = c.scrollTop;
    c.resetScrollTopWrites();

    find.closeFind();

    expect(c.scrollTop).toBe(parkedTop);
    expect(c.scrollTopWrites).toBe(0);
    expect(r.currentFrame!.seq).toBe(2);
    expect(r.readerIntent).toBe("reading");
    expect(r.readerReason).toBe("native_scroll");
    find.dispose();
  });

  test("a dismissed find park follows a box grow its anchor would have refused", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    seedHeldHistory(r, 80, [row(0, "v")], nRows(400));
    c.scrollTop = PAD_TOP + 50 * ROW_PX;
    r.handleScroll();
    r.scrollToScrollbackRow(399); // a tail hit parks find on the bottom
    r.handleScroll();
    r.apply(newerFrame());
    const find = openBar(r);

    find.closeFind();
    c.clientHeight = 700; // scroll range remains: only a scroll park may follow
    r.noteBoxResize();

    expect(r.readerIntent).toBe("live");
    expect(r.currentFrame!.seq).toBe(3);
    expect(spansText(r.currentFrame!.viewportRows[0]!.spans)).toBe("latest-v");
    expect(r.reconcileBlockReason()).toBeNull();
    find.dispose();
  });
});
