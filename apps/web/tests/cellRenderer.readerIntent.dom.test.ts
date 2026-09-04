// CellGridRenderer DOM tripwire — explicit reader intent vs the live tail.
//
// Geometry is an effect, not intent. Live panes follow every accepted frame;
// only explicit native/find/selection actions freeze canonical state from DOM.

import { describe, test, expect } from "bun:test";
import { CellGridRenderer } from "../src/lib/cellRenderer.ts";
import { spansText, type CellGridFrame, type CellRow } from "@roost/shared/cell";
import {
  PAD_TOP,
  ROW_PX,
  makeContainer,
  row,
  deltaFrame,
  seedHeldHistory,
  altFullFrame,
  altDeltaFrame,
  sbEl,
  vpEl,
  sbRows,
} from "./helpers/cellRendererFakeDom.ts";

// ── explicit reader intent + persistent live tail ─────────────────────────
// Geometry is an effect, not intent. Live panes follow every accepted frame;
// only explicit native/find/selection actions freeze canonical state from DOM.
describe("CellGridRenderer DOM — reader intent and live tail", () => {
  const nRows = (n: number, from = 0) =>
    Array.from({ length: n }, (_, i) => row(from + i, `s${from + i}`));
  const appDelta = (append: CellRow[], total: number, seq: number): CellGridFrame =>
    ({ ...deltaFrame(80, 1, [row(0, "v")], append, seq), scrollbackTotal: total });

  test("live output follows the tail and its owned scroll event stays live", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    seedHeldHistory(r, 80, [row(0, "v")], nRows(400));
    c.scrollTop = Math.max(0, c.scrollHeight - c.clientHeight);
    c.resetScrollTopWrites();

    r.apply(appDelta([row(400, "new")], 401, 3));

    expect(c.scrollTop).toBe(c.scrollHeight - c.clientHeight);
    expect(c.scrollTopWrites).toBe(1);
    expect(r.readerIntent).toBe("live");
    expect(r.handleScroll()).toEqual({ reconciled: false, anchorChanged: false });
    expect(r.readerIntent).toBe("live");
  });

  test("a coalesced pin retargets once, then the next native scroll reads", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    seedHeldHistory(r, 80, [row(0, "v")], nRows(400));
    const bottom = Math.max(0, c.scrollHeight - c.clientHeight);
    c.scrollTop = bottom;
    r.handleScroll(); // observe the seed render before starting this epoch

    r.setSelectionHold(true);
    c.scrollTop = bottom - 1;
    expect(r.apply({ ...appDelta([], 400, 3), cursorCol: 1 })).toBe(true);
    c.resetScrollTopWrites();
    expect(r.prepareLiveInteraction()).toEqual({ reconciled: true, anchorChanged: false });
    expect(c.scrollTopWrites).toBe(1);

    // Before the event is delivered, layout clamps the scroll position and a
    // second live pin is already unchanged at that final coalesced value.
    c.clientHeight += ROW_PX;
    const finalOwnedTop = c.scrollHeight - c.clientHeight;
    c.scrollTop = finalOwnedTop;
    c.resetScrollTopWrites();
    expect(r.prepareLiveInteraction()).toEqual({ reconciled: false, anchorChanged: false });
    expect(c.scrollTopWrites).toBe(0);
    expect(r.canonicalEpochSeq()).toEqual(r.reconciledEpochSeq());
    expect(r.readerIntent).toBe("live");

    c.clientHeight -= 1; // the one delivered event now observes off-bottom geometry
    expect(r.atBottom()).toBe(false);
    expect(r.handleScroll()).toEqual({ reconciled: false, anchorChanged: false });
    expect(r.readerIntent).toBe("live");

    c.scrollTop = finalOwnedTop - ROW_PX;
    expect(r.handleScroll()).toEqual({ reconciled: false, anchorChanged: false });
    expect(r.readerIntent).toBe("reading");
    expect(r.readerReason).toBe("native_scroll");
  });

  test("selection release re-pins before its owned event, then the next wheel reads", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    seedHeldHistory(r, 80, [row(0, "v")], nRows(400));
    const bottom = Math.max(0, c.scrollHeight - c.clientHeight);
    c.scrollTop = bottom;
    r.handleScroll(); // consume the seed render's owned event

    // Actual Chromium order for an admitted key with a retained native range:
    // prepare/reconcile → Selection.removeAllRanges → scrollTop=0 scroll →
    // selectionchange → live re-pin scroll.
    r.setSelectionHold(true);
    expect(r.apply({ ...appDelta([], 400, 3), cursorCol: 1 })).toBe(true);
    expect(r.prepareLiveInteraction()).toEqual({ reconciled: true, anchorChanged: false });
    r.beginLiveSelectionRelease();
    c.scrollTop = 0;
    c.resetScrollTopWrites();

    expect(r.handleScroll()).toEqual({ reconciled: false, anchorChanged: false });
    expect(c.scrollTop).toBe(bottom);
    expect(c.scrollTopWrites).toBe(1);
    expect(r.readerIntent).toBe("live");
    expect(r.readerReason).toBeNull();
    expect(r.holdMask).toBe(0);
    expect(r.canonicalEpochSeq()).toEqual(r.reconciledEpochSeq());

    r.finishLiveSelectionRelease(); // asynchronous selectionchange
    c.resetScrollTopWrites();
    expect(r.handleScroll()).toEqual({ reconciled: false, anchorChanged: false });
    expect(c.scrollTopWrites).toBe(0);
    expect(r.readerIntent).toBe("live");

    // The lifecycle bracket and its repin ownership are both gone. The very
    // next genuine gesture must be visible and keep its explicit reason.
    r.enterReading("wheel");
    c.scrollTop = bottom - ROW_PX;
    expect(r.handleScroll()).toEqual({ reconciled: false, anchorChanged: false });
    expect(r.readerIntent).toBe("reading");
    expect(r.readerReason).toBe("wheel");
  });

  test("unchanged and fully clamped pins leave no stale scroll ownership", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    seedHeldHistory(r, 80, [row(0, "v")], nRows(400));
    const bottom = Math.max(0, c.scrollHeight - c.clientHeight);
    c.scrollTop = bottom;
    r.handleScroll();
    c.scrollTop = bottom - ROW_PX;
    r.handleScroll(); // mismatching native position ends the seed write's epoch

    const clampedTop = c.scrollTop;
    c.nextScrollTopWriteResult = clampedTop;
    expect(r.prepareLiveInteraction()).toEqual({ reconciled: false, anchorChanged: false });
    expect(r.readerIntent).toBe("live");
    expect(r.atBottom()).toBe(false);
    r.handleScroll();
    expect(r.readerIntent).toBe("reading");
    expect(r.readerReason).toBe("native_scroll");

    c.scrollTop = bottom;
    r.handleScroll();
    expect(r.readerIntent).toBe("live");
    c.clientHeight -= 1;
    expect(r.atBottom()).toBe(false);
    r.handleScroll();
    expect(r.readerIntent).toBe("reading");
    expect(r.readerReason).toBe("native_scroll");
  });

  test("transient off-bottom geometry does not freeze a live frame", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    seedHeldHistory(r, 80, [row(0, "v")], nRows(400));
    c.scrollTop = c.scrollHeight - c.clientHeight - 1;
    c.resetScrollTopWrites();

    r.apply(appDelta([row(400, "new")], 401, 3));

    expect(c.scrollTop).toBe(c.scrollHeight - c.clientHeight);
    expect(c.scrollTopWrites).toBe(1);
    expect(r.readerIntent).toBe("live");
  });

  test("an incompatible full stays off-DOM during native reading and reconciles on resume", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    const viewportEl = vpEl(c);
    seedHeldHistory(r, 80, [row(0, "old-v")], nRows(400));
    c.scrollTop = PAD_TOP + 50 * ROW_PX;
    expect(r.handleScroll()).toEqual({ reconciled: false, anchorChanged: false });
    const heldRow = viewportEl.children[0];
    c.resetScrollTopWrites();

    expect(r.apply({
      ...altFullFrame(80, [row(0, "TUI")], []),
      cursorKeysApp: true,
      bracketedPaste: true,
      scrollbackTotal: 410,
      sbBase: 410,
      seq: 3,
    })).toBe(true);
    expect(r.apply({
      ...altDeltaFrame(80, 1, [row(0, "latest")], 4),
      cursorKeysApp: true,
      bracketedPaste: true,
      scrollbackAppend: [row(410, "live-410")],
      scrollbackTotal: 411,
    })).toBe(true);

    expect(r.readerIntent).toBe("reading");
    expect(r.readerReason).toBe("native_scroll");
    expect(r.currentFrame!.gridEpoch).toBe("test-grid:0");
    expect(r.canonicalEpochSeq()).toEqual({ grid_epoch: "test-grid:1", seq: 4 });
    expect(viewportEl.children[0]).toBe(heldRow);
    expect(c.scrollTop).toBe(PAD_TOP + 50 * ROW_PX);
    expect(c.scrollTopWrites).toBe(0);
    expect(r.backfillAnchor()).toBeNull();
    expect(r.presentationSnapshot().mode).toEqual({
      canonical: {
        alt_screen: true,
        cursor_keys_app: true,
        bracketed_paste: true,
      },
      reconciled: {
        alt_screen: false,
        cursor_keys_app: false,
        bracketed_paste: false,
      },
    });

    expect(r.prepareLiveInteraction()).toEqual({
      reconciled: true,
      anchorChanged: true,
    });
    expect(viewportEl.children[0]).not.toBe(heldRow);
    expect(r.currentFrame!.gridEpoch).toBe("test-grid:1");
    expect(r.currentFrame!.seq).toBe(4);
    expect(r.readerIntent).toBe("live");
    expect(r.reconciledEpochSeq()).toEqual(r.canonicalEpochSeq());
    expect(r.presentationSnapshot().mode.reconciled).toEqual({
      alt_screen: true,
      cursor_keys_app: true,
      bracketed_paste: true,
    });
  });

  test("native selection keeps an incompatible full off-DOM until selection release", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    const viewportEl = vpEl(c);
    seedHeldHistory(r, 80, [row(0, "selected-old")], nRows(400));
    const heldRow = viewportEl.children[0];

    expect(r.setSelectionHold(true)).toEqual({
      reconciled: false,
      anchorChanged: false,
    });
    expect(r.apply({
      ...altFullFrame(100, [row(0, "selected-canonical")], []),
      seq: 3,
      scrollbackTotal: 410,
      sbBase: 410,
    })).toBe(true);
    expect(r.apply({
      ...altDeltaFrame(100, 1, [row(0, "selected-latest")], 4),
      scrollbackAppend: [row(410, "live-410")],
      scrollbackTotal: 411,
    })).toBe(true);

    expect(r.readerReason).toBe("selection");
    expect(r.canonicalEpochSeq()).toEqual({ grid_epoch: "test-grid:1", seq: 4 });
    expect(r.gridText()).toBe("selected-latest");
    expect(spansText(r.currentFrame!.viewportRows[0]!.spans)).toBe("selected-old");
    expect(viewportEl.children[0]).toBe(heldRow);

    expect(r.setSelectionHold(false)).toEqual({
      reconciled: true,
      anchorChanged: true,
    });
    expect(r.readerIntent).toBe("live");
    expect(r.currentFrame!.seq).toBe(4);
    expect(spansText(r.currentFrame!.viewportRows[0]!.spans)).toBe("selected-latest");
    expect(viewportEl.children[0]).not.toBe(heldRow);
    expect(r.reconciledEpochSeq()).toEqual(r.canonicalEpochSeq());
  });

  test("selection release preserves an independent wheel reader interval", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    const viewportEl = vpEl(c);
    seedHeldHistory(r, 80, [row(0, "wheel-old")], nRows(400));
    const heldRow = viewportEl.children[0];
    r.enterReading("wheel");
    r.setSelectionHold(true);
    expect(r.readerReason).toBe("wheel");

    expect(r.apply({
      ...altFullFrame(80, [row(0, "wheel-new")], []),
      seq: 3,
    })).toBe(true);
    expect(r.setSelectionHold(false)).toEqual({
      reconciled: false,
      anchorChanged: false,
    });
    expect(r.readerReason).toBe("wheel");
    expect(viewportEl.children[0]).toBe(heldRow);

    expect(r.prepareLiveInteraction()).toEqual({
      reconciled: true,
      anchorChanged: true,
    });
    expect(viewportEl.children[0]).not.toBe(heldRow);
  });

  test("a genuine return to literal bottom reconciles pending canonical state", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    seedHeldHistory(r, 80, [row(0, "old-v")], nRows(400));
    c.scrollTop = PAD_TOP + 50 * ROW_PX;
    r.handleScroll();
    r.apply({
      ...appDelta([row(400, "new")], 401, 3),
      viewportRows: [row(0, "latest-v")],
    });
    expect(r.currentFrame!.seq).toBe(2);

    c.scrollTop = c.scrollHeight - c.clientHeight;
    c.resetScrollTopWrites();
    expect(r.handleScroll()).toEqual({ reconciled: true, anchorChanged: true });
    expect(r.currentFrame!.seq).toBe(3);
    expect(spansText((r.currentFrame!.viewportRows[0]!).spans)).toBe("latest-v");
    expect(r.readerIntent).toBe("live");
    expect(c.scrollTop).toBe(c.scrollHeight - c.clientHeight);
    expect(c.scrollTopWrites).toBe(1);
  });

  test("find and native reader actions stay frozen until explicit live preparation", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    seedHeldHistory(r, 80, [row(0, "v")], nRows(400));

    r.scrollToScrollbackRow(50);
    expect(r.readerIntent).toBe("reading");
    expect(r.readerReason).toBe("find");
    r.handleScroll(); // consumes the find-owned write
    expect(r.readerReason).toBe("find");
    r.apply(appDelta([row(400, "new")], 401, 3));
    expect(r.prepareLiveInteraction()).toEqual({ reconciled: true, anchorChanged: true });
    expect(r.readerIntent).toBe("live");

    c.scrollTop = c.scrollHeight - c.clientHeight - ROW_PX;
    r.handleScroll();
    expect(r.readerReason).toBe("native_scroll");
    expect(r.prepareLiveInteraction()).toEqual({ reconciled: false, anchorChanged: false });
    expect(r.readerIntent).toBe("live");
    expect(r.atBottom()).toBe(true);
  });

  test("a non-bottom backfill prepend performs no application scroll write", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    const total = 1000, held = 300;
    seedHeldHistory(r, 80, [row(0, "v")], nRows(held, total - held), total);
    c.scrollTop = PAD_TOP + 50 * ROW_PX;
    const before = c.scrollTop;
    c.resetScrollTopWrites();

    r.prependScrollback(nRows(100, total - held - 100));

    expect(c.scrollTop).toBe(before);
    expect(c.scrollTopWrites).toBe(0);
    expect(r.backfillAnchor()!.sbBase).toBe(total - held - 100);
    expect(sbRows(sbEl(c)).length).toBe(held + 100);
    expect(r.currentFrame!.scrollbackRows.length).toBe(held + 100);
  });

  test("live intent persists through box changes before ResizeObserver runs", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    seedHeldHistory(r, 80, [row(0, "v")], nRows(400));
    c.scrollTop = Math.max(0, c.scrollHeight - c.clientHeight);
    c.resetScrollTopWrites();
    c.clientHeight = 400;

    for (let k = 0; k < 3; k++) {
      r.apply(appDelta(nRows(50, 400 + 50 * k), 450 + 50 * k, 3 + k));
    }

    expect(c.scrollTop).toBe(c.scrollHeight - c.clientHeight);
    expect(r.atBottom()).toBe(true);
    expect(r.readerIntent).toBe("live");
    expect(c.scrollTopWrites).toBe(3);
  });
});
