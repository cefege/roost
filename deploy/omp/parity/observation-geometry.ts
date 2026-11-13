import type { Page } from "playwright";

export interface BoxEdges { top: number; bottom: number; left: number; right: number }
/** Sub-pixel layout rounding; differences below it are the same edge. */
const EDGE_TOLERANCE = 0.5;

/** Blank rows between the reference's last row and the subject's first; negative when they overlap or the subject is above. */
export function terminalRowGap(row: number, referenceRow: number, referenceLines: number): number {
  return row - referenceRow - referenceLines;
}

/**
 * Rows are lines of content, not pixel tops: elements of different heights in
 * a vertically centred or baseline row share a row when each one's centre line
 * lies inside the other's vertical extent. Stacked elements never do, however
 * close they sit, and a tall element beside two stacked ones shares a row with
 * neither (its centre falls between them).
 */
export function sameVisualRow(current: BoxEdges, reference: BoxEdges): boolean {
  const centre = (box: BoxEdges): number => (box.top + box.bottom) / 2;
  const inside = (value: number, box: BoxEdges): boolean => value >= box.top - EDGE_TOLERANCE && value <= box.bottom + EDGE_TOLERANCE;
  return inside(centre(current), reference) && inside(centre(reference), current);
}

/**
 * Below means the whole subject starts at or after the reference's bottom edge,
 * matching the terminal read where the subject's first row follows the
 * reference's last row (a blank row between them is not required, and any
 * overlap is not below).
 */
export function strictlyBelow(current: BoxEdges, reference: BoxEdges): boolean {
  return current.top >= reference.bottom - EDGE_TOLERANCE;
}

export function relativeGeometryValue(current: BoxEdges, reference: BoxEdges, read: string): unknown {
  if (read === "sameRow") return sameVisualRow(current, reference);
  if (read === "rowGap") return current.top - reference.bottom;
  if (read === "columnGap") return current.left - reference.right;
  if (read === "below") return strictlyBelow(current, reference);
  if (read === "sameRight") return Math.abs(current.right - reference.right) < EDGE_TOLERANCE;
  if (read === "sameColumn") return Math.abs(current.left - reference.left) < EDGE_TOLERANCE;
  throw new Error(`Unsupported relative geometry ${read}`);
}

export async function relativeGeometry(page: Page, selector: string, referenceSelector: string | undefined, read: string): Promise<unknown> {
  if (!referenceSelector) throw new Error(`${read} requires an actual reference element`);
  const target = page.locator(selector), reference = page.locator(referenceSelector);
  const counts = await Promise.all([target.count(), reference.count()]);
  if (counts.some(count => count > 1)) throw new Error("Ambiguous relative geometry target");
  if (counts.some(count => count === 0)) return undefined;
  if (!(await target.isVisible()) || !(await reference.isVisible())) return undefined;
  const handle = await reference.elementHandle();
  if (!handle) return undefined;
  try {
    const boxes = await target.evaluate((element, options) => {
      const edges = (rect: DOMRect) => ({ top: rect.top, bottom: rect.bottom, left: rect.left, right: rect.right });
      const current = element.getBoundingClientRect(), reference = options.reference.getBoundingClientRect();
      if (current.width <= 0 || current.height <= 0 || reference.width <= 0 || reference.height <= 0) return undefined;
      if (getComputedStyle(element).visibility !== "visible" || getComputedStyle(options.reference).visibility !== "visible") return undefined;
      return { current: edges(current), reference: edges(reference) };
    }, { reference: handle });
    return boxes ? relativeGeometryValue(boxes.current, boxes.reference, read) : undefined;
  } finally { await handle.dispose(); }
}
