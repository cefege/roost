// CellGridRenderer DOM tripwire — box geometry.
//
// Synchronous box reconciliation (ResizeObserver can run after layout and after
// another cell frame) and pointer hit-testing, whose origin is the VIEWPORT box
// rather than the scroll container.

import { describe, test, expect } from "bun:test";
import { CellGridRenderer, RENDERER_HOLD_SELECTION } from "../src/lib/cellRenderer.ts";
import { spansText } from "@roost/shared/cell";
import { cellFromPoint } from "../src/lib/terminalMouse.ts";
import {
  PAD_TOP,
  ROW_PX,
  CELL_PX,
  PANE_PX,
  FakeEl,
  makeContainer,
  row,
  fullFrame,
  seedHeldHistory,
  sbEl,
  vpEl,
} from "./helpers/cellRendererFakeDom.ts";

// ── synchronous box reconciliation ───────────────────────────────────────
// ResizeObserver can run after layout and after another cell frame. The old
// literal-bottom sample repairs that interleave immediately, while explicit
// off-bottom reading remains untouched.
describe("CellGridRenderer DOM — box resize + unreachable window", () => {
  const spPx = (c: FakeEl): number =>
    parseFloat(String((c.children.find((x: FakeEl) => x.className === "cell-sb-spacer") as FakeEl).style.height));
  const nRows = (n: number, from = 0) =>
    Array.from({ length: n }, (_, i) => row(from + i, `b${from + i}`));

  test("a live old-bottom anchor follows a box shrink with exactly one pin", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    seedHeldHistory(r, 80, [row(0, "v")], nRows(400));
    c.scrollTop = Math.max(0, c.scrollHeight - c.clientHeight);
    c.resetScrollTopWrites();

    c.clientHeight = 400; // divider drag / window resize under a parked pane
    r.noteBoxResize();

    expect(c.scrollTop).toBe(c.scrollHeight - c.clientHeight);
    expect(c.scrollTopWrites).toBe(1);
    expect(r.atBottom()).toBe(true);

    r.noteBoxResize(); // same height again — observer re-tick is a no-op
    expect(c.scrollTopWrites).toBe(1);
  });

  test("old-bottom resize reconciles a frame that arrived after layout without scroll", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    seedHeldHistory(r, 80, [row(0, "old")], nRows(400));
    c.scrollTop = Math.max(0, c.scrollHeight - c.clientHeight);
    r.handleScroll(); // consume the seed pin's owned event

    c.clientHeight = 400; // layout happens before ResizeObserver
    r.handleScroll(); // model the geometry event that briefly looks native
    expect(r.readerIntent).toBe("reading");
    expect(r.apply({
      ...fullFrame(80, [row(0, "after-layout")], 410),
      scrollbackTotal: 410,
      sbBase: 410,
      seq: 3,
    })).toBe(true);
    expect(r.currentFrame!.seq).toBe(2);
    c.resetScrollTopWrites();

    expect(r.noteBoxResize()).toEqual({ reconciled: true, anchorChanged: true });
    expect(r.currentFrame!.seq).toBe(3);
    expect(spansText((r.currentFrame!.viewportRows[0]!).spans)).toBe("after-layout");
    expect(r.readerIntent).toBe("live");
    expect(c.scrollTop).toBe(c.scrollHeight - c.clientHeight);
    expect(c.scrollTopWrites).toBe(1);
  });

  test("a pending owned bottom placement repins a compatible full after late geometry", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    seedHeldHistory(r, 80, [row(0, "old")], nRows(400));
    const bottom = c.scrollHeight - c.clientHeight;
    c.scrollTop = bottom - ROW_PX;
    r.prepareLiveInteraction();

    c.clientHeight -= ROW_PX;
    expect(r.atBottom()).toBe(false);
    expect(r.handleScroll()).toEqual({ reconciled: false, anchorChanged: false });
    c.resetScrollTopWrites();

    expect(r.apply({ ...fullFrame(80, [row(0, "after-layout")], 401), seq: 3 })).toBe(true);

    expect(r.readerIntent).toBe("live");
    expect(c.scrollTop).toBe(c.scrollHeight - c.clientHeight);
    expect(c.scrollTopWrites).toBe(1);
  });

  test("a user scroll clears a late owned placement before a compatible full", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    seedHeldHistory(r, 80, [row(0, "old")], nRows(400));
    const bottom = c.scrollHeight - c.clientHeight;
    c.scrollTop = bottom - ROW_PX;
    r.prepareLiveInteraction();

    c.clientHeight -= ROW_PX;
    r.handleScroll();
    const readerTop = c.scrollTop - ROW_PX;
    c.scrollTop = readerTop;
    expect(r.handleScroll()).toEqual({ reconciled: false, anchorChanged: false });
    expect(r.readerIntent).toBe("reading");
    c.resetScrollTopWrites();

    expect(r.apply({ ...fullFrame(80, [row(0, "new")], 401), seq: 3 })).toBe(true);

    expect(r.currentFrame!.seq).toBe(2);
    expect(c.scrollTop).toBe(readerTop);
    expect(c.scrollTopWrites).toBe(0);
  });

  test("off-bottom reader is untouched by a box shrink", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    seedHeldHistory(r, 80, [row(0, "v")], nRows(400));
    c.scrollTop = c.scrollHeight - c.clientHeight - 2; // >1px above the old bottom
    const before = c.scrollTop;
    c.resetScrollTopWrites();

    c.clientHeight = 400;
    r.noteBoxResize();

    expect(c.scrollTop).toBe(before);
    expect(c.scrollTopWrites).toBe(0);
  });

  test("at-bottom reader follows a box grow onto the new bottom", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    seedHeldHistory(r, 80, [row(0, "v")], nRows(400));
    c.scrollTop = Math.max(0, c.scrollHeight - c.clientHeight);
    c.resetScrollTopWrites();

    // Grow: the scroll maximum DROPS below the held scrollTop. A real browser
    // clamps scrollTop onto the new bottom; the fake has no clamp, so the pin
    // is what lands the reader there — max(prev, h) reads the over-max offset
    // as at-bottom either way.
    c.clientHeight = 700;
    r.noteBoxResize();

    expect(c.scrollTop).toBe(c.scrollHeight - c.clientHeight);
    expect(c.scrollTopWrites).toBe(1);
    expect(r.atBottom()).toBe(true);
  });

  // Park a pane with `reason`, leave a newer frame retained off-DOM, and hand
  // back the box so each case only states the grow it models.
  const parkedPane = (
    reason: "wheel" | "find",
    historyRows: number,
    boxPx: number,
    parkRow: number | "bottom",
  ) => {
    const c = makeContainer();
    c.clientHeight = boxPx;
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    seedHeldHistory(r, 80, [row(0, "v")], nRows(historyRows));
    c.scrollTop = parkRow === "bottom"
      ? c.scrollHeight - c.clientHeight
      : PAD_TOP + parkRow * ROW_PX;
    r.enterReading(reason);
    r.apply({ ...fullFrame(80, [row(0, "latest")], historyRows + 10), seq: 3 });
    c.resetScrollTopWrites();
    return { c, r };
  };

  // A grow past the frozen content leaves scrollHeight === clientHeight: the box
  // can never fire another scroll event, so this observer tick is the only
  // resume the pane will ever get, whatever gesture parked it.
  test("a wheel-parked reader resumes when a box grow leaves no scroll range", () => {
    const { c, r } = parkedPane("wheel", 20, 100, 4);
    expect(r.currentFrame!.seq).toBe(2);
    expect(vpEl(c).textContent).toBe("v");
    expect(r.reconcileBlockReason()).toBe("reader_pending_frame");

    c.clientHeight = 400;
    expect(c.scrollHeight).toBeLessThanOrEqual(c.clientHeight);
    r.noteBoxResize();

    expect(r.readerIntent).toBe("live");
    expect(r.currentFrame!.seq).toBe(3);
    expect(vpEl(c).textContent).toBe("latest");
    expect(r.reconcileBlockReason()).toBeNull();
  });

  test("a wheel reader off the old bottom keeps its park across a box grow", () => {
    const { c, r } = parkedPane("wheel", 400, 500, 100);
    const parked = c.scrollTop;

    c.clientHeight = 700; // grown, but the content still dwarfs the box
    r.noteBoxResize();

    expect(r.readerIntent).toBe("reading");
    expect(r.readerReason).toBe("wheel");
    expect(vpEl(c).textContent).toBe("v");
    expect(c.scrollTop).toBe(parked);
    expect(c.scrollTopWrites).toBe(0);
  });

  test("a find park resumes only when the grow leaves no scroll range", () => {
    const unreachable = parkedPane("find", 20, 100, 4);
    unreachable.c.clientHeight = 400;
    unreachable.r.noteBoxResize();
    expect(unreachable.r.readerIntent).toBe("live");
    expect(vpEl(unreachable.c).textContent).toBe("latest");

    // Range left: the hit is still reachable, so its anchor outranks the grow.
    const ranged = parkedPane("find", 400, 500, "bottom");
    ranged.c.clientHeight = 700;
    ranged.r.noteBoxResize();
    expect(ranged.r.readerIntent).toBe("reading");
    expect(ranged.r.readerReason).toBe("find");
    expect(vpEl(ranged.c).textContent).toBe("v");
    expect(ranged.c.scrollTopWrites).toBe(0);
  });

  // A paint hold cannot be resumed through, so the pane must keep REPORTING the
  // park that owns it: a live/null pane with a set mask hides the block reason
  // from every diagnostic and from the stall watchdog.
  test("a held pane keeps its reader identity when a scroll returns to the bottom", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    seedHeldHistory(r, 80, [row(0, "v")], nRows(400));
    const bottom = c.scrollHeight - c.clientHeight;
    c.scrollTop = bottom;
    r.handleScroll(); // consume the seed pin's owned event
    r.setSelectionHold(true);

    c.scrollTop = bottom - 5 * ROW_PX; // the drag scrolls the pane up
    expect(r.handleScroll()).toEqual({ reconciled: false, anchorChanged: false });
    c.scrollTop = bottom; // ... and back onto the exact bottom
    c.resetScrollTopWrites();
    expect(r.handleScroll()).toEqual({ reconciled: false, anchorChanged: false });

    expect(r.readerIntent).toBe("reading");
    expect(r.readerReason).toBe("selection");
    expect(r.holdMask).toBe(RENDERER_HOLD_SELECTION);
    expect(r.reconcileBlockReason()).toBe("selection_hold");
    expect(c.scrollTopWrites).toBe(0);

    expect(r.apply({ ...fullFrame(80, [row(0, "held-latest")], 410), seq: 3 })).toBe(true);
    expect(vpEl(c).textContent).toBe("v");

    expect(r.setSelectionHold(false)).toEqual({ reconciled: true, anchorChanged: true });
    expect(r.readerIntent).toBe("live");
    expect(vpEl(c).textContent).toBe("held-latest");
    expect(r.reconcileBlockReason()).toBeNull();
  });

  test("an epoch-changing full frame waits off-DOM during explicit reading", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    seedHeldHistory(r, 80, [row(0, "v")], nRows(250, 500), 750);
    c.scrollTop = PAD_TOP + 600 * ROW_PX;
    r.handleScroll();
    const before = c.scrollTop;
    c.resetScrollTopWrites();

    r.apply({
      ...fullFrame(80, [row(0, "v")], 5000),
      gridEpoch: "test-grid:1",
      scrollbackTotal: 5000,
      sbBase: 5000,
      seq: 3,
    });

    expect(c.scrollTop).toBe(before);
    expect(c.scrollTopWrites).toBe(0);
    expect(r.currentFrame!.gridEpoch).toBe("test-grid:0");
    expect(r.canonicalFrameSeq()).toBe(3);
    expect(r.prepareLiveInteraction()).toEqual({ reconciled: true, anchorChanged: true });
    expect(r.currentFrame!.gridEpoch).toBe("test-grid:1");
    expect(c.scrollTop).toBe(c.scrollHeight - c.clientHeight);
    expect(c.scrollTopWrites).toBe(1);
  });

  test("renderFull reserves the incoming spacer BEFORE wiping painted history", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    seedHeldHistory(r, 80, [row(0, "v")], nRows(250, 500), 750);
    c.scrollTop = Math.max(0, c.scrollHeight - c.clientHeight);
    const preScrollTop = c.scrollTop;

    // Spy the wipe: at the instant .cell-scrollback is cleared, the spacer must
    // already hold the INCOMING frame's reserve — the scroll maximum never dips
    // below the reader's offset, so the browser never clamps them into blank
    // space (whose scroll event would start a top-down backfill drain).
    const sb = sbEl(c);
    const orig = sb.replaceChildren.bind(sb);
    let spacerAtWipe = -1;
    let heightAtWipe = -1;
    sb.replaceChildren = (...kids: unknown[]) => {
      orig(...kids);
      spacerAtWipe = spPx(c);
      heightAtWipe = c.scrollHeight;
    };

    // Width change → slow path → renderFull replaceChildren.
    r.apply({ ...fullFrame(100, [row(0, "v")], 6000), gridEpoch: "test-grid:1", seq: 3 });

    expect(spacerAtWipe).toBe(6000 * ROW_PX);
    expect(heightAtWipe).toBeGreaterThanOrEqual(preScrollTop);
  });
});

