// Unit coverage for the tiling tree (store/paneLayout.ts). Drives the pure ops
// + reconcile + ratio→pixel geometry. The invariant tests (a session lives in
// exactly one leaf) guard the SCD-self-clamp rule from the plan.

import { describe, test, expect } from "bun:test";
import { LAYOUT_RATIO_MAX, LAYOUT_RATIO_MIN } from "@roost/shared/layout-document";
import {
  defaultLayout,
  splitLeaf,
  moveTab,
  reorderTab,
  selectTab,
  closeTab,
  setRatio,
  compactLeafForLayout,
  reconcile,
  allLeaves,
  findLeaf,
  findLeafOfTab,
  flatTabs,
  layoutRects,
  layoutView,
  DIVIDER_PX,
  type Layout,
  type PaneSplit,
} from "../src/store/paneLayout.ts";

const tabsOf = (l: Layout) => allLeaves(l.root).map((p) => p.tabs);
const allTabs = (l: Layout) => allLeaves(l.root).flatMap((p) => p.tabs);
const rootPane = (l: Layout) => allLeaves(l.root)[0].paneId;

/** No SessionId appears in two leaves. */
function assertNoDupes(l: Layout) {
  const seen = allTabs(l);
  expect(new Set(seen).size).toBe(seen.length);
}

describe("defaultLayout", () => {
  test("single pane holds every session, first selected", () => {
    const l = defaultLayout(["s1", "s2", "s3"]);
    expect(l.root.kind).toBe("leaf");
    expect(tabsOf(l)).toEqual([["s1", "s2", "s3"]]);
    expect(allLeaves(l.root)[0].selectedTab).toBe("s1");
    expect(l.focusedPaneId).toBe(rootPane(l));
  });
  test("empty session set → empty leaf", () => {
    const l = defaultLayout([]);
    expect(allLeaves(l.root)[0].selectedTab).toBe("");
  });
});

describe("splitLeaf", () => {
  test("moves an existing tab into a new pane; focus follows; ratio 0.5", () => {
    const base = defaultLayout(["s1", "s2"]);
    const l = splitLeaf(base, rootPane(base), "row", "s2", false);
    expect(l.root.kind).toBe("split");
    const split = l.root as PaneSplit;
    expect(split.dir).toBe("row");
    expect(split.ratio).toBe(0.5);
    // s2 left in its own pane, s1 kept behind
    expect(tabsOf(l).sort()).toEqual([["s1"], ["s2"]].sort());
    expect(l.focusedPaneId).toBe(findLeafOfTab(l.root, "s2")!.paneId);
    assertNoDupes(l);
  });
  test("insertFirst places the new pane on the a-side", () => {
    const base = defaultLayout(["s1", "s2"]);
    const l = splitLeaf(base, rootPane(base), "row", "s2", true);
    const split = l.root as PaneSplit;
    expect((split.a as any).tabs).toEqual(["s2"]);
    expect((split.b as any).tabs).toEqual(["s1"]);
  });
  test("fresh session (not yet in tree) splits without disturbing existing tabs", () => {
    const base = defaultLayout(["s1", "s2"]);
    const l = splitLeaf(base, rootPane(base), "col", "sNEW", false);
    // s1+s2 stay together; sNEW gets its own pane
    expect(tabsOf(l).sort()).toEqual([["s1", "s2"], ["sNEW"]].sort());
    assertNoDupes(l);
  });
  test("splitting a single-tab pane by its own tab is a no-op", () => {
    const base = defaultLayout(["only"]);
    const l = splitLeaf(base, rootPane(base), "row", "only", false);
    expect(l).toBe(base);
  });
});

