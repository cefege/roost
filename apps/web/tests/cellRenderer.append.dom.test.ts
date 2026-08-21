// CellGridRenderer DOM tripwire — the corruption-class guarantee in CI.
//
// Cell mode kills the terminal-history-corruption saga by NEVER re-parsing or
// reflowing: scrollback rows are immutable + append-only, and the painted
// width is pinned to the worker's grid cols (letterbox, no client reflow).
// Otherwise it is only checked by the browser tier's render-stress cases
// (smoke/terminal/terminal-render*.spec.ts) — a /simplify pass turning apply()'s
// delta path back into a full re-render would go green. Locked by NODE IDENTITY:
// existing scrollback DOM nodes must survive every delta.
//
// This file: the append-only scrollback path plus viewport-only full frames and
// the explicit backfill splice. The rest of the suite is cellRenderer.heldWindow
// / .reconcile / .readerIntent / .geometry .dom.test.ts; the shared fake DOM and
// frame builders live in helpers/cellRendererFakeDom.ts.

import { describe, test, expect } from "bun:test";
import {
  CellGridRenderer,
  RENDERER_HOLD_LINK,
  RENDERER_HOLD_SELECTION,
} from "../src/lib/cellRenderer.ts";
import {
  makeContainer,
  row,
  fullFrame,
  deltaFrame,
  seedHeldHistory,
  altFullFrame,
  altDeltaFrame,
  sbEl,
  vpEl,
  sbRows,
} from "./helpers/cellRendererFakeDom.ts";

