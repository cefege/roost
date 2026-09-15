import { beforeEach, describe, expect, test } from "bun:test";
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
  sbRows,
} from "./helpers/cellRendererFakeDom.ts";

// ── truthful scroll space: the [0, sbBase) history spacer ─────────────────
// A full may retain only a tail. The leading spacer and in-place gap pages keep
// every absolute row at a stable pixel offset while history materializes.
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
    // The pager targets the actual visible gap, not the diagnostic base.
    c.scrollTop = PAD_TOP + 100 * ROW_PX;
    expect(r.missingScrollbackRangeAtScroll()).toEqual({
      start: 0,
      end: 500,
      focusRow: 100,
    });
  });


  test("a head page shrinks the spacer by exactly the rows it adds", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    seedHeldHistory(r, 80, [row(0, "v")], nRows(250, 500), 750);
    c.scrollTop = PAD_TOP + 600 * ROW_PX; // off the bottom, inside painted history
    const heightBefore = c.scrollHeight;
    const readerBefore = rowAtReader(c);
    c.resetScrollTopWrites();

    expect(r.insertHistoryPage(nRows(250, 250), false)).toBe(true);

    expect(spEl(c).style.height).toBe("4000.00px"); // 8000 - 250×16
    expect(c.scrollHeight).toBe(heightBefore);      // the thumb does not move
    expect(c.scrollTopWrites).toBe(0);
    expect(rowAtReader(c)).toBe(readerBefore);      // same absolute row, same offset
    expect(readerBefore).toBe("r600");
  });

  test("targeted pages split a tail gap without replacing existing rows", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    seedHeldHistory(r, 80, [row(0, "v")], nRows(250, 500), 750);
    expect(r.applyFullFrame({ ...fullFrame(80, [row(0, "v")], 760), seq: 3 })).toBe(true);
    const existing = sbRows(sbEl(c)).slice();

    expect(r.missingScrollbackRange(755)).toEqual({ start: 751, end: 760 });
    expect(r.insertHistoryPage(nRows(6, 751), false)).toBe(true);
    expect(r.insertHistoryPage([row(756, "duplicate"), row(757, "new")], false)).toBe(false);
    expect(r.missingScrollbackRange(759)).toEqual({ start: 757, end: 760 });
    expect(r.paintPresentation().tailGapPx).toBe(3 * ROW_PX);
    for (let index = 0; index < existing.length; index++) {
      expect(sbRows(sbEl(c))[index]).toBe(existing[index]);
    }

    expect(r.insertHistoryPage(nRows(3, 757), false)).toBe(true);
    expect(r.hasPaintedScrollbackRange(750, 760)).toBe(true);
    expect(r.missingScrollbackRange(759)).toBeNull();
    expect(sbRows(sbEl(c))).toHaveLength(260);
  });

  test("an interior page splits only its placeholder", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    seedHeldHistory(r, 80, [row(0, "v")], nRows(250, 500), 750);
    const existing = sbRows(sbEl(c)).slice();

    expect(r.insertHistoryPage(nRows(50, 250), false)).toBe(true);
    expect(r.missingScrollbackRange(375)).toEqual({ start: 300, end: 500 });
    expect(r.insertHistoryPage(nRows(50, 350), false)).toBe(true);
    expect(r.missingScrollbackRange(325)).toEqual({ start: 300, end: 350 });
    expect(r.missingScrollbackRange(450)).toEqual({ start: 400, end: 500 });
    const after = sbRows(sbEl(c)).slice(-existing.length);
    for (let index = 0; index < existing.length; index++) {
      expect(after[index]).toBe(existing[index]);
    }
  });

  test("an eviction grows the spacer by exactly the rows it drops", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    const held = 1900;
    seedHeldHistory(r, 80, [row(0, "v")], nRows(held, 500), 500 + held);
    c.scrollTop = Math.max(0, c.scrollHeight - c.clientHeight); // literal bottom
    const heightBefore = c.scrollHeight;
    const spacerBefore = spPx(c);

    r.apply(appDelta(nRows(250, 500 + held), 750 + held, 3)); // held 2150 > cap

    const dropped = r.currentFrame!.sbBase - 500; // rows the evictor pushed back into the hole
    expect(dropped).toBe(150);
    expect(r.currentFrame!.scrollbackRows.length).toBe(MAX_HELD_SCROLLBACK_ROWS);
    expect(spPx(c)).toBe(spacerBefore + dropped * ROW_PX);
    // Net scroll space = the 250 rows that arrived. The eviction itself is free:
    // the spacer absorbs every dropped row, so no row above the reader moves.
    expect(c.scrollHeight).toBe(heightBefore + 250 * ROW_PX);
  });


  test("a live viewport-only full preserves painted history and inserts only the missing tail gap", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    seedHeldHistory(r, 80, [row(0, "old-v")], nRows(250, 500), 750);
    const historyNodes = sbRows(sbEl(c)).slice();
    const repair = { ...fullFrame(80, [row(0, "repair-v")], 760), seq: 3 };

    expect(r.applyFullFrame(repair)).toBe(true);

    // The store-owned shell passed to the renderer remains viewport-only.
    expect(repair.sbBase).toBe(760);
    expect(repair.scrollbackRows).toEqual([]);
    expect(r.currentFrame).not.toBe(repair);
    expect(r.currentFrame!.viewportRows).not.toBe(repair.viewportRows);
    expect(r.currentFrame!.sbBase).toBe(760);
    expect(r.currentFrame!.scrollbackRows).toEqual([]);
    expect(r.backfillAnchor()).toMatchObject({ sbBase: 500, total: 760 });
    expect(sbRows(sbEl(c))).toHaveLength(251);
    for (let i = 0; i < historyNodes.length; i++) {
      expect(sbRows(sbEl(c))[i]).toBe(historyNodes[i]);
    }
    expect(r.gridText()).toBe("repair-v");
    expect(r.paintPresentation()).toEqual({
      rows: [
        ...nRows(250, 500).map((entry) => ({
          index: entry.index,
          text: spansText(entry.spans),
        })),
        { index: 750, text: "old-v" },
      ],
      headSpacerPx: 500 * ROW_PX,
      tailGapPx: 9 * ROW_PX,
      readerAnchor: null,
    });

    expect(r.apply({
      ...deltaFrame(80, 1, [row(0, "delta-1")], [row(760, "live-760")], 4),
      scrollbackTotal: 761,
    })).toBe(true);
    expect(r.apply({
      ...deltaFrame(80, 1, [row(0, "delta-2")], [row(761, "live-761")], 5),
      scrollbackTotal: 762,
    })).toBe(true);
    const after = sbRows(sbEl(c));
    for (let i = 0; i < historyNodes.length; i++) expect(after[i]).toBe(historyNodes[i]);
    expect(r.currentFrame!.scrollbackRows.map((entry) => entry.index).slice(-2)).toEqual([760, 761]);
    expect(r.paintPresentation().tailGapPx).toBe(9 * ROW_PX);
    expect(r.gridText()).toBe("delta-2");
  });

  test("a same-epoch streaming repair preserves fetched history through explicit reading and atomic resume", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    seedHeldHistory(r, 80, [row(0, "old-v")], nRows(250, 500), 750);
    const historyNodes = sbRows(sbEl(c)).slice();
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
    expect(r.canonicalFrameSeq()).toBe(4);
    expect(r.canonicalEpochSeq()).toEqual({ grid_epoch: "test-grid:0", seq: 4 });
    expect(r.reconciledEpochSeq()).toEqual(reconciledBefore);
    expect(r.reconcileBlockReason()).toBe("reader_pending_frame");
    expect(c.scrollHeight).toBe(heightBefore);
    expect(c.scrollTopWrites).toBe(0);
    expect(rowAtReader(c)).toBe(readerBefore);
    for (let i = 0; i < historyNodes.length; i++) {
      expect(sbRows(sbEl(c))[i]).toBe(historyNodes[i]);
    }

    expect(r.prepareLiveInteraction()).toEqual({ reconciled: true, anchorChanged: true });
    expect(r.currentFrame!.sbBase).toBe(760);
    expect(r.currentFrame!.scrollbackRows.map((entry) => entry.index)).toEqual([760]);
    expect(r.backfillAnchor()).toMatchObject({ sbBase: 500, total: 761 });
    expect(spansText((r.currentFrame!.viewportRows[0]!).spans)).toBe("latest-v");
    for (let i = 0; i < historyNodes.length; i++) {
      expect(sbRows(sbEl(c))[i]).toBe(historyNodes[i]);
    }
    expect(r.paintPresentation().tailGapPx).toBe(9 * ROW_PX);
    expect(c.scrollTopWrites).toBe(1);
    expect(r.atBottom()).toBe(true);
    expect(r.reconciledEpochSeq()).toEqual({ grid_epoch: "test-grid:0", seq: 4 });
    expect(r.reconcileBlockReason()).toBeNull();
  });


  test("a viewport-only renewal retains equivalent painted rows without a bridge", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    const history = nRows(250, 500);
    seedHeldHistory(r, 80, [row(0, "old-v")], history, 750);
    const existing = sbRows(sbEl(c)).slice();

    expect(r.applyFullFrame({
      ...fullFrame(80, [row(0, "new-v")], 752),
      streamId: "test-stream:2",
      gridEpoch: "test-grid:0",
      seq: 2,
    })).toBe(true);

    expect(r.currentFrame!.scrollbackRows).toEqual([]);
    expect(r.currentFrame!.sbBase).toBe(752);
    for (let index = 0; index < existing.length; index++) {
      expect(sbRows(sbEl(c))[index]).toBe(existing[index]);
    }
  });


  test("an incompatible full keeps the reader window immutable until explicit resume", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    seedHeldHistory(r, 80, [row(0, "old-v")], nRows(250, 500), 750);
    const oldNodes = sbRows(sbEl(c)).slice();
    c.scrollTop = PAD_TOP + 600 * ROW_PX;
    r.handleScroll();
    expect(r.paintPresentation().readerAnchor).toEqual({ row: 600, offsetPx: 0 });
    c.resetScrollTopWrites();

    expect(r.applyFullFrame({
      ...fullFrame(100, [row(0, "new-epoch-v")], 760),
      streamId: "test-stream:1",
      gridEpoch: "test-grid:1",
    })).toBe(true);

    expect(r.backfillAnchor()).toBeNull();
    expect(r.readerAnchorForBackfill()).toBeNull();
    expect(r.currentFrame!.gridEpoch).toBe("test-grid:0");
    expect(r.canonicalEpochSeq()).toEqual({ grid_epoch: "test-grid:1", seq: 1 });
    expect(c.scrollTopWrites).toBe(0);
    for (let index = 0; index < oldNodes.length; index++) {
      expect(sbRows(sbEl(c))[index]).toBe(oldNodes[index]);
    }

    expect(r.prepareLiveInteraction()).toEqual({
      reconciled: true,
      anchorChanged: true,
    });
    expect(r.currentFrame!.gridEpoch).toBe("test-grid:1");
    expect(r.reconciledEpochSeq()).toEqual(r.canonicalEpochSeq());
    for (const node of oldNodes) expect(sbRows(sbEl(c))).not.toContain(node);
    expect(c.scrollTop).toBe(c.scrollHeight - c.clientHeight);
  });

  test("releasing selection reconciles its pending canonical state", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    seedHeldHistory(r, 80, [row(0, "old-v")], nRows(250, 500), 750);
    c.scrollTop = PAD_TOP + 600 * ROW_PX;
    r.setSelectionHold(true);
    r.apply({ ...fullFrame(80, [row(0, "latest-v")], 760), seq: 3 });

    c.resetScrollTopWrites();
    expect(r.setSelectionHold(false)).toEqual({
      reconciled: true,
      anchorChanged: true,
    });
    expect(r.currentFrame!.seq).toBe(3);
    expect(r.readerIntent).toBe("live");
    expect(spansText((r.currentFrame!.viewportRows[0]!).spans)).toBe("latest-v");
    expect(c.scrollTop).toBe(c.scrollHeight - c.clientHeight);
    expect(c.scrollTopWrites).toBe(1);
  });
  test("a compatible viewport-only checkpoint retains rows that just left the viewport", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    const oldViewport = [row(0, "old-0"), row(1, "old-1"), row(2, "old-2")];

    expect(r.applyFullFrame({ ...fullFrame(80, oldViewport, 10), seq: 1 })).toBe(true);
    expect(r.applyFullFrame({
      ...fullFrame(80, [row(0, "new-0"), row(1, "new-1"), row(2, "new-2")], 12),
      seq: 2,
    })).toBe(true);

    expect(r.currentFrame!.scrollbackRows).toEqual([]);
    expect(r.hasPaintedScrollbackRange(10, 12)).toBe(true);
    expect(r.paintPresentation().rows).toEqual([
      { index: 10, text: "old-0" },
      { index: 11, text: "old-1" },
    ]);
    expect(r.missingScrollbackRange(10)).toBeNull();
    expect(r.missingScrollbackRange(11)).toBeNull();
  });

  test("checkpoint tail promotion does not evict demanded head coverage", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    const history = nRows(MAX_HELD_SCROLLBACK_ROWS);

    expect(seedHeldHistory(r, 80, [row(0, "old-v")], history, MAX_HELD_SCROLLBACK_ROWS)).toBe(true);
    expect(r.applyFullFrame({
      ...fullFrame(80, [row(0, "new-v")], MAX_HELD_SCROLLBACK_ROWS + 1),
      seq: 3,
    })).toBe(true);

    expect(r.hasPaintedScrollbackRange(0, MAX_HELD_SCROLLBACK_ROWS + 1)).toBe(true);
    expect(r.paintPresentation(MAX_HELD_SCROLLBACK_ROWS + 1).rows).toHaveLength(
      MAX_HELD_SCROLLBACK_ROWS + 1,
    );
  });

});
