// CellGridRenderer DOM tripwire — viewport reconciliation restraint.
//
// A delta inspects only rows the worker marked dirty. Reconstructing or
// re-hashing the full viewport changes untouched node identity and loses the
// row-diff engine's sparse-update guarantee.

import { describe, test, expect } from "bun:test";
import { CellGridRenderer } from "../src/lib/cellRenderer.ts";
import {
  FakeEl,
  makeContainer,
  row,
  fullFrame,
  deltaFrame,
  seedHeldHistory,
  vpEl,
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
