import {
  spansText,
  type CellGridFrame,
  type CellRow,
} from "@roost/shared/cell";
import type { TerminalCellGeometry } from "./terminalMouse.ts";

export const SCROLLBACK_BLOCK_ROWS = 250;
export const DEFAULT_CELL_ROW_PX = 16.8;

/** The exact contain-intrinsic-size value for a measured block of rows. */
export function blockPlaceholder(rows: number, rowHeight: number): string {
  return `${(
    rows * (rowHeight > 0 ? rowHeight : DEFAULT_CELL_ROW_PX)
  ).toFixed(2)}px`;
}

export function sizeScrollbackBlock(
  block: HTMLElement,
  rows: number,
  rowHeight: number,
): void {
  block.style.setProperty(
    "contain-intrinsic-size",
    blockPlaceholder(rows, rowHeight),
  );
}

export interface CellRendererElements {
  doc: Document;
  spacer: HTMLElement;
  scrollback: HTMLElement;
  viewport: HTMLElement;
  cursor: HTMLElement;
  ghosts: HTMLElement;
}

export function createCellRendererElements(
  container: HTMLElement,
): CellRendererElements {
  const doc = container.ownerDocument;
  container.classList.add("wterm", "cell-grid");
  container.setAttribute("role", "log");
  const spacer = doc.createElement("div");
  spacer.className = "cell-sb-spacer";
  spacer.style.setProperty("height", "0px");
  const scrollback = doc.createElement("div");
  scrollback.className = "cell-scrollback";
  const viewport = doc.createElement("div");
  viewport.className = "cell-viewport";
  viewport.style.position = "relative";
  const cursor = doc.createElement("div");
  cursor.className = "cell-cursor";
  const ghosts = doc.createElement("div");
  ghosts.className = "cell-ghosts";
  container.appendChild(spacer);
  container.appendChild(scrollback);
  container.appendChild(viewport);
  return { doc, spacer, scrollback, viewport, cursor, ghosts };
}

export function createGhostElements(
  doc: Document,
  ghosts: ReadonlyMap<string, { x: number; y: number; label?: string }>,
): HTMLElement[] {
  const boxes: HTMLElement[] = [];
  for (const [id, ghost] of ghosts) {
    const box = doc.createElement("div");
    box.className = "cell-ghost";
    box.dataset.operatorId = id;
    box.title = ghost.label ?? id;
    box.style.transform = `translate(${ghost.x}ch, ${ghost.y}lh)`;
    boxes.push(box);
  }
  return boxes;
}

export function paintedRowAt(
  rows: readonly CellRow[],
  index: number,
): CellRow | null {
  let lo = 0;
  let hi = rows.length;
  while (lo < hi) {
    const mid = (lo + hi) >>> 1;
    const row = rows[mid]!;
    if (row.index < index) lo = mid + 1;
    else hi = mid;
  }
  const row = rows[lo];
  return row?.index === index ? row : null;
}

export function cellGridText(frame: CellGridFrame | null): string {
  if (!frame) return "";
  return frame.viewportRows.map((row) => spansText(row.spans)).join("\n");
}

export function cellScrollbackText(
  frame: CellGridFrame | null,
  maxRows: number,
): string {
  if (!frame) return "";
  return frame.scrollbackRows
    .slice(Math.max(0, frame.scrollbackRows.length - maxRows))
    .map((row) => spansText(row.spans))
    .join("\n");
}

export function measureCellRowHeight(
  doc: Document,
  viewport: HTMLElement,
): number {
  const probe = doc.createElement("div");
  probe.className = "cell-row";
  probe.style.position = "absolute";
  probe.style.visibility = "hidden";
  probe.textContent = " ";
  viewport.appendChild(probe);
  const height = probe.getBoundingClientRect?.().height ?? 0;
  probe.remove();
  return height;
}

export function terminalViewportCellGeometry(
  frame: CellGridFrame | null,
  viewport: HTMLElement,
  rowHeight: number,
): TerminalCellGeometry | null {
  if (!frame || rowHeight <= 0) return null;
  const rect = viewport.getBoundingClientRect?.();
  if (!rect) return null;
  const cellWidth = rect.width / frame.cols;
  if (cellWidth <= 0) return null;
  return {
    left: rect.left,
    top: rect.top,
    cellWidth,
    rowHeight,
    cols: frame.cols,
    rows: frame.rows,
  };
}

export function syncAlternateScreen(
  container: HTMLElement,
  frame: CellGridFrame | null,
  painted: boolean | null,
): boolean {
  const active = frame?.altScreen === true;
  if (active !== painted) container.classList.toggle("alt-active", active);
  return active;
}

export function paintCellGridWidth(
  container: HTMLElement,
  frame: CellGridFrame | null,
  painted: number | null,
): number | null {
  if (!frame || frame.cols === painted) return painted;
  container.style.setProperty("--cell-cols", String(frame.cols));
  return frame.cols;
}
