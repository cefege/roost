// CellGridRenderer DOM tripwire — reconciliation restraint.
//
// Two halves of one guarantee: a delta inspects only the rows the worker marked
// dirty, and the [0, sbBase) history spacer keeps the reader's row where it was
// across prepend / evict / reframe without a single scroll write. Both fail the
// same way — a full reconstruction that "looks right" and loses the reader.

import { describe, test, expect } from "bun:test";
import { CellGridRenderer, MAX_HELD_SCROLLBACK_ROWS } from "../src/lib/cellRenderer.ts";
import { spansText, type CellGridFrame, type CellRow } from "@roost/shared/cell";
import {
  PAD_TOP,
  ROW_PX,
  FakeEl,
  makeContainer,
  row,
  fullFrame,
  deltaFrame,
  seedHeldHistory,
  sbEl,
  vpEl,
  sbRows,
} from "./helpers/cellRendererFakeDom.ts";

// ── viewport patching — only authoritative dirty rows are inspected ──────
// A regression to full reconstruction/re-hashing either changes untouched
// node identity or trips the poisoned-row accessor below.
describe("CellGridRenderer DOM — viewport diff", () => {
  test("a content-identical delta advances reconciliation without replacing rows", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    const viewportEl = vpEl(c);
    seedHeldHistory(r, 80, [row(0, "v0"), row(1, "v1")], []);
    const n0 = viewportEl.children[0];
    const n1 = viewportEl.children[1];
    expect(r.reconciledEpochSeq()).toEqual({ grid_epoch: "test-grid:0", seq: 1 });

    // Same row hashes, mode, and cursor: reconciliation is proven by the
    // completed diff/cursor/mode path even though it performs zero row writes.
    r.apply(deltaFrame(80, 2, [row(0, "v0"), row(1, "v1")], [], 2));
    expect(viewportEl.children[0]).toBe(n0);
    expect(viewportEl.children[1]).toBe(n1);
    expect(r.canonicalEpochSeq()).toEqual({ grid_epoch: "test-grid:0", seq: 2 });
    expect(r.reconciledEpochSeq()).toEqual({ grid_epoch: "test-grid:0", seq: 2 });
    expect(r.reconcileBlockReason()).toBeNull();
  });

  test("an empty-row cursor-only delta moves the cursor and preserves every row node", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    const viewportEl = vpEl(c);
    seedHeldHistory(r, 80, [row(0, "v0"), row(1, "v1")], []);
    const n0 = viewportEl.children[0];
    const n1 = viewportEl.children[1];
    const cursor = viewportEl.children.find((child: FakeEl) => child.className === "cell-cursor") as FakeEl;

    expect(r.applyDeltaFrame({
      ...deltaFrame(80, 2, [], [], 2),
      cursorRow: 1,
      cursorCol: 3,
    })).toBe(true);

    expect(viewportEl.children[0]).toBe(n0);
    expect(viewportEl.children[1]).toBe(n1);
    expect(cursor.style.top).toBe("1lh");
    expect(cursor.style.left).toBe("3ch");
    expect(cursor.dataset).toMatchObject({ row: "1", column: "3", visible: "true" });
    expect(r.presentationSnapshot().cursor).toEqual({
      canonical: { visible: true, row: 1, column: 3 },
      dom: { visible: true, row: 1, column: 3, connected: true },
    });
    expect(r.reconciledEpochSeq()).toEqual({ grid_epoch: "test-grid:0", seq: 2 });
  });

  test("cursor-only pending state resumes cleanly without replacing row nodes", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    const viewportEl = vpEl(c);
    seedHeldHistory(r, 80, [row(0, "v0"), row(1, "v1")], []);
    const n0 = viewportEl.children[0];
    const n1 = viewportEl.children[1];

    r.enterReading("wheel");
    expect(r.applyDeltaFrame({
      ...deltaFrame(80, 2, [], [], 2),
      cursorRow: 1,
      cursorCol: 4,
    })).toBe(true);
    expect(r.presentationSnapshot().cursor).toEqual({
      canonical: { visible: true, row: 1, column: 4 },
      dom: { visible: true, row: 0, column: 0, connected: true },
    });

    expect(r.prepareLiveInteraction()).toEqual({ reconciled: true, anchorChanged: false });
    expect(viewportEl.children[0]).toBe(n0);
    expect(viewportEl.children[1]).toBe(n1);
    expect(r.presentationSnapshot().cursor).toEqual({
      canonical: { visible: true, row: 1, column: 4 },
      dom: { visible: true, row: 1, column: 4, connected: true },
    });
  });

  test("a one-row delta replaces ONLY that row's node, positionally", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    const viewportEl = vpEl(c);
    seedHeldHistory(r, 80, [row(0, "v0"), row(1, "v1")], []);
    const n0 = viewportEl.children[0];
    const n1 = viewportEl.children[1];
    r.apply(deltaFrame(80, 2, [row(1, "v1-changed")], [], 2));
    expect(viewportEl.children[0]).toBe(n0); // untouched → zero DOM writes
    expect(viewportEl.children[1]).not.toBe(n1); // replaced in place
    expect(viewportEl.children[1].children[0].textContent).toBe("v1-changed");
  });

  test("a sparse delta never reads or hashes an untouched held row", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    seedHeldHistory(r, 80, [row(0, "stable"), row(1, "old")], []);
    const stable = r.currentFrame!.viewportRows[0]!;
    Object.defineProperty(stable, "spans", {
      configurable: true,
      get: () => { throw new Error("untouched row was inspected"); },
    });

    expect(r.applyDeltaFrame(deltaFrame(80, 2, [row(1, "new")], [], 2))).toBe(true);
    expect(vpEl(c).children[1].children[0].textContent).toBe("new");
  });

  test("a viewport-only full frame rebuild prunes surplus viewport rows", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    const viewportEl = vpEl(c);
    seedHeldHistory(r, 80, [row(0, "v0"), row(1, "v1"), row(2, "v2")], []);
    const n0 = viewportEl.children[0];
    expect(viewportEl.children.length).toBe(5);
    seedHeldHistory(r, 80, [row(0, "v0")], []);
    expect(viewportEl.children.length).toBe(3);
    expect(viewportEl.children[0]).not.toBe(n0);
  });

  test("a scrolling delta REUSES shifted row nodes; only the new tail renders", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    const viewportEl = vpEl(c);
    seedHeldHistory(r, 80, [row(0, "A"), row(1, "B"), row(2, "C")], []);
    const nB = viewportEl.children[1];
    const nC = viewportEl.children[2];
    // One line scrolled out: A moved to scrollback. Only newly exposed D is
    // carried as a dirty row; B/C transfer through the canonical model + DOM.
    expect(r.applyDeltaFrame({
      ...deltaFrame(80, 3, [row(2, "D")], [row(0, "A")], 2),
      scrollbackTotal: 1,
    })).toBe(true);
    expect(viewportEl.children[0]).toBe(nB); // shifted up, node reused
    expect(viewportEl.children[1]).toBe(nC); // shifted up, node reused
    expect(viewportEl.children[2]).not.toBe(nC); // the only newly rendered row
    expect(viewportEl.children[2].children[0].textContent).toBe("D");
    expect(r.gridText()).toBe("B\nC\nD");
  });

  test("a scrolling delta without every exposed tail row requests repair", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    seedHeldHistory(r, 80, [row(0, "A"), row(1, "B"), row(2, "C")], []);
    const before = r.gridText();
    expect(r.applyDeltaFrame({
      ...deltaFrame(80, 3, [], [row(0, "A")], 2),
      scrollbackTotal: 1,
    })).toBe(false);
    expect(r.gridText()).toBe(before);
  });
});

