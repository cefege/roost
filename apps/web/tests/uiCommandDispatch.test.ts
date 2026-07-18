// Unit coverage for the ui-cc PURE core (lib/uiCommandCore.ts): agent
// UiCommands mapped onto the tiling tree via the same store/paneLayout.ts ops
// user gestures use, plus the tab-targeting predicate. Contract under test:
// valid commands transform the layout exactly like the equivalent gesture;
// anything referencing an unknown session / bad arg returns null (shell drops
// it). Deliberately imports ONLY uiCommandCore.ts — the dispatcher shell pulls
// paneLayoutStore (import-time pagehide hook), and loading it here would
// poison paneLayoutStore.test.ts's window-stub capture via bun's shared module
// cache (full-suite order coupling). Shell wiring is covered by the live
// ui-cc E2E (roost api ui …).

import { describe, test, expect } from "bun:test";
import type { UiCommand, UiCommandFrame } from "@roost/shared/proto/sync_pb";
import { applyUiCommandToLayout, frameAccepted } from "../src/lib/uiCommandCore.ts";
import {
  defaultLayout, splitLeaf, allLeaves, findLeaf, findLeafOfTab, type Layout,
} from "../src/store/paneLayout.ts";

// Plain command literals — protobuf-es Message<> only adds $typeName metadata,
// which the pure core never reads; the cast keeps the test free of the proto
// runtime (unchecked-cast-at-boundary, reason: fixture construction).
const cmd = (c: { case: string; value: object }): UiCommand =>
  ({ command: c }) as unknown as UiCommand;

const placeSplit = (sessionId: string, anchorSessionId: string, dir: string, insertFirst = false) =>
  cmd({ case: "placeSplit", value: { sessionId, anchorSessionId, dir, insertFirst } });
const selectTabCmd = (sessionId: string) => cmd({ case: "selectTab", value: { sessionId } });
const focusPaneCmd = (sessionId: string) => cmd({ case: "focusPane", value: { sessionId } });
const moveTabCmd = (sessionId: string, destSessionId: string) =>
  cmd({ case: "moveTab", value: { sessionId, destSessionId } });
const arrangeCmd = (preset: string) => cmd({ case: "arrange", value: { preset } });

const LIVE = ["s1", "s2", "s3"];

/** Row split: left pane [s1, s2] (selected s1) | right pane [s3]; focus right
 *  (splitLeaf focuses the pane it creates). */
function fixture(): { l: Layout; leftPane: string; rightPane: string } {
  let l = defaultLayout(LIVE); // one leaf [s1, s2, s3], s1 selected
  l = splitLeaf(l, allLeaves(l.root)[0].paneId, "row", "s3", false);
  const leftPane = findLeafOfTab(l.root, "s1")!.paneId;
  const rightPane = findLeafOfTab(l.root, "s3")!.paneId;
  expect(leftPane).not.toBe(rightPane);
  expect(l.focusedPaneId).toBe(rightPane);
  return { l, leftPane, rightPane };
}

describe("applyUiCommandToLayout placeSplit", () => {
  test("creates a sibling pane holding the new session, focused", () => {
    const { l, leftPane } = fixture();
    const next = applyUiCommandToLayout(l, placeSplit("s4", "s1", "col"), LIVE)!;
    expect(next).not.toBeNull();
    const newLeaf = findLeafOfTab(next.root, "s4")!;
    expect(newLeaf.tabs).toEqual(["s4"]);
    expect(newLeaf.selectedTab).toBe("s4");
    expect(next.focusedPaneId).toBe(newLeaf.paneId); // focus follows the new pane
    // anchor pane survives with its tabs intact; tree grew by one leaf
    expect(findLeaf(next.root, leftPane)!.tabs).toEqual(["s1", "s2"]);
    expect(allLeaves(next.root)).toHaveLength(3);
  });
  test("insertFirst puts the new pane on the left/top side", () => {
    const { l } = fixture();
    const next = applyUiCommandToLayout(l, placeSplit("s4", "s3", "row", true), LIVE)!;
    // the split that holds s4 lists it as child `a` — verified via leaf order
    const order = allLeaves(next.root).map((leaf) => leaf.tabs[0]);
    expect(order.indexOf("s4")).toBeLessThan(order.indexOf("s3"));
  });
  test("unknown anchor session → null", () => {
    const { l } = fixture();
    expect(applyUiCommandToLayout(l, placeSplit("s4", "nope", "row"), LIVE)).toBeNull();
  });
  test("invalid dir → null", () => {
    const { l } = fixture();
    expect(applyUiCommandToLayout(l, placeSplit("s4", "s1", "diagonal"), LIVE)).toBeNull();
  });
});

