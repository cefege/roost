// Terminal tiling layout — the "tabs-in-panes" tree for ONE folder bucket.
// A leaf is a pane holding an ordered tab list (SessionIds) + a selected tab;
// a split joins two children left|right (row) or top|bottom (col) at a ratio.
// All ops here are PURE (immutable tree → new tree) so they're trivially
// testable and Solid-signal friendly. Reconcile folds the live session set in;
// layoutRects/visibleBySession derive pixel rects (ratios are the truth).
// Persistence + reactivity live in store/paneLayoutStore.ts. Geometry is
// consumed by MainPane.tsx's TerminalDeck.

export type PaneDir = "row" | "col"; // row = left|right; col = top|bottom

export interface PaneLeaf {
  kind: "leaf";
  paneId: string;
  tabs: string[]; // ordered SessionIds
  selectedTab: string; // a SessionId in `tabs`, or "" when the leaf is empty
}
export interface PaneSplit {
  kind: "split";
  id: string; // stable id so a divider can address its split
  dir: PaneDir;
  ratio: number; // 0..1 fraction of the primary axis given to child `a`
  a: PaneNode;
  b: PaneNode;
}
export type PaneNode = PaneLeaf | PaneSplit;

export interface Layout {
  root: PaneNode;
  focusedPaneId: string;
}

export interface Rect {
  x: number;
  y: number;
  w: number;
  h: number;
}
export interface DividerRect extends Rect {
  splitId: string;
  dir: PaneDir;
  ratio: number; // the split's current ratio (so a no-move release is a no-op)
  regionStart: number; // px origin of the split's area along its axis (x row / y col)
  regionLen: number; // px length of that area (w row / h col) — drag → ratio
}
/** Everything the deck needs to render ONE pane this frame: its rect, its full
 *  tab list (for the strip), the selected tab (the terminal that paints), and
 *  whether it owns the keyboard. */
export interface PaneView {
  paneId: string;
  rect: Rect;
  tabIds: string[];
  selectedTab: string;
  focused: boolean;
}

const RATIO_MIN = 0.1;
const RATIO_MAX = 0.9;
/** Gutter reserved between two split children for the drag divider (px). */
export const DIVIDER_PX = 6;

const newId = (): string => crypto.randomUUID();

// ── construction ────────────────────────────────────────────────────────────

/** A single pane holding every given session; the first is selected. */
export function defaultLayout(sessionIds: string[]): Layout {
  const paneId = newId();
  return {
    root: { kind: "leaf", paneId, tabs: [...sessionIds], selectedTab: sessionIds[0] ?? "" },
    focusedPaneId: paneId,
  };
}

// ── tree walkers (pure) ──────────────────────────────────────────────────────

export function allLeaves(node: PaneNode): PaneLeaf[] {
  if (node.kind === "leaf") return [node];
  return [...allLeaves(node.a), ...allLeaves(node.b)];
}

export function findLeaf(node: PaneNode, paneId: string): PaneLeaf | null {
  if (node.kind === "leaf") return node.paneId === paneId ? node : null;
  return findLeaf(node.a, paneId) ?? findLeaf(node.b, paneId);
}

export function findLeafOfTab(node: PaneNode, tab: string): PaneLeaf | null {
  if (node.kind === "leaf") return node.tabs.includes(tab) ? node : null;
  return findLeafOfTab(node.a, tab) ?? findLeafOfTab(node.b, tab);
}

function firstLeaf(node: PaneNode): PaneLeaf {
  return node.kind === "leaf" ? node : firstLeaf(node.a);
}

/** Flatten a layout's pane tree into one ordered tab list — the compact/mobile
 *  view. Pane topology is desktop-only: on a phone the deck paints a single
 *  terminal, so every tab across every pane collapses to one scrollable row.
 *  Order = leaf order (allLeaves: a-before-b = left/top before right/bottom),
 *  then tab order within each leaf. Each entry carries its owning paneId so a
 *  tap selects the tab AND focuses the right pane (selectTab focuses the leaf
 *  that owns the tab). Pure + testable. */
export function flatTabs(root: PaneNode): { tabId: string; paneId: string }[] {
  return allLeaves(root).flatMap((leaf) =>
    leaf.tabs.map((tabId) => ({ tabId, paneId: leaf.paneId })),
  );
}

/** Replace the leaf `paneId` with `fn(leaf)` (may return a split → grows tree). */
function updateLeaf(node: PaneNode, paneId: string, fn: (leaf: PaneLeaf) => PaneNode): PaneNode {
  if (node.kind === "leaf") return node.paneId === paneId ? fn(node) : node;
  return { ...node, a: updateLeaf(node.a, paneId, fn), b: updateLeaf(node.b, paneId, fn) };
}

/** Set the divider ratio of split `splitId` (clamped). */
export function setRatio(node: PaneNode, splitId: string, ratio: number): PaneNode {
  if (node.kind === "leaf") return node;
  const clamped = Math.max(RATIO_MIN, Math.min(RATIO_MAX, ratio));
  const next: PaneSplit = {
    ...node,
    ratio: node.id === splitId ? clamped : node.ratio,
    a: setRatio(node.a, splitId, ratio),
    b: setRatio(node.b, splitId, ratio),
  };
  return next;
}