describe("CellGridRenderer DOM — first reconciliation notification", () => {
  test("fires only after the first completed DOM reconciliation", () => {
    const c = makeContainer();
    let notifications = 0;
    const r = new CellGridRenderer(
      c as unknown as HTMLElement,
      () => { notifications += 1; },
    );

    const rejected = { ...fullFrame(80, [row(0, "rejected")]), rows: 2 };
    expect(r.applyFullFrame(rejected)).toBe(false);
    expect(notifications).toBe(0);

    r.setSelectionHold(true);
    expect(r.applyFullFrame(fullFrame(80, [row(0, "held")]))).toBe(true);
    expect(r.reconciledEpochSeq()).toEqual({ grid_epoch: null, seq: null });
    expect(notifications).toBe(0);

    expect(r.prepareLiveInteraction()).toEqual({ reconciled: true, anchorChanged: true });
    expect(r.reconciledEpochSeq()).toEqual({ grid_epoch: "test-grid:0", seq: 1 });
    expect(notifications).toBe(1);

    expect(r.applyDeltaFrame(deltaFrame(80, 1, [row(0, "delta")], [], 2))).toBe(true);
    expect(r.applyFullFrame({
      ...fullFrame(80, [row(0, "later-full")]),
      seq: 3,
    })).toBe(true);
    expect(notifications).toBe(1);
  });
});

