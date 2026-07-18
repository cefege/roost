// Edge-band drop-zone routing for drag-to-tile. Given a
// pane's rect and a pointer position inside it, decide whether a dropped tab
// should SPLIT the pane on a side (pointer in the outer band) or MERGE into it
// (pointer in the center). Edge band = max(80px, 25% of the dimension); the
// nearest in-band edge wins, ties favour horizontal (left/right).
// Callers: TerminalDeck.tsx (drop routing + drop-zone overlay).

export type DropZone = "center" | "left" | "right" | "top" | "bottom" | "reorder";

export interface Rect {
  x: number;
  y: number;
  w: number;
  h: number;
}

const EDGE_RATIO = 0.25;
const EDGE_MIN = 80;

/** Which zone a pointer at (x,y) lands in, in the SAME coordinate space as rect. */
export function dropZoneFor(rect: Rect, x: number, y: number): DropZone {
  const bx = Math.max(EDGE_MIN, rect.w * EDGE_RATIO);
  const by = Math.max(EDGE_MIN, rect.h * EDGE_RATIO);
  const left = x - rect.x;
  const right = rect.x + rect.w - x;
  const top = y - rect.y;
  const bottom = rect.y + rect.h - y;
  const cand: Array<[DropZone, number]> = [];
  if (left < bx) cand.push(["left", left]);
  if (right < bx) cand.push(["right", right]);
  if (top < by) cand.push(["top", top]);
  if (bottom < by) cand.push(["bottom", bottom]);
  if (cand.length === 0) return "center";
  // closest edge wins; horizontal (left/right) breaks ties by sorting first
  const order: Record<DropZone, number> = { left: 0, right: 1, top: 2, bottom: 3, center: 4, reorder: 5 };
  cand.sort((a, b) => a[1] - b[1] || order[a[0]] - order[b[0]]);
  return cand[0][0];
}

/** The split a non-center zone maps to (dir + which side the NEW pane takes). */
export function zoneToSplit(zone: DropZone): { dir: "row" | "col"; insertFirst: boolean } | null {
  switch (zone) {
    case "left": return { dir: "row", insertFirst: true };
    case "right": return { dir: "row", insertFirst: false };
    case "top": return { dir: "col", insertFirst: true };
    case "bottom": return { dir: "col", insertFirst: false };
    default: return null; // center → merge into the pane, not a split
  }
}

/** The region a zone highlights (for the drop overlay): the half that the new
 *  pane will occupy, or the whole pane for a center merge. */
export function zoneRect(rect: Rect, zone: DropZone): Rect {
  const hw = rect.w / 2;
  const hh = rect.h / 2;
  switch (zone) {
    case "left": return { x: rect.x, y: rect.y, w: hw, h: rect.h };
    case "right": return { x: rect.x + hw, y: rect.y, w: hw, h: rect.h };
    case "top": return { x: rect.x, y: rect.y, w: rect.w, h: hh };
    case "bottom": return { x: rect.x, y: rect.y + hh, w: rect.w, h: hh };
    default: return { ...rect };
  }
}

/** A pane's id + its rect in the deck's coordinate space. */
export interface PaneBox {
  paneId: string;
  rect: Rect;
}

/** The pane + zone a dragged tab targets. null = pointer over no pane (off-deck)
 *  or over its own strip (reorder handled by the strip slide, no overlay).
 *  zone "reorder" = home-pane body center: preview an overlay, but NOT a tile op. */
export type TileTarget = { paneId: string; rect: Rect; zone: DropZone } | null;

/** Route a drag pointer (deck-local x,y) to a tile target. Pure mirror of the
 *  TerminalDeck drop logic so it's unit-testable (components don't mount under
 *  bun test). stripH = the tab-strip band height at each pane's top. */
export function tileTargetFor(panes: PaneBox[], originPaneId: string, x: number, y: number, stripH: number): TileTarget {
  const pane = panes.find((p) => x >= p.rect.x && x < p.rect.x + p.rect.w && y >= p.rect.y && y < p.rect.y + p.rect.h);
  if (!pane) return null;
  const home = pane.paneId === originPaneId;
  // strip band = merge into that pane; home strip = reorder (strip slide is the cue) → no overlay
  if (y < pane.rect.y + stripH) return home ? null : { paneId: pane.paneId, rect: pane.rect, zone: "center" };
  const zone = dropZoneFor(pane.rect, x, y);
  // home body center = reorder: show a distinct overlay, but keep the drop a reorder
  if (zone === "center" && home) return { paneId: pane.paneId, rect: pane.rect, zone: "reorder" };
  return { paneId: pane.paneId, rect: pane.rect, zone };
}
