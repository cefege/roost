// Preset tile layouts ("Arrange"): turn a folder's N sessions into a tidy
// PaneNode tree, one session per pane. even = equal columns; rows = equal
// full-width rows; tiled = grid (splits alternate row/col); main-vertical =
// one big pane on the left + the rest stacked on the right. Consumed via
// paneLayoutStore.commitLayout(folderKey, presetLayout(kind, ids)); reconcile
// then folds in any live session the preset didn't place. layoutRects renders
// any tree, so presets need zero renderer changes.
//
// balance is the non-rebuild op: it keeps the CURRENT tree (panes, tab groups,
// focus) and only recomputes each split ratio so every pane gets equal area.
// arrangeLayout is the single dispatcher the UI (ArrangeMenu.tsx) and
// TerminalDeck's ⌘⌥ keymap call.

import { type Layout, type PaneNode, type PaneDir, defaultLayout, allLeaves } from "./paneLayout.ts";

export type PresetKind = "even" | "rows" | "tiled" | "main-vertical";

const newId = (): string => crypto.randomUUID();
const leafOf = (sid: string): PaneNode => ({ kind: "leaf", paneId: newId(), tabs: [sid], selectedTab: sid });
const otherDir = (d: PaneDir): PaneDir => (d === "row" ? "col" : "row");

/** Balanced binary tree over ids; ratio = size share of the left subtree so
 *  every leaf ends up ~equal. `alternate` flips row/col each level → a grid. */
function buildBalanced(ids: string[], dir: PaneDir, alternate: boolean): PaneNode {
  if (ids.length <= 1) return leafOf(ids[0]);
  const mid = Math.ceil(ids.length / 2);
  const childDir = alternate ? otherDir(dir) : dir;
  return {
    kind: "split",
    id: newId(),
    dir,
    ratio: mid / ids.length,
    a: buildBalanced(ids.slice(0, mid), childDir, alternate),
    b: buildBalanced(ids.slice(mid), childDir, alternate),
  };
}

export function presetLayout(kind: PresetKind, sessionIds: string[]): Layout {
  const ids = sessionIds.filter(Boolean);
  if (ids.length <= 1) return defaultLayout(ids);
  let root: PaneNode;
  if (kind === "main-vertical") {
    root = {
      kind: "split",
      id: newId(),
      dir: "row",
      ratio: 0.6,
      a: leafOf(ids[0]),
      b: buildBalanced(ids.slice(1), "col", false),
    };
  } else {
    // even = row splits (equal columns); rows = col splits (equal rows);
    // tiled = alternate for a grid.
    root = buildBalanced(ids, kind === "rows" ? "col" : "row", kind === "tiled");
  }
  return { root, focusedPaneId: allLeaves(root)[0].paneId };
}

/** Set each split ratio to leafCount(a)/(leafCount(a)+leafCount(b)); this
 *  telescopes to exactly 1/N area per leaf (modulo DIVIDER_PX gutters). Stays
 *  in (0,1) so no RATIO_MIN/MAX clamp — those guard user drags, not balance. */
function balanceNode(n: PaneNode): PaneNode {
  if (n.kind === "leaf") return n;
  const a = balanceNode(n.a), b = balanceNode(n.b);
  const ca = allLeaves(a).length, cb = allLeaves(b).length;
  return { ...n, ratio: ca / (ca + cb), a, b };
}

/** Equalize pane areas in place: same tree, same paneIds/tabs/focus, new ratios. */
export function balanceLayout(layout: Layout): Layout {
  return { ...layout, root: balanceNode(layout.root) };
}

export type ArrangeKind = PresetKind | "balance";

export function arrangeLayout(kind: ArrangeKind, layout: Layout, sessionIds: string[]): Layout {
  return kind === "balance" ? balanceLayout(layout) : presetLayout(kind, sessionIds);
}