describe("CellGridRenderer DOM — append-only scrollback, no reflow", () => {
  test("a delta APPENDS scrollback; existing rows keep their identity", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as any);
    const scrollbackEl = sbEl(c); // ctor appends scrollback then viewport

    seedHeldHistory(r, 80, [row(0, "v0"), row(1, "v1")], [row(0, "h0"), row(1, "h1")]);
    const rows0 = sbRows(scrollbackEl);
    expect(rows0.length).toBe(2);
    const h0 = rows0[0];
    const h1 = rows0[1];

    r.apply(deltaFrame(80, 2, [row(1, "v1b")], [row(2, "h2")], 3));
    // Append-only: the two original nodes are the SAME objects (not re-rendered),
    // the third is new. A full re-render would replace all three.
    const rows1 = sbRows(scrollbackEl);
    expect(rows1.length).toBe(3);
    expect(rows1[0]).toBe(h0);
    expect(rows1[1]).toBe(h1);
    expect(rows1[2]).not.toBe(h1);
  });

  test("canonicalFrameSeq tracks each exactly sequenced accepted frame", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement); // FakeEl covers the renderer's DOM surface
    expect(r.canonicalFrameSeq()).toBe(0); // nothing held → the worker must snapshot

    seedHeldHistory(r, 80, [row(0, "v0")], []);
    expect(r.canonicalFrameSeq()).toBe(1); // fullFrame() carries seq 1

    r.apply(deltaFrame(80, 1, [row(0, "v0b")], [row(1, "h1")], 2));
    expect(r.canonicalFrameSeq()).toBe(2);

    r.apply(deltaFrame(80, 1, [row(0, "v0c")], [], 3));
    expect(r.canonicalFrameSeq()).toBe(3);
  });

  test("a delta from a different grid epoch is rejected", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    seedHeldHistory(r, 80, [row(0, "held")], []);
    const stale = {
      ...deltaFrame(80, 1, [row(0, "wrong")], [], 2),
      gridEpoch: "test-grid:1",
    };
    expect(r.apply(stale)).toBe(false);
    expect(r.currentFrame?.seq).toBe(1);
    expect(r.gridText()).toBe("held");
  });

  test("a viewport-only delta does NOT touch scrollback DOM at all", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as any);
    const scrollbackEl = sbEl(c);

    seedHeldHistory(r, 80, [row(0, "v0")], [row(0, "h0"), row(1, "h1")]);
    const before = sbRows(scrollbackEl);

    r.apply(deltaFrame(80, 1, [row(0, "v0-changed")], [], 2));
    const after = sbRows(scrollbackEl);
    expect(after.length).toBe(2);
    expect(after[0]).toBe(before[0]);
    expect(after[1]).toBe(before[1]);
  });

  test("painted width is pinned to the worker's frame.cols (letterbox, no reflow)", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as any);

    seedHeldHistory(r, 80, [row(0, "v0")], []);
    expect((c.style as any)["--cell-cols"]).toBe("80");
    // The renderer has no container-width input — a delta can only carry the
    // worker's cols, never the pane width. Cols stays the worker's value.
    r.apply(deltaFrame(80, 1, [row(0, "x")], [], 2));
    expect((c.style as any)["--cell-cols"]).toBe("80");
  });

  test("selection enters reading; release stays frozen until admitted input resumes live", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    const viewportEl = vpEl(c);
    const scrollbackEl = sbEl(c);

    expect(seedHeldHistory(r, 80, [row(0, "v0")], [row(0, "h0")])).toBe(true);
    const beforeHold = r.reconciledEpochSeq();
    const heldRow = viewportEl.children[0];

    expect(r.setSelectionHold(true)).toEqual({ reconciled: false, anchorChanged: false });
    expect(r.readerIntent).toBe("reading");
    expect(r.holdMask).toBe(RENDERER_HOLD_SELECTION);
    expect(r.apply({
      ...deltaFrame(80, 1, [row(0, "v0-changed")], [row(1, "h1")], 3),
      scrollbackTotal: 2,
    })).toBe(true);
    expect(r.apply({
      ...deltaFrame(80, 1, [row(0, "v0-again")], [], 4),
      scrollbackTotal: 2,
    })).toBe(true);
    expect(viewportEl.children[0]).toBe(heldRow);
    expect(sbRows(scrollbackEl).length).toBe(1);
    expect(r.canonicalEpochSeq()).toEqual({ grid_epoch: "test-grid:0", seq: 4 });
    expect(r.reconciledEpochSeq()).toEqual(beforeHold);
    expect(r.reconcileBlockReason()).toBe("reader_pending_frame");
    expect(r.presentationSnapshot().hold_mask).toEqual({ selection: true, link: false });
    expect(r.gridText()).toBe("v0-again");

    expect(r.setSelectionHold(false)).toEqual({ reconciled: false, anchorChanged: false });
    expect(viewportEl.children[0]).toBe(heldRow);
    expect(r.prepareLiveInteraction()).toEqual({ reconciled: true, anchorChanged: true });
    expect(viewportEl.children[0]).not.toBe(heldRow);
    expect(sbRows(scrollbackEl).length).toBe(2);
    expect(r.reconciledEpochSeq()).toEqual({ grid_epoch: "test-grid:0", seq: 4 });
    expect(r.readerIntent).toBe("live");
    expect(r.reconcileBlockReason()).toBeNull();
  });

  test("armed link hold freezes viewport and flushes on release without entering reading", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    const viewportEl = vpEl(c);

    seedHeldHistory(r, 80, [row(0, "v0")], []);
    const heldRow = viewportEl.children[0];

    r.setArmedHold(true);
    expect(r.readerIntent).toBe("live");
    expect(r.holdMask).toBe(RENDERER_HOLD_LINK);
    r.apply(deltaFrame(80, 1, [row(0, "v0-changed")], [], 2));
    expect(viewportEl.children[0]).toBe(heldRow);
    expect(r.canonicalEpochSeq()).toEqual({ grid_epoch: "test-grid:0", seq: 2 });
    expect(r.reconciledEpochSeq()).toEqual({ grid_epoch: "test-grid:0", seq: 1 });
    expect(r.reconcileBlockReason()).toBe("link_hold");

    expect(r.setArmedHold(false)).toEqual({ reconciled: true, anchorChanged: false });
    expect(viewportEl.children[0]).not.toBe(heldRow);
    expect(r.reconciledEpochSeq()).toEqual({ grid_epoch: "test-grid:0", seq: 2 });
    expect(r.reconcileBlockReason()).toBeNull();
  });

  test("selection and link holds clear atomically with at most one epoch repair", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    const viewportEl = vpEl(c);

    seedHeldHistory(r, 80, [row(0, "v0")], []);
    r.setSelectionHold(true);
    r.setArmedHold(true);
    expect(r.holdMask).toBe(RENDERER_HOLD_SELECTION | RENDERER_HOLD_LINK);
    expect(r.presentationSnapshot().hold_mask).toEqual({ selection: true, link: true });
    expect(r.apply({
      ...altFullFrame(80, [row(0, "TUI")], []),
      seq: 2,
    })).toBe(true);

    const originalReplace = viewportEl.replaceChildren.bind(viewportEl);
    let repairWrites = 0;
    viewportEl.replaceChildren = (...children: unknown[]) => {
      repairWrites++;
      originalReplace(...children);
    };
    expect(r.prepareLiveInteraction()).toEqual({ reconciled: true, anchorChanged: true });
    expect(repairWrites).toBe(1);
    expect(r.holdMask).toBe(0);
    expect(r.readerIntent).toBe("live");
    expect(c.classList.contains("alt-active")).toBe(true);
    expect(r.reconcileBlockReason()).toBeNull();
  });

  test("a delta before any full frame is rejected and the next full is accepted", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    const scrollbackEl = sbEl(c);

    expect(r.apply(deltaFrame(80, 1, [row(0, "x")], [row(0, "orphan")], 1))).toBe(false);
    expect(r.canonicalFrameSeq()).toBe(0);
    expect(scrollbackEl.children.length).toBe(0);

    expect(seedHeldHistory(r, 80, [row(0, "v0")], [row(0, "h0")])).toBe(true);
    expect(scrollbackEl.children.length).toBe(1);
  });

  test("alt-screen frame sets .alt-active; leaving alt clears it", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as any);

    // Main-screen full frame → no alt-active (scrollback stays visible/scrollable).
    seedHeldHistory(r, 80, [row(0, "v0")], [row(0, "h0")]);
    expect(c.classList.contains("alt-active")).toBe(false);

    // A fullscreen terminal app enters altScreen:true → alt-active latches (CSS hides
    // scrollback + locks scroll: no historic junk on top, no scroll-up).
    r.apply(altFullFrame(80, [row(0, "TUI")], [row(0, "stale-h0")]));
    expect(r.presentationSnapshot().mode).toEqual({
      canonical: {
        alt_screen: true,
        cursor_keys_app: false,
        bracketed_paste: false,
      },
      reconciled: {
        alt_screen: true,
        cursor_keys_app: false,
        bracketed_paste: false,
      },
    });
    expect(c.classList.contains("alt-active")).toBe(true);

    // A delta while still in alt keeps it on.
    r.apply(altDeltaFrame(80, 1, [row(0, "TUI2")], 3));
    expect(c.classList.contains("alt-active")).toBe(true);

    // Leaving alt (main-screen frame) clears it → scrollback returns.
    seedHeldHistory(r, 80, [row(0, "back")], [row(0, "h0")]);
    expect(c.classList.contains("alt-active")).toBe(false);
  });
});