// ── pointer hit-test geometry: the viewport IS the origin ─────────────────
// Forwarded mouse reports used to derive their cell from the SCROLL
// CONTAINER's rect. Inside that container the history spacer and the
// append-only scrollback sheet sit ABOVE .cell-viewport, so the container's top
// is (painted history − scrollTop) above row 1 and every click reported a row
// that far down the grid — the user had to aim centimetres high. It only ever
// looked right on a fresh alt-screen pane, where both are display:none.
describe("CellGridRenderer DOM — viewportCellGeometry", () => {
  const nRows = (n: number, from = 0) =>
    Array.from({ length: n }, (_, i) => row(from + i, `g${from + i}`));
  const vpRows = (n: number) => Array.from({ length: n }, (_, i) => row(i, `v${i}`));

  test("geometry is the VIEWPORT's box, not the scroll container's", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    // 300 rows of painted history (plus 500 unpainted, held by the spacer) above
    // a 24-row live grid — an ordinary pane that has produced output.
    seedHeldHistory(r, 80, vpRows(24), nRows(300, 500), 800);
    c.scrollTop = 120;

    const geometry = r.viewportCellGeometry()!;
    expect(geometry).not.toBeNull();
    // Row 1 of the grid begins below the spacer AND the painted scrollback.
    const containerTop = c.getBoundingClientRect().top;
    const historyPx = (500 + 300) * ROW_PX;
    expect(geometry.top).toBe(containerTop + PAD_TOP - 120 + historyPx);
    // The regression this pins: the container's top is nowhere near row 1.
    expect(geometry.top - containerTop).toBeGreaterThan(historyPx - 120);
    expect(vpEl(c).getBoundingClientRect().top).toBe(geometry.top);

    // Exact cell box: rowHeight() for the row, and the cols-pinned viewport
    // width divided by cols for the column advance (no probe rounding).
    expect(geometry.rowHeight).toBe(ROW_PX);
    expect(geometry.cellWidth).toBe(CELL_PX);
    expect(geometry.left).toBe(vpEl(c).getBoundingClientRect().left);
    expect(geometry.cols).toBe(80);
    expect(geometry.rows).toBe(24);
  });

  test("a click resolves to the row the user aimed at, over painted history", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    seedHeldHistory(r, 80, vpRows(24), nRows(300, 500), 800);
    c.scrollTop = 120;

    const geometry = r.viewportCellGeometry()!;
    // Aim at the middle of grid row 5, column 3.
    const x = geometry.left + 2 * CELL_PX + CELL_PX / 2;
    const y = geometry.top + 4 * ROW_PX + ROW_PX / 2;
    expect(cellFromPoint(geometry, x, y)).toEqual({ col: 3, row: 5 });
    // Letterbox margin to the right of the grid, and past the last row.
    expect(cellFromPoint(geometry, geometry.left + PANE_PX, y).col).toBe(80);
    expect(cellFromPoint(geometry, x, geometry.top + 24 * ROW_PX + 4).row).toBe(24);
    // A container-relative hit-test would have landed far down the grid; with
    // the viewport origin, the container's top clamps to row 1.
    expect(cellFromPoint(geometry, x, c.getBoundingClientRect().top).row).toBe(1);
  });

  test("no frame and an unmeasurable viewport box report no geometry", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    expect(r.viewportCellGeometry()).toBeNull(); // pre-first-frame

    seedHeldHistory(r, 80, vpRows(2), []);
    expect(r.viewportCellGeometry()).not.toBeNull();
    // Detached / zero-size layout: the cell advance is unknowable, so hit-testing
    // must fall back rather than divide by zero.
    const viewportEl = vpEl(c);
    viewportEl.getBoundingClientRect = () =>
      ({ height: 0, width: 0, top: 0, left: 0, bottom: 0, right: 0 });
    expect(r.viewportCellGeometry()).toBeNull();
  });
});