// ── truthful scroll space: the [0, sbBase) history spacer ─────────────────
// A full frame ships only a scrollback TAIL, so the painted DOM used to occupy
// the WHOLE scroll space while describing ~250 rows: every backfill prepend
// grew scrollHeight (thumb shrank + jumped with no user action) and a reframe's
// replaceChildren left the browser's pixel offset over completely different
// rows (the "scrollbar all over the place after a tab switch" report).
// .cell-sb-spacer reserves the unpainted history, so an absolute row index has
// a FIXED pixel offset for the epoch and native scrollTop preserves the
// reader's row across prepend / evict / reframe with ZERO scroll writes.
describe("CellGridRenderer DOM — truthful scroll space", () => {
  const spEl = (c: FakeEl): FakeEl => c.children.find((x: FakeEl) => x.className === "cell-sb-spacer") as FakeEl;
  const spPx = (c: FakeEl): number => parseFloat(String(spEl(c).style.height));
  const nRows = (n: number, from = 0) =>
    Array.from({ length: n }, (_, i) => row(from + i, `r${from + i}`));
  const appDelta = (append: CellRow[], total: number, seq: number): CellGridFrame =>
    ({ ...deltaFrame(80, 1, [row(0, "v")], append, seq), scrollbackTotal: total });
  // The painted row the reader's pixel offset lands on, by GEOMETRY — the one
  // question the smoke helpers can't answer and the bug was hiding in.
  const rowAtReader = (c: FakeEl): string | undefined => {
    const sb = sbEl(c);
    const i = Math.floor((c.scrollTop - sb.offsetTop) / ROW_PX);
    return sbRows(sb)[i]?.children[0]?.textContent;
  };

  test("the spacer reserves the unpainted history", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    seedHeldHistory(r, 80, [row(0, "v")], nRows(250, 500), 750);

    expect(spEl(c).style.height).toBe("8000.00px"); // 500 unpainted rows × 16px
    // 750 rows of history in the scroll space, not the 250 that are painted.
    expect(c.scrollHeight).toBe(PAD_TOP + (750 + 1) * ROW_PX); // +1 viewport row
    // A reader in reserved space is "near the painted top" → the drain pulls to them.
    c.scrollTop = PAD_TOP + 100 * ROW_PX;
    expect(r.nearHistoryTop()).toBe(true);
  });

  test("a backfill prepend shrinks the spacer by exactly the rows it adds", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    seedHeldHistory(r, 80, [row(0, "v")], nRows(250, 500), 750);
    c.scrollTop = PAD_TOP + 600 * ROW_PX; // off the bottom, inside painted history
    const heightBefore = c.scrollHeight;
    const readerBefore = rowAtReader(c);
    c.resetScrollTopWrites();

    r.prependScrollback(nRows(250, 250));

    expect(spEl(c).style.height).toBe("4000.00px"); // 8000 - 250×16
    expect(c.scrollHeight).toBe(heightBefore);      // the thumb does not move
    expect(c.scrollTopWrites).toBe(0);
    expect(rowAtReader(c)).toBe(readerBefore);      // same absolute row, same offset
    expect(readerBefore).toBe("r600");
  });

  test("an eviction grows the spacer by exactly the rows it drops", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    const held = 1900;
    seedHeldHistory(r, 80, [row(0, "v")], nRows(held, 500), 500 + held);
    c.scrollTop = Math.max(0, c.scrollHeight - c.clientHeight); // literal bottom
    const heightBefore = c.scrollHeight;
    const spacerBefore = spPx(c);

    r.apply(appDelta(nRows(250, 500 + held), 750 + held, 2)); // held 2150 > cap

    const dropped = r.currentFrame!.sbBase - 500; // rows the evictor pushed back into the hole
    expect(dropped).toBe(250);
    expect(r.currentFrame!.scrollbackRows.length).toBeLessThanOrEqual(MAX_HELD_SCROLLBACK_ROWS);
    expect(spPx(c)).toBe(spacerBefore + dropped * ROW_PX);
    // Net scroll space = the 250 rows that arrived. The eviction itself is free:
    // the spacer absorbs every dropped row, so no row above the reader moves.
    expect(c.scrollHeight).toBe(heightBefore + 250 * ROW_PX);
  });


  test("a same-epoch streaming repair freezes explicit reading until atomic resume", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    seedHeldHistory(r, 80, [row(0, "old-v")], nRows(250, 500), 750);
    const reconciledBefore = r.reconciledEpochSeq();
    c.scrollTop = PAD_TOP + 600 * ROW_PX;
    r.handleScroll();
    const readerBefore = rowAtReader(c);
    const heightBefore = c.scrollHeight;
    c.resetScrollTopWrites();

    r.apply({ ...fullFrame(80, [row(0, "repair-v")], 760), seq: 3 });
    r.apply({
      ...deltaFrame(80, 1, [row(0, "latest-v")], [row(760, "live-760")], 4),
      scrollbackTotal: 761,
    });

    expect(r.currentFrame!.scrollbackTotal).toBe(750);
    expect(spansText((r.currentFrame!.viewportRows[0]!).spans)).toBe("old-v");
    expect(r.heldFrameSeq()).toBe(4);
    expect(r.canonicalEpochSeq()).toEqual({ grid_epoch: "test-grid:0", seq: 4 });
    expect(r.reconciledEpochSeq()).toEqual(reconciledBefore);
    expect(r.reconcileBlockReason()).toBe("reader_pending_frame");
    expect(c.scrollHeight).toBe(heightBefore);
    expect(c.scrollTopWrites).toBe(0);
    expect(rowAtReader(c)).toBe(readerBefore);

    expect(r.prepareLiveInteraction()).toEqual({ reconciled: true, anchorChanged: true });
    expect(r.currentFrame!.scrollbackTotal).toBe(761);
    expect(r.currentFrame!.sbBase).toBe(760);
    expect(r.currentFrame!.scrollbackRows[0]!.index).toBe(760);
    expect(spansText((r.currentFrame!.viewportRows[0]!).spans)).toBe("latest-v");
    expect(c.scrollTopWrites).toBe(1);
    expect(r.atBottom()).toBe(true);
    expect(r.reconciledEpochSeq()).toEqual({ grid_epoch: "test-grid:0", seq: 4 });
    expect(r.reconcileBlockReason()).toBeNull();
  });

  test("releasing selection alone preserves reading and pending canonical state", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    seedHeldHistory(r, 80, [row(0, "old-v")], nRows(250, 500), 750);
    c.scrollTop = PAD_TOP + 600 * ROW_PX;
    r.setSelectionHold(true);
    r.apply({ ...fullFrame(80, [row(0, "latest-v")], 760), seq: 3 });

    expect(r.setSelectionHold(false)).toEqual({ reconciled: false, anchorChanged: false });
    expect(r.currentFrame!.seq).toBe(2);
    expect(r.readerIntent).toBe("reading");
    c.resetScrollTopWrites();
    expect(r.prepareLiveInteraction()).toEqual({ reconciled: true, anchorChanged: true });
    expect(r.currentFrame!.seq).toBe(3);
    expect(spansText((r.currentFrame!.viewportRows[0]!).spans)).toBe("latest-v");
    expect(c.scrollTop).toBe(c.scrollHeight - c.clientHeight);
    expect(c.scrollTopWrites).toBe(1);
  });
});