describe("moveTab / reorderTab / selectTab", () => {
  test("cross-pane move removes from source, selects in target, collapses emptied source", () => {
    let l = defaultLayout(["s1", "s2", "s3"]);
    l = splitLeaf(l, rootPane(l), "row", "s3", false); // [s1,s2 | s3]
    const leftPane = findLeafOfTab(l.root, "s1")!.paneId;
    const rightPane = findLeafOfTab(l.root, "s3")!.paneId;
    l = moveTab(l, "s3", leftPane); // move s3 back → right pane empties → collapses
    expect(l.root.kind).toBe("leaf");
    expect(allTabs(l).sort()).toEqual(["s1", "s2", "s3"]);
    expect(findLeaf(l.root, rightPane)).toBeNull();
    expect(findLeafOfTab(l.root, "s3")!.selectedTab).toBe("s3");
    assertNoDupes(l);
  });
  test("same-pane move reorders", () => {
    let l = defaultLayout(["a", "b", "c"]);
    l = moveTab(l, "c", rootPane(l), 0);
    expect(tabsOf(l)).toEqual([["c", "a", "b"]]);
    assertNoDupes(l);
  });
  test("reorderTab sets explicit order", () => {
    let l = defaultLayout(["a", "b", "c"]);
    l = reorderTab(l, rootPane(l), ["b", "c", "a"]);
    expect(tabsOf(l)).toEqual([["b", "c", "a"]]);
  });
  test("selectTab selects and focuses the owning pane", () => {
    let l = defaultLayout(["a", "b"]);
    l = splitLeaf(l, rootPane(l), "row", "b", false);
    l = selectTab(l, "a");
    expect(l.focusedPaneId).toBe(findLeafOfTab(l.root, "a")!.paneId);
    expect(findLeafOfTab(l.root, "a")!.selectedTab).toBe("a");
  });
});

describe("closeTab", () => {
  test("closing the last tab of a pane collapses it and promotes the sibling", () => {
    let l = defaultLayout(["s1", "s2"]);
    l = splitLeaf(l, rootPane(l), "row", "s2", false); // [s1 | s2]
    l = closeTab(l, "s2");
    expect(l.root.kind).toBe("leaf");
    expect(allTabs(l)).toEqual(["s1"]);
    expect(l.focusedPaneId).toBe(findLeafOfTab(l.root, "s1")!.paneId);
  });
  test("closing a non-selected tab keeps the pane", () => {
    let l = defaultLayout(["a", "b", "c"]);
    l = closeTab(l, "b");
    expect(tabsOf(l)).toEqual([["a", "c"]]);
  });
  // The "close terminal closes EVERYTHING" report: closing one tab must leave
  // every OTHER pane's tabs + selection untouched (only the target leaf shrinks).
  test("closing one tab in a multi-pane layout leaves sibling panes intact", () => {
    let l = defaultLayout(["s1", "s2", "s3"]);
    l = splitLeaf(l, rootPane(l), "row", "s2", false); // [s1 | s2]
    const rightPane = findLeafOfTab(l.root, "s2")!.paneId;
    l = splitLeaf(l, rightPane, "col", "s3", false); // [s1 | [s2 / s3]]
    const s1Pane = findLeafOfTab(l.root, "s1")!.paneId;
    const s3Pane = findLeafOfTab(l.root, "s3")!.paneId;
    l = closeTab(l, "s2"); // kill only s2's leaf
    expect(allTabs(l).sort()).toEqual(["s1", "s3"]);
    // s1 and s3 panes survive with their exact tabs + selection
    expect(findLeaf(l.root, s1Pane)!.tabs).toEqual(["s1"]);
    expect(findLeaf(l.root, s3Pane)!.tabs).toEqual(["s3"]);
    expect(findLeaf(l.root, s3Pane)!.selectedTab).toBe("s3");
    assertNoDupes(l);
  });
});