// ── viewport-only full frames + explicit backfill splice ──────────────────
describe("CellGridRenderer DOM — viewport-only frames + backfill", () => {

  test("viewport-only full reserves depth; explicit pages fill the seam", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    const scrollbackEl = sbEl(c);
    r.apply({
      ...fullFrame(80, [row(0, "v")]),
      scrollbackTotal: 4,
      sbBase: 4,
    });
    expect(sbRows(scrollbackEl)).toHaveLength(0);
    r.prependScrollback([row(2, "h2"), row(3, "h3")]);
    const a0 = r.backfillAnchor()!;
    expect(a0.sbBase).toBe(2);
    expect(a0.gridEpoch).toBe("test-grid:0");
    const newestPage = sbRows(scrollbackEl);
    r.prependScrollback([row(0, "h0"), row(1, "h1")]);
    const all = sbRows(scrollbackEl);
    expect(all.map((n) => n.children[0].textContent)).toEqual(["h0", "h1", "h2", "h3"]);
    expect(all[2]).toBe(newestPage[0]);
    expect(all[3]).toBe(newestPage[1]);
    expect(r.backfillAnchor()!.sbBase).toBe(0);
    r.prependScrollback([row(0, "stale")]);
    expect(sbRows(scrollbackEl)).toHaveLength(4);
  });

  test("a delta after explicit backfill keeps appending at the same epoch", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    const scrollbackEl = sbEl(c);
    r.apply({
      ...fullFrame(80, [row(0, "v")]),
      scrollbackTotal: 2,
      sbBase: 2,
    });
    r.prependScrollback([row(1, "h1")]);
    r.apply({ ...deltaFrame(80, 1, [], [row(2, "h2")], 2), scrollbackTotal: 3 });
    expect(sbRows(scrollbackEl)).toHaveLength(2);
    expect(r.backfillAnchor()!.sbBase).toBe(1);
    r.prependScrollback([row(0, "h0")]);
    expect(sbRows(scrollbackEl).map((n) => n.children[0].textContent)).toEqual(["h0", "h1", "h2"]);
  });
});
