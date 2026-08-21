// CellGridRenderer DOM tripwire — box geometry.
//
// Synchronous box reconciliation (ResizeObserver can run after layout and after
// another cell frame) and pointer hit-testing, whose origin is the VIEWPORT box
// rather than the scroll container.

import { describe, test, expect } from "bun:test";
import { CellGridRenderer } from "../src/lib/cellRenderer.ts";
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

  test("an epoch-changing full frame installs atomically during explicit reading", () => {
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
    expect(r.currentFrame!.gridEpoch).toBe("test-grid:1");
    expect(r.canonicalFrameSeq()).toBe(3);
    expect(r.prepareLiveInteraction()).toEqual({ reconciled: false, anchorChanged: false });
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