function removeTabFromLeaf(leaf: PaneLeaf, tab: string): PaneLeaf {
  const idx = leaf.tabs.indexOf(tab);
  if (idx < 0) return leaf;
  const tabs = leaf.tabs.filter((t) => t !== tab);
  const selectedTab =
    leaf.selectedTab === tab ? (tabs[idx] ?? tabs[idx - 1] ?? tabs[0] ?? "") : leaf.selectedTab;
  return { ...leaf, tabs, selectedTab };
}

/** Drop `tab` from whichever leaf holds it. Does NOT collapse emptied leaves. */
function removeTabEverywhere(node: PaneNode, tab: string): PaneNode {
  if (node.kind === "leaf") return removeTabFromLeaf(node, tab);
  return { ...node, a: removeTabEverywhere(node.a, tab), b: removeTabEverywhere(node.b, tab) };
}

/** Bottom-up: a split with an empty-leaf child becomes its other child. */
function collapseEmpties(node: PaneNode): PaneNode {
  if (node.kind === "leaf") return node;
  const a = collapseEmpties(node.a);
  const b = collapseEmpties(node.b);
  if (a.kind === "leaf" && a.tabs.length === 0) return b;
  if (b.kind === "leaf" && b.tabs.length === 0) return a;
  return { ...node, a, b };
}

/** Point focus at a real pane; if `preferred` is gone, fall back to first leaf. */
function fixFocus(root: PaneNode, preferred: string): string {
  return findLeaf(root, preferred) ? preferred : firstLeaf(root).paneId;
}

// ── mutations (Layout → Layout) ──────────────────────────────────────────────

/** Fold the live session set into a stored layout: prune dead tabs, collapse
 *  emptied panes, append never-placed live sessions to the focused pane. */
export function reconcile(layout: Layout, liveIds: string[]): Layout {
  const live = new Set(liveIds);
  // 1. prune dead tabs from every leaf
  let root: PaneNode = mapLeaves(layout.root, (leaf) => {
    const tabs = leaf.tabs.filter((t) => live.has(t));
    if (tabs.length === leaf.tabs.length) return leaf;
    const selectedTab = live.has(leaf.selectedTab) ? leaf.selectedTab : (tabs[0] ?? "");
    return { ...leaf, tabs, selectedTab };
  });
  // 2. collapse panes emptied by the prune
  root = collapseEmpties(root);
  // 3. append live sessions that aren't placed anywhere → focused pane
  const placed = new Set(allLeaves(root).flatMap((l) => l.tabs));
  const orphans = liveIds.filter((id) => !placed.has(id));
  const focusedPaneId = fixFocus(root, layout.focusedPaneId);
  if (orphans.length > 0) {
    root = updateLeaf(root, focusedPaneId, (leaf) => ({
      ...leaf,
      tabs: [...leaf.tabs, ...orphans],
      selectedTab: leaf.selectedTab || orphans[0],
    }));
  }
  return { root, focusedPaneId };
}

function mapLeaves(node: PaneNode, fn: (leaf: PaneLeaf) => PaneLeaf): PaneNode {
  if (node.kind === "leaf") return fn(node);
  return { ...node, a: mapLeaves(node.a, fn), b: mapLeaves(node.b, fn) };
}

/** Move `movingTab` into a brand-new pane split off `targetPaneId`. Used by
 *  ⌘D (movingTab = a freshly spawned session) and drag-to-edge (existing tab).
 *  insertFirst = new pane takes the left/top side. Focus follows the new pane. */
export function splitLeaf(
  layout: Layout,
  targetPaneId: string,
  dir: PaneDir,
  movingTab: string,
  insertFirst: boolean,
): Layout {
  const target = findLeaf(layout.root, targetPaneId);
  if (!target) return layout;
  const source = findLeafOfTab(layout.root, movingTab);
  // Splitting a pane by moving its own only tab is a no-op (nothing left behind).
  if (source && source.paneId === targetPaneId && target.tabs.length <= 1) return layout;

  let root = removeTabEverywhere(layout.root, movingTab);
  const newLeaf: PaneLeaf = { kind: "leaf", paneId: newId(), tabs: [movingTab], selectedTab: movingTab };
  root = updateLeaf(root, targetPaneId, (leaf) => {
    const split: PaneSplit = {
      kind: "split",
      id: newId(),
      dir,
      ratio: 0.5,
      a: insertFirst ? newLeaf : leaf,
      b: insertFirst ? leaf : newLeaf,
    };
    return split;
  });
  root = collapseEmpties(root);
  return { ...layout, root, focusedPaneId: newLeaf.paneId };
}

/** Move `tab` into `toPaneId` at `index` (append when index omitted), selecting
 *  it there. Emptied source panes collapse. This is drag-onto-a-strip / merge. */
