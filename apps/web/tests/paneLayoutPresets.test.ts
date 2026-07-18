// Preset tile layouts (store/paneLayoutPresets.ts).

import { describe, test, expect } from "bun:test";
import { presetLayout, balanceLayout } from "../src/store/paneLayoutPresets.ts";
import { allLeaves, layoutRects, type Layout, type PaneNode, type PaneSplit } from "../src/store/paneLayout.ts";

const ids = (n: number) => Array.from({ length: n }, (_, i) => `s${i}`);

describe("presetLayout", () => {
  test("one session → a single leaf", () => {
    expect(presetLayout("even", ["only"]).root.kind).toBe("leaf");
  });

  test("every kind places every session in its own single-tab pane", () => {
    for (const kind of ["even", "rows", "tiled", "main-vertical"] as const) {
      const leaves = allLeaves(presetLayout(kind, ids(5)).root);
      expect(leaves.length).toBe(5);
      expect(leaves.every((p) => p.tabs.length === 1)).toBe(true);
      expect(new Set(leaves.flatMap((p) => p.tabs))).toEqual(new Set(ids(5)));
    }
  });

  test("even = equal columns (all panes span the full height)", () => {
    const { panes } = layoutRects(presetLayout("even", ids(4)).root, { x: 0, y: 0, w: 800, h: 600 });
    expect(panes.size).toBe(4);
    expect(new Set([...panes.values()].map((r) => Math.round(r.h))).size).toBe(1);
  });

  test("rows = equal rows (all panes span the full width)", () => {
    const { panes } = layoutRects(presetLayout("rows", ids(4)).root, { x: 0, y: 0, w: 800, h: 600 });
    expect(panes.size).toBe(4);
    expect(new Set([...panes.values()].map((r) => Math.round(r.w))).size).toBe(1);
  });

  test("main-vertical: the first pane is the widest", () => {
    const l = presetLayout("main-vertical", ids(4));
    const { panes } = layoutRects(l.root, { x: 0, y: 0, w: 1000, h: 600 });
    const mainId = allLeaves(l.root)[0].paneId;
    const mainW = panes.get(mainId)!.w;
    const others = [...panes.entries()].filter(([id]) => id !== mainId).map(([, r]) => r.w);
    expect(mainW).toBeGreaterThan(Math.max(...others));
  });

  test("focusedPaneId is a real leaf", () => {
    const l = presetLayout("tiled", ids(3));
    expect(allLeaves(l.root).some((p) => p.paneId === l.focusedPaneId)).toBe(true);
  });
});

describe("balanceLayout", () => {
  // Skewed tree: s1 row-splits leaf A against s2; s2 col-splits leaf B/C.
  const leaf = (sid: string): PaneNode => ({ kind: "leaf", paneId: `${sid}-pane`, tabs: [sid], selectedTab: sid });
  const skewed = (): Layout => ({
    root: {
      kind: "split", id: "s1", dir: "row", ratio: 0.8,
      a: leaf("A"),
      b: { kind: "split", id: "s2", dir: "col", ratio: 0.3, a: leaf("B"), b: leaf("C") },
    },
    focusedPaneId: "A-pane",
  });

  test("ratios become leaf-count shares; tree/panes/focus untouched", () => {
    const out = balanceLayout(skewed());
    const s1 = out.root as PaneSplit;
    const s2 = s1.b as PaneSplit;
    expect(s1.ratio).toBeCloseTo(1 / 3, 10); // A (1 leaf) vs s2 (2 leaves)
    expect(s2.ratio).toBeCloseTo(0.5, 10); // B vs C
    expect(s1.id).toBe("s1");
    expect(s2.id).toBe("s2");
    expect(out.focusedPaneId).toBe("A-pane");
    const leaves = allLeaves(out.root);
    expect(leaves.map((p) => p.paneId)).toEqual(["A-pane", "B-pane", "C-pane"]);
    expect(leaves.map((p) => p.tabs.join(","))).toEqual(["A", "B", "C"]);
    expect(leaves.map((p) => p.selectedTab)).toEqual(["A", "B", "C"]);
  });

  test("all pane areas equal within 2% of the mean", () => {
    const { panes } = layoutRects(balanceLayout(skewed()).root, { x: 0, y: 0, w: 1206, h: 806 });
    const areas = [...panes.values()].map((r) => r.w * r.h);
    const mean = areas.reduce((s, a) => s + a, 0) / areas.length;
    for (const a of areas) expect(Math.abs(a - mean) / mean).toBeLessThan(0.02);
  });

  test("single leaf is a structural no-op", () => {
    const one = presetLayout("even", ["only"]);
    expect(balanceLayout(one)).toEqual(one);
  });
});