describe("reconcile", () => {
  test("prunes dead tabs, collapses emptied panes, appends orphans to focused pane", () => {
    let l = defaultLayout(["s1", "s2"]);
    l = splitLeaf(l, rootPane(l), "row", "s2", false); // [s1 | s2], focus s2's pane
    // s2 died, s9 is new
    l = reconcile(l, ["s1", "s9"]);
    // s2's pane collapsed → single leaf; s9 appended to the (now-fixed) focus pane
    expect(allTabs(l).sort()).toEqual(["s1", "s9"]);
    assertNoDupes(l);
    // focus fell back to a real pane
    expect(findLeaf(l.root, l.focusedPaneId)).not.toBeNull();
  });
  test("no live sessions → empty leaf, no crash", () => {
    let l = defaultLayout(["s1"]);
    l = reconcile(l, []);
    expect(allTabs(l)).toEqual([]);
  });

  test("preserves intentional empty branches across repeated reconciliation", () => {
    const layout: Layout = {
      root: {
        kind: "split",
        id: "intentional-split",
        dir: "row",
        ratio: 0.5,
        a: { kind: "leaf", paneId: "empty-pane", tabs: [], selectedTab: "" },
        b: { kind: "leaf", paneId: "live-pane", tabs: ["s1"], selectedTab: "s1" },
      },
      focusedPaneId: "empty-pane",
    };
    const first = reconcile(layout, ["s1"]);
    const second = reconcile(first, ["s1"]);
    expect(first.root.kind).toBe("split");
    expect(second.root.kind).toBe("split");
    expect(tabsOf(second)).toEqual([[], ["s1"]]);
    expect(second.focusedPaneId).toBe("empty-pane");
  });

  test("mutations collapse only the pane they empty, not intentional siblings", () => {
    const layout: Layout = {
      root: {
        kind: "split",
        id: "outer",
        dir: "row",
        ratio: 0.5,
        a: { kind: "leaf", paneId: "empty-pane", tabs: [], selectedTab: "" },
        b: {
          kind: "split",
          id: "occupied",
          dir: "col",
          ratio: 0.5,
          a: { kind: "leaf", paneId: "b-pane", tabs: ["s1"], selectedTab: "s1" },
          b: { kind: "leaf", paneId: "c-pane", tabs: ["s2"], selectedTab: "s2" },
        },
      },
      focusedPaneId: "b-pane",
    };
    const closed = closeTab(layout, "s2");
    expect(closed.root.kind).toBe("split");
    expect(findLeaf(closed.root, "empty-pane")).not.toBeNull();
    expect(tabsOf(closed)).toEqual([[], ["s1"]]);

    const moved = moveTab(layout, "s2", "b-pane");
    expect(moved.root.kind).toBe("split");
    expect(findLeaf(moved.root, "empty-pane")).not.toBeNull();
    expect(tabsOf(moved)).toEqual([[], ["s1", "s2"]]);

    const split = splitLeaf(layout, "b-pane", "row", "s2", false);
    expect(findLeaf(split.root, "empty-pane")).not.toBeNull();
    expect(findLeaf(split.root, "c-pane")).toBeNull();
  });

  test("normalizes every legacy split ratio into portable bounds", () => {
    const layout: Layout = {
      root: {
        kind: "split",
        id: "high",
        dir: "row",
        ratio: 2,
        a: {
          kind: "split",
          id: "low",
          dir: "col",
          ratio: -1,
          a: { kind: "leaf", paneId: "p1", tabs: ["s1"], selectedTab: "s1" },
          b: { kind: "leaf", paneId: "p2", tabs: ["s2"], selectedTab: "s2" },
        },
        b: {
          kind: "split",
          id: "non-finite",
          dir: "col",
          ratio: Number.NaN,
          a: { kind: "leaf", paneId: "p3", tabs: ["s3"], selectedTab: "s3" },
          b: { kind: "leaf", paneId: "p4", tabs: ["s4"], selectedTab: "s4" },
        },
      },
      focusedPaneId: "p1",
    };
    const root = reconcile(layout, ["s1", "s2", "s3", "s4"]).root as PaneSplit;
    expect(root.ratio).toBe(LAYOUT_RATIO_MAX);
    expect((root.a as PaneSplit).ratio).toBe(LAYOUT_RATIO_MIN);
    expect((root.b as PaneSplit).ratio).toBe(
      (LAYOUT_RATIO_MIN + LAYOUT_RATIO_MAX) / 2,
    );
  });

  test("stable when nothing changed", () => {
    const layout = defaultLayout(["s1", "s2"]);
    const reconciled = reconcile(layout, ["s1", "s2"]);
    expect(tabsOf(reconciled)).toEqual([["s1", "s2"]]);
  });
});

describe("compactLeafForLayout", () => {
  test("falls back from focused empty panes without mutating desktop focus", () => {
    const layout: Layout = {
      root: {
        kind: "split",
        id: "outer",
        dir: "row",
        ratio: 0.5,
        a: { kind: "leaf", paneId: "empty", tabs: [], selectedTab: "" },
        b: {
          kind: "split",
          id: "occupied",
          dir: "col",
          ratio: 0.5,
          a: { kind: "leaf", paneId: "first", tabs: ["s1"], selectedTab: "s1" },
          b: { kind: "leaf", paneId: "active", tabs: ["s2"], selectedTab: "s2" },
        },
      },
      focusedPaneId: "empty",
    };
    const before = structuredClone(layout);
    expect(compactLeafForLayout(layout, "s2")?.paneId).toBe("active");
    expect(compactLeafForLayout(layout, null)?.paneId).toBe("first");
    expect(layout).toEqual(before);
    expect(layout.focusedPaneId).toBe("empty");
  });

});