export function moveTab(layout: Layout, tab: string, toPaneId: string, index?: number): Layout {
  const target = findLeaf(layout.root, toPaneId);
  if (!target) return layout;
  const source = findLeafOfTab(layout.root, tab);
  if (source && source.paneId === toPaneId) {
    // same-pane → reorder within the strip
    return reorderTab(layout, toPaneId, moveWithin(target.tabs, tab, index));
  }
  let root = removeTabEverywhere(layout.root, tab);
  root = updateLeaf(root, toPaneId, (leaf) => {
    const tabs = [...leaf.tabs];
    const at = index === undefined ? tabs.length : Math.max(0, Math.min(index, tabs.length));
    tabs.splice(at, 0, tab);
    return { ...leaf, tabs, selectedTab: tab };
  });
  root = collapseEmpties(root);
  return { ...layout, root, focusedPaneId: toPaneId };
}

function moveWithin(tabs: string[], tab: string, index?: number): string[] {
  const rest = tabs.filter((t) => t !== tab);
  const at = index === undefined ? rest.length : Math.max(0, Math.min(index, rest.length));
  rest.splice(at, 0, tab);
  return rest;
}

/** Replace a pane's tab order (drag-reorder within its own strip). */
export function reorderTab(layout: Layout, paneId: string, orderedTabs: string[]): Layout {
  const root = updateLeaf(layout.root, paneId, (leaf) => ({ ...leaf, tabs: orderedTabs }));
  return { ...layout, root };
}

/** Select a tab in its pane and focus that pane (mirrors a click / URL nav). */
export function selectTab(layout: Layout, tab: string): Layout {
  const leaf = findLeafOfTab(layout.root, tab);
  if (!leaf) return layout;
  const root = updateLeaf(layout.root, leaf.paneId, (l) => ({ ...l, selectedTab: tab }));
  return { ...layout, root, focusedPaneId: leaf.paneId };
}

export function focusPane(layout: Layout, paneId: string): Layout {
  if (!findLeaf(layout.root, paneId)) return layout;
  return { ...layout, focusedPaneId: paneId };
}

/** Close a tab (session already killed elsewhere): drop it, collapse an emptied
 *  pane, keep focus valid. Returns an empty single-leaf layout if nothing left. */
export function closeTab(layout: Layout, tab: string): Layout {
  let root = removeTabEverywhere(layout.root, tab);
  root = collapseEmpties(root);
  const focusedPaneId = fixFocus(root, layout.focusedPaneId);
  return { root, focusedPaneId };
}

// ── geometry (ratios → pixels) ───────────────────────────────────────────────

/** Recursively assign each pane a pixel rect + collect divider handles. */
export function layoutRects(root: PaneNode, area: Rect): { panes: Map<string, Rect>; dividers: DividerRect[] } {
  const panes = new Map<string, Rect>();
  const dividers: DividerRect[] = [];
  walk(root, area);
  return { panes, dividers };

  function walk(node: PaneNode, r: Rect): void {
    if (node.kind === "leaf") {
      panes.set(node.paneId, r);
      return;
    }
    const g = DIVIDER_PX;
    if (node.dir === "row") {
      const aw = Math.max(0, (r.w - g) * node.ratio);
      const bw = Math.max(0, r.w - g - aw);
      walk(node.a, { x: r.x, y: r.y, w: aw, h: r.h });
      dividers.push({ splitId: node.id, dir: node.dir, ratio: node.ratio, x: r.x + aw, y: r.y, w: g, h: r.h, regionStart: r.x, regionLen: r.w });
      walk(node.b, { x: r.x + aw + g, y: r.y, w: bw, h: r.h });
    } else {
      const ah = Math.max(0, (r.h - g) * node.ratio);
      const bh = Math.max(0, r.h - g - ah);
      walk(node.a, { x: r.x, y: r.y, w: r.w, h: ah });
      dividers.push({ splitId: node.id, dir: node.dir, ratio: node.ratio, x: r.x, y: r.y + ah, w: r.w, h: g, regionStart: r.y, regionLen: r.h });
      walk(node.b, { x: r.x, y: r.y + ah + g, w: r.w, h: bh });
    }
  }
}

/** The render list for a frame: one PaneView per visible pane + the divider
 *  handles between them. The deck paints each pane's SELECTED tab at its rect
 *  and the pane's strip above it; everything else stays mounted-but-hidden. */
export function layoutView(layout: Layout, w: number, h: number): { panes: PaneView[]; dividers: DividerRect[] } {
  const area: Rect = { x: 0, y: 0, w, h };
  const { panes: rects, dividers } = layoutRects(layout.root, area);
  const panes: PaneView[] = [];
  for (const leaf of allLeaves(layout.root)) {
    const rect = rects.get(leaf.paneId);
    if (!rect) continue;
    panes.push({ paneId: leaf.paneId, rect, tabIds: leaf.tabs, selectedTab: leaf.selectedTab, focused: leaf.paneId === layout.focusedPaneId });
  }
  return { panes, dividers };
}