describe("applyUiCommandToLayout selectTab", () => {
  test("selects the tab in its pane and focuses that pane", () => {
    const { l, leftPane } = fixture();
    const next = applyUiCommandToLayout(l, selectTabCmd("s2"), LIVE)!;
    expect(findLeaf(next.root, leftPane)!.selectedTab).toBe("s2");
    expect(next.focusedPaneId).toBe(leftPane);
  });
  test("unknown session → null", () => {
    const { l } = fixture();
    expect(applyUiCommandToLayout(l, selectTabCmd("nope"), LIVE)).toBeNull();
  });
});

describe("applyUiCommandToLayout focusPane", () => {
  test("focuses the pane CONTAINING the session without changing its selection", () => {
    const { l, leftPane } = fixture();
    // s2 is a background tab of the left pane — focusing by it must focus the
    // pane but keep s1 selected (focus ≠ select; the deck navigates to s1).
    const next = applyUiCommandToLayout(l, focusPaneCmd("s2"), LIVE)!;
    expect(next.focusedPaneId).toBe(leftPane);
    expect(findLeaf(next.root, leftPane)!.selectedTab).toBe("s1");
  });
  test("unknown session → null", () => {
    const { l } = fixture();
    expect(applyUiCommandToLayout(l, focusPaneCmd("nope"), LIVE)).toBeNull();
  });
});

describe("applyUiCommandToLayout moveTab", () => {
  test("moves between panes, selects it there, collapses the emptied source", () => {
    const { l, leftPane } = fixture();
    // right pane holds only s3 → moving it into s1's pane empties + collapses it
    const next = applyUiCommandToLayout(l, moveTabCmd("s3", "s1"), LIVE)!;
    expect(next.root.kind).toBe("leaf"); // split collapsed back to a single pane
    const only = allLeaves(next.root)[0];
    expect(only.paneId).toBe(leftPane);
    expect(only.tabs).toEqual(["s1", "s2", "s3"]);
    expect(only.selectedTab).toBe("s3");
    expect(next.focusedPaneId).toBe(leftPane);
  });
  test("unknown moved session → null; unknown dest session → null", () => {
    const { l } = fixture();
    expect(applyUiCommandToLayout(l, moveTabCmd("nope", "s1"), LIVE)).toBeNull();
    expect(applyUiCommandToLayout(l, moveTabCmd("s3", "nope"), LIVE)).toBeNull();
  });
});

describe("applyUiCommandToLayout arrange", () => {
  test("balance keeps the tree shape (paneIds, tabs, focus) and equalizes ratios", () => {
    const { l } = fixture();
    const next = applyUiCommandToLayout(l, arrangeCmd("balance"), LIVE)!;
    expect(allLeaves(next.root).map((p) => p.paneId).sort())
      .toEqual(allLeaves(l.root).map((p) => p.paneId).sort());
    expect(next.focusedPaneId).toBe(l.focusedPaneId);
    expect(next.root.kind).toBe("split");
    if (next.root.kind === "split") expect(next.root.ratio).toBe(0.5); // 1 leaf each side
  });
  test("rebuild preset re-tiles one live session per pane", () => {
    const { l } = fixture();
    const next = applyUiCommandToLayout(l, arrangeCmd("even"), LIVE)!;
    const leaves = allLeaves(next.root);
    expect(leaves).toHaveLength(LIVE.length);
    expect(leaves.flatMap((p) => p.tabs).sort()).toEqual([...LIVE].sort());
  });
  test("unknown preset → null", () => {
    const { l } = fixture();
    expect(applyUiCommandToLayout(l, arrangeCmd("sideways"), LIVE)).toBeNull();
  });
});

describe("applyUiCommandToLayout misc", () => {
  test("non-layout command (navigate lives in the shell) → null", () => {
    const { l } = fixture();
    expect(applyUiCommandToLayout(l, cmd({ case: "navigate", value: { path: "/" } }), LIVE)).toBeNull();
  });
  test("input layout is never mutated", () => {
    const { l } = fixture();
    const snapshot = JSON.stringify(l);
    applyUiCommandToLayout(l, moveTabCmd("s3", "s1"), LIVE);
    applyUiCommandToLayout(l, arrangeCmd("even"), LIVE);
    expect(JSON.stringify(l)).toBe(snapshot);
  });
});

describe("frameAccepted tab targeting", () => {
  const frame = (targetTabId: string, c: UiCommand): UiCommandFrame =>
    ({ targetTabId, command: c }) as unknown as UiCommandFrame;
  test("frame targeted at another tab is rejected", () => {
    expect(frameAccepted(frame("some-other-tab", cmd({ case: "navigate", value: { path: "/x" } })), "own-tab")).toBe(false);
  });
  test("broadcast (empty targetTabId) is accepted by every tab", () => {
    expect(frameAccepted(frame("", cmd({ case: "navigate", value: { path: "/s/abc" } })), "own-tab")).toBe(true);
  });
  test("frame targeted at THIS tab is accepted", () => {
    expect(frameAccepted(frame("own-tab", cmd({ case: "navigate", value: { path: "/x" } })), "own-tab")).toBe(true);
  });
});