describe("setRatio", () => {
  test("setRatio clamps to [0.1, 0.9] and targets the right split", () => {
    let l = defaultLayout(["a", "b"]);
    l = splitLeaf(l, rootPane(l), "row", "b", false);
    const split = l.root as PaneSplit;
    const clampedHi = setRatio(l.root, split.id, 5) as PaneSplit;
    expect(clampedHi.ratio).toBe(0.9);
    const clampedLo = setRatio(l.root, split.id, -1) as PaneSplit;
    expect(clampedLo.ratio).toBe(0.1);
    const untouched = setRatio(l.root, "nope", 0.3) as PaneSplit;
    expect(untouched.ratio).toBe(0.5);
  });
});

describe("geometry", () => {
  test("row split divides width, subtracts the gutter, emits one divider", () => {
    let l = defaultLayout(["a", "b"]);
    l = splitLeaf(l, rootPane(l), "row", "b", true); // a-side = b's pane
    const { panes, dividers } = layoutRects(l.root, { x: 0, y: 0, w: 100, h: 100 });
    expect(dividers.length).toBe(1);
    const aw = (100 - DIVIDER_PX) * 0.5; // 47
    const aPane = findLeafOfTab(l.root, "b")!.paneId; // insertFirst → a-side
    const bPane = findLeafOfTab(l.root, "a")!.paneId;
    expect(panes.get(aPane)).toEqual({ x: 0, y: 0, w: aw, h: 100 });
    expect(panes.get(bPane)).toEqual({ x: aw + DIVIDER_PX, y: 0, w: 100 - DIVIDER_PX - aw, h: 100 });
    expect(dividers[0]).toMatchObject({ x: aw, y: 0, w: DIVIDER_PX, h: 100, dir: "row" });
  });
  test("layoutView emits one pane per leaf with its selected tab + focus flag", () => {
    let l = defaultLayout(["a", "b"]);
    l = splitLeaf(l, rootPane(l), "row", "b", false); // focus b's pane
    const { panes } = layoutView(l, 200, 100);
    expect(panes.length).toBe(2);
    const bPane = panes.find((p) => p.selectedTab === "b")!;
    expect(bPane.focused).toBe(true);
    expect(panes.find((p) => p.selectedTab === "a")!.focused).toBe(false);
  });
});

describe("flatTabs", () => {
  test("single pane: tabs in stored order, all share the pane id", () => {
    const l = defaultLayout(["a", "b", "c"]);
    expect(flatTabs(l.root)).toEqual([
      { tabId: "a", paneId: rootPane(l) },
      { tabId: "b", paneId: rootPane(l) },
      { tabId: "c", paneId: rootPane(l) },
    ]);
  });
  test("two panes [s1,s2 | s3]: leaf order, then tab order within each leaf", () => {
    let l = defaultLayout(["s1", "s2", "s3"]);
    l = splitLeaf(l, rootPane(l), "row", "s3", false);
    const left = findLeafOfTab(l.root, "s1")!.paneId;
    const right = findLeafOfTab(l.root, "s3")!.paneId;
    expect(flatTabs(l.root)).toEqual([
      { tabId: "s1", paneId: left },
      { tabId: "s2", paneId: left },
      { tabId: "s3", paneId: right },
    ]);
  });
  test("nested split [[a | c] | b]: depth-first leaf order (a, c, b)", () => {
    let l = defaultLayout(["a", "b", "c"]);
    l = splitLeaf(l, rootPane(l), "row", "b", false); // [a,c | b]
    const acPane = findLeafOfTab(l.root, "a")!.paneId;
    l = splitLeaf(l, acPane, "row", "c", false);      // [[a | c] | b]
    expect(flatTabs(l.root).map((t) => t.tabId)).toEqual(["a", "c", "b"]);
    // each entry's paneId is the leaf that actually owns that tab
    for (const { tabId, paneId } of flatTabs(l.root)) {
      expect(findLeafOfTab(l.root, tabId)!.paneId).toBe(paneId);
    }
  });
});
